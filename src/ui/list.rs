//! The lifecycle every table-backed tab shares.
//!
//! Four tabs list rows in a [`Table`](super::table::Table) — Processes, Details,
//! Services and Startup apps — and each of them used to spell out the same nine
//! steps: pin the sort order while Ctrl is held, decide whether the display list is
//! stale, draw the header and treat a sort click as staleness, hoist the visible
//! column ids and a formatting buffer out of `self` so the row callback can borrow
//! them, draw the body, settle the selection from this frame's clicks, move it with
//! the arrow keys, re-find it after the list was rebuilt under it, and honour a
//! pending scroll.
//!
//! Written four times, those steps drifted. One tab passed its *row* index to
//! [`list_nav`](super::table::list_nav) and another its selection key; one rebuilt
//! the visible-column list every frame and another cached it; one re-validated the
//! selection after a rebuild and two never did. None of that is a decision — it is
//! four transcriptions of one design, and a fix to any of them reached one tab.
//!
//! # What is *not* here
//!
//! Only what all four genuinely share. The Processes tab's frozen row *ranking*, the
//! Details tab's column set, the polled re-enumeration behind Services and Startup:
//! those differ for real reasons and stay with their tabs. A shared type that
//! swallowed them would have half its fields unused by every user, which is worse
//! than the repetition it removed.
//!
//! # How the borrows work out
//!
//! [`ListTab::body`] hands the visible column ids and the formatting buffer to the row
//! closure as *arguments* rather than letting it capture them. That is the whole
//! reason this composes: `Table::body` holds the table mutably for the length of the
//! call, so a closure that captured `self.vis` could not coexist with it — which is
//! why every tab used to `std::mem::take` its buffers out of `self` and put them
//! back afterwards. Passing them in makes the dance unnecessary, and a tab now
//! destructures itself instead:
//!
//! ```ignore
//! let Self { list, entries, order, .. } = self;
//! let out = list.body(ui, t, order.len(), |rp, vis, buf| {
//!     let e = &entries[order[rp.row] as usize];
//!     // ...
//! });
//! ```

use super::table::{list_nav, Table, TableOutput};
use super::theme::Theme;
use egui::{Id, Ui};

/// The `generation` a tab passes when its rows do not come from the sampler.
///
/// Services and Startup apps enumerate their own rows on a timer, so there is no
/// snapshot generation to compare against; this makes that comparison a no-op
/// instead of asking them to invent a number.
pub const NO_GENERATION: u64 = u64::MAX;

/// The table, the selection, and the cache keys that say when to rebuild.
///
/// `K` is how the tab identifies what is selected — a
/// [`ProcKey`](crate::sys::snapshot::ProcKey) for the two process tables, a row index
/// for the two that enumerate their own rows. Keeping it generic is what lets the
/// selection survive a rebuild: [`Self::built`] re-finds the key in the new list
/// rather than trusting a row number that may now be a different row.
pub struct ListTab<K> {
    pub table: Table,

    /// What is selected, in the tab's own terms. Survives a rebuild.
    pub selected: Option<K>,
    /// Which display row that currently is. Derived from `selected` on every
    /// rebuild, and never the other way round.
    pub selected_row: Option<usize>,

    /// True while the sort order is pinned. Only the tabs that call
    /// [`Self::freeze_with_ctrl`] ever set it.
    pub frozen: bool,

    /// Ids of the visible columns, in display order. Reused between frames, and
    /// handed to the row closure rather than captured by it — see the module docs.
    vis: Vec<u16>,
    /// Formatting buffer for the row closure, reused between rows and frames so a
    /// cell costs no allocation. See [`super::fmt`].
    scratch: String,

    /// Scroll the selection into view once the list has been rebuilt around it.
    ///
    /// A flag and not a key: whatever asked for the reveal also selected it, so the
    /// rebuild's own hunt for `selected_row` is the lookup a key here would repeat.
    pending_reveal: bool,

    dirty: bool,
    built_generation: u64,
    built_filter: String,

    /// Names this table once, for two things that must agree: the egui id its
    /// widgets hang off, and the key its column layout is persisted under.
    key: &'static str,
}

impl<K: Copy + PartialEq> ListTab<K> {
    pub fn new(key: &'static str, table: Table) -> Self {
        Self {
            table,
            selected: None,
            selected_row: None,
            frozen: false,
            vis: Vec::new(),
            scratch: String::new(),
            pending_reveal: false,
            // `dirty` is what forces the first build; the generation only has to be
            // a value no snapshot will present before then.
            dirty: true,
            built_generation: 0,
            built_filter: String::new(),
            key,
        }
    }

    /// The egui id this table's widgets hang off, and the key its layout is saved
    /// under. One string, because they name the same table.
    pub fn id(&self) -> Id {
        Id::new(self.key)
    }

    pub fn key(&self) -> &'static str {
        self.key
    }

    /// Rebuild the display list on the next frame.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Drop the selection, for Escape.
    ///
    /// Both halves: `selected_row` is only re-derived from `selected` on a rebuild,
    /// so leaving it behind would keep one row painted as selected with nothing
    /// selected.
    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.selected_row = None;
    }

    /// Select something and scroll it into view once the list can say where it is.
    pub fn reveal(&mut self, key: K) {
        self.selected = Some(key);
        self.pending_reveal = true;
        self.dirty = true;
    }

    /// Pin the sort order while Ctrl is held, so a row that keeps moving can be
    /// clicked. Values keep updating; only the ordering is held.
    ///
    /// Returns true when the state changed, which is itself a reason to rebuild —
    /// the caller does not need to act on it, [`Self::stale`] already has.
    pub fn freeze_with_ctrl(&mut self, ui: &Ui) -> bool {
        let freeze = ui.input(|i| i.modifiers.ctrl);
        let changed = freeze != self.frozen;
        if changed {
            self.frozen = freeze;
            self.dirty = true;
        }
        changed
    }

    /// Whether the display list has to be rebuilt: something marked it dirty, the
    /// filter text changed, or a newer snapshot arrived.
    ///
    /// Pass [`NO_GENERATION`] for a tab that enumerates its own rows.
    pub fn stale(&self, generation: u64, filter: &str) -> bool {
        self.dirty || self.built_generation != generation || self.built_filter != filter
    }

    /// Record what the list was just built from, and re-find the selection in it.
    ///
    /// `key_of` maps a display row to the tab's identity for it, or `None` for a row
    /// that cannot be selected — a section header, a summary line. Call this
    /// immediately after rebuilding, and only then: it is what keeps `selected_row`
    /// honest, and a pending reveal is spent here because this is the first moment
    /// the row it wants has a number.
    pub fn built(
        &mut self,
        generation: u64,
        filter: &str,
        n_rows: usize,
        key_of: impl Fn(usize) -> Option<K>,
    ) {
        self.built_generation = generation;
        if self.built_filter != filter {
            self.built_filter.clear();
            self.built_filter.push_str(filter);
        }
        self.dirty = false;

        self.selected_row = self
            .selected
            .and_then(|want| (0..n_rows).find(|&r| key_of(r) == Some(want)));

        // Spent whether or not it resolved: a row that is not in this list is not
        // going to turn up in a later one either, and `selected_row` is then `None`,
        // which asks for no scroll.
        if std::mem::take(&mut self.pending_reveal) {
            self.table.scroll_to_row = self.selected_row;
        }
    }

    /// The header strip. A sort click or a column change marks the list stale.
    pub fn header(&mut self, ui: &mut Ui, t: &Theme) {
        self.header_with(ui, t, &[]);
    }

    /// The header strip with a live reading beside some of the titles.
    pub fn header_with(&mut self, ui: &mut Ui, t: &Theme, values: &[(u16, &str)]) {
        let id = self.id();
        if self.table.header_with(ui, t, id, values) {
            self.dirty = true;
        }
    }

    /// The rows.
    ///
    /// `paint` is handed the row painter, the visible column ids in display order,
    /// and a formatting buffer to reuse — as arguments rather than captures, which is
    /// what lets it coexist with the mutable borrow of the table. See the module
    /// documentation.
    pub fn body(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        n_rows: usize,
        mut paint: impl FnMut(&mut super::table::RowPainter<'_>, &[u16], &mut String),
    ) -> TableOutput {
        // Refreshed every frame into the same allocation: the header above may have
        // just toggled a column, and a stale list would paint cells into the wrong
        // slots.
        self.vis.clear();
        self.vis
            .extend(self.table.cols.iter().filter(|c| c.visible).map(|c| c.id));

        let id = Id::new(self.key);
        let Self {
            table,
            vis,
            scratch,
            selected_row,
            ..
        } = self;
        table.body(ui, t, id, n_rows, *selected_row, |rp| {
            paint(rp, vis, scratch)
        })
    }

    /// Settle the selection from this frame's clicks and the arrow keys.
    ///
    /// For a flat list. A tree brings its own navigation — the arrow keys have to
    /// expand and collapse — so the Processes tab calls
    /// [`select_from_clicks`](Self::select_from_clicks) and drives the rest itself.
    pub fn settle(
        &mut self,
        ui: &Ui,
        out: &TableOutput,
        n_rows: usize,
        key_of: impl Fn(usize) -> Option<K>,
    ) {
        self.select_from_clicks(out, &key_of);
        // Keyed off the row rather than the identity: the arrow keys move by
        // position, and position is what the list is ordered by.
        if let Some(r) = list_nav(ui, self.selected_row, n_rows) {
            self.select_and_scroll(r, &key_of);
        }
    }

    /// Move the selection to whatever was clicked this frame.
    ///
    /// A right-click selects too, so the menu that opens is about the row it opened
    /// on. `key_of` is the same mapping [`Self::built`] takes, and a row it answers
    /// `None` for — a section header, a summary line — *clears* the selection rather
    /// than leaving the previous one standing somewhere else on screen.
    pub fn select_from_clicks(
        &mut self,
        out: &TableOutput,
        key_of: impl Fn(usize) -> Option<K>,
    ) {
        if let Some(r) = out.clicked_row.or(out.context_row) {
            self.select(r, &key_of);
        }
    }

    /// Point the selection at one display row, or clear it if that row is not
    /// something that can be selected.
    ///
    /// Deliberately does *not* scroll: a click lands on a row the reader can already
    /// see, and scrolling under a click that was only meant to select would move the
    /// list out from under the pointer.
    pub fn select(&mut self, row: usize, key_of: impl Fn(usize) -> Option<K>) {
        match key_of(row) {
            Some(k) => {
                self.selected = Some(k);
                self.selected_row = Some(row);
            }
            None => self.clear_selection(),
        }
    }

    /// The same, and bring the row into view — for the keyboard, which can walk the
    /// selection off the end of what is on screen.
    pub fn select_and_scroll(&mut self, row: usize, key_of: impl Fn(usize) -> Option<K>) {
        self.select(row, key_of);
        // Only when it took: a section header clears the selection, and there is then
        // nothing to scroll to.
        if self.selected_row == Some(row) {
            self.table.scroll_to_row = Some(row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::table::{Align, Column};

    fn list() -> ListTab<u32> {
        ListTab::new("test", Table::new(vec![Column::new(0, "A", 80.0, Align::Left)], 0, false))
    }

    /// The point of keying the selection: the rows move, and it stays on the same
    /// thing rather than on the same row number.
    #[test]
    fn follows_its_selection_through_a_rebuild() {
        let mut l = list();
        // Rows are [10, 11, 12]; select the middle one.
        l.built(NO_GENERATION, "", 3, |r| Some(10 + r as u32));
        l.selected = Some(11);
        l.built(NO_GENERATION, "", 3, |r| Some(10 + r as u32));
        assert_eq!(l.selected_row, Some(1));

        // Now the list is [11, 12]: the same thing is row 0.
        l.built(NO_GENERATION, "", 2, |r| Some(11 + r as u32));
        assert_eq!(l.selected_row, Some(0));
        assert_eq!(l.selected, Some(11));

        // And when it is gone, nothing is selected -- not "still row 0".
        l.built(NO_GENERATION, "", 2, |r| Some(20 + r as u32));
        assert_eq!(l.selected_row, None);
        assert_eq!(l.selected, Some(11), "still wanted, just not here");
    }

    /// A reveal is spent on the first rebuild that can say where the row is, and
    /// asks for no scroll when the row never turned up.
    #[test]
    fn spends_a_reveal_on_the_next_rebuild() {
        let mut l = list();
        l.reveal(12);
        assert!(l.stale(NO_GENERATION, ""), "a reveal is a reason to rebuild");

        l.built(NO_GENERATION, "", 3, |r| Some(10 + r as u32));
        assert_eq!(l.table.scroll_to_row, Some(2));

        // Spent: a later rebuild does not scroll again.
        l.table.scroll_to_row = None;
        l.built(NO_GENERATION, "", 3, |r| Some(10 + r as u32));
        assert_eq!(l.table.scroll_to_row, None);

        // A reveal of something absent asks for no scroll.
        l.reveal(99);
        l.built(NO_GENERATION, "", 3, |r| Some(10 + r as u32));
        assert_eq!(l.table.scroll_to_row, None);
        assert_eq!(l.selected_row, None);
    }

    /// Staleness: the three reasons, and that `built` clears all of them.
    #[test]
    fn rebuilds_for_a_new_sample_new_filter_or_a_dirty_flag() {
        let mut l = list();
        assert!(l.stale(7, ""), "never built");
        l.built(7, "", 0, |_| None);
        assert!(!l.stale(7, ""));

        assert!(l.stale(8, ""), "a newer snapshot");
        assert!(l.stale(7, "chrome"), "a different filter");

        l.mark_dirty();
        assert!(l.stale(7, ""), "asked to");
        l.built(7, "", 0, |_| None);
        assert!(!l.stale(7, ""));
    }

    /// A tab with no snapshot behind it must not rebuild every frame just because
    /// it has no generation to offer.
    #[test]
    fn a_tab_that_enumerates_its_own_rows_settles() {
        let mut l = list();
        l.built(NO_GENERATION, "", 0, |_| None);
        assert!(!l.stale(NO_GENERATION, ""));
    }
}
