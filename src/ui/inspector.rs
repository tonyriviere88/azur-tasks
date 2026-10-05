//! The inspector: what the selected process is made of, in a panel under the list.
//!
//! Three tabs over one collector ([`crate::sys::inspect`]) -- modules, handles,
//! threads -- and only the visible one is ever asked for, because the handle table
//! costs more to read than the other two together.
//!
//! The panel is a *view of a request*, not a cache of a process. Every draw states
//! the question it wants answered -- this pid, this list -- and paints the newest
//! answer that matches it. An answer for the row you have just moved off is not
//! shown for a frame and then corrected; it is not shown at all.

use super::fmt;
use super::table::{Align, Column, RowPainter, Table};
use super::theme::Theme;
use super::Action;
use crate::sys::inspect::{Inspection, Inspector, Kind, Rows};
use azur_egui_theme as azur;
use azur_egui_theme::components::{
    menu_divider, ContextMenu, IconButton, MenuItem, Size, Tabs, Text, TextColor, TextVariant,
    Variant,
};
use azur_egui_theme::desktop;
use azur_egui_theme::tokens::space;
use egui::{Id, Layout, Ui};
use std::sync::Arc;

/// The tab strip's height: whatever a [`Tabs`] strip takes, asked rather than
/// assumed.
///
/// It was `row::DENSE` -- the dense row every other strip in this window is -- and a
/// tab strip is not a row: `Tabs` is a body line with `space-3` above and below it
/// and a rule under that, 13 points taller than a listing row. Reserving a row's
/// worth clipped the component and left its rule and its selected-tab indicator
/// painted over the table header below.
fn strip_h() -> f32 {
    azur_egui_theme::components::tabs_height()
}

/// The least the panel can be dragged to: the strip, a header and two rows.
///
/// Derived rather than guessed, because the strip is 13 points taller than a row
/// and a hand-picked 96 left it showing one.
pub fn min_h() -> f32 {
    strip_h() + super::table::HEADER_H + azur_egui_theme::tokens::row::DENSE * 2.0
}

/// The share of the view the panel takes the first time it is opened.
///
/// Only the first time. From then on the height is the reader's -- see
/// [`LIST_MIN_H`] for the one thing that overrules it.
pub const FIRST_SHARE: f32 = 0.5;

/// What the list above keeps, whatever the panel would rather be.
///
/// The panel gives way when the window is too short for both, and only then: a
/// dragged height is an instruction, and a panel that crept back to some
/// proportion of the window every time it was reopened would be arguing with it.
/// Below this the list has stopped being a list -- a header and four rows.
pub const LIST_MIN_H: f32 = 150.0;

// Module columns.
const M_NAME: u16 = 0;
const M_BASE: u16 = 1;
const M_SIZE: u16 = 2;
const M_PATH: u16 = 3;

// Handle columns.
const H_TYPE: u16 = 0;
const H_VALUE: u16 = 1;
const H_ACCESS: u16 = 2;
const H_NAME: u16 = 3;
const H_REFS: u16 = 4;

// Thread columns.
const T_TID: u16 = 0;
const T_START: u16 = 1;
const T_STATE: u16 = 2;
const T_PRIO: u16 = 3;
const T_CPU: u16 = 4;
const T_SWITCHES: u16 = 5;
const T_CREATED: u16 = 6;
const T_BASE_PRIO: u16 = 7;

/// The three tabs, in the order they are shown.
const TABS: [Kind; 3] = [Kind::Modules, Kind::Handles, Kind::Threads];

/// What the reader did to the strip this frame.
#[derive(Default)]
struct Strip {
    tab_changed: bool,
    closed: bool,
}

pub struct InspectorPanel {
    /// Index into [`TABS`].
    tab: usize,
    /// One table per tab, so column widths and the sort stay put per list.
    tables: [Table; 3],
    selected: [Option<usize>; 3],
    inspector: Inspector,
    /// The newest answer that matched what we were drawing. Kept so switching tabs
    /// or ticking the sampler does not blank the panel while the worker catches up.
    shown: Option<Arc<Inspection>>,
    /// Display order into `shown`'s rows, and what it was built from: the identity
    /// of the answer plus the sort. Rebuilt when either moves, not per frame -- a
    /// leaking process can have fifty thousand handles and the pointer moving over
    /// a row is a repaint.
    order: Vec<u32>,
    built: Option<(usize, u16, bool)>,
    /// What we last asked for, so a request goes out when the question changes
    /// rather than on every frame.
    asked: Option<(u32, Kind, u64)>,
}

impl InspectorPanel {
    pub fn new(ctx: &egui::Context) -> Self {
        let ctx = ctx.clone();
        let mut panel = Self {
            tab: 0,
            tables: [
                Table::new(
                    vec![
                        Column::new(M_NAME, "Name", 220.0, Align::Left).min(120.0),
                        Column::new(M_BASE, "Base", 130.0, Align::Right)
                            .help("Where the image is mapped in the process"),
                        Column::new(M_SIZE, "Size", 90.0, Align::Right),
                        Column::new(M_PATH, "Path", 520.0, Align::Left).stretch(),
                    ],
                    M_NAME,
                    false,
                ),
                Table::new(
                    vec![
                        Column::new(H_TYPE, "Type", 130.0, Align::Left).min(80.0),
                        Column::new(H_VALUE, "Handle", 90.0, Align::Right),
                        Column::new(H_ACCESS, "Access", 100.0, Align::Right)
                            .help("The rights this handle was opened with"),
                        Column::new(H_NAME, "Name", 620.0, Align::Left).stretch(),
                        Column::new(H_REFS, "Refs", 70.0, Align::Right).help(
                            "How many handles onto this object exist machine-wide: many rows at 1                              is a leak of objects, one row in the thousands is a leak of handles                              onto one",
                        ),
                    ],
                    H_TYPE,
                    false,
                ),
                Table::new(
                    vec![
                        Column::new(T_TID, "TID", 80.0, Align::Right),
                        Column::new(T_START, "Start address", 260.0, Align::Left)
                            .min(140.0)
                            .stretch()
                            .help("The module the thread's entry point is in, and the offset into it"),
                        Column::new(T_STATE, "State", 150.0, Align::Left)
                            .help("What the thread is doing, and what it is waiting on"),
                        Column::new(T_PRIO, "Priority", 80.0, Align::Right)
                            .help("Current priority, boosts included"),
                        Column::new(T_CPU, "CPU time", 100.0, Align::Right),
                        Column::new(T_SWITCHES, "Switches", 100.0, Align::Right)
                            .help("Context switches since the thread started"),
                        Column::new(T_CREATED, "Created", 120.0, Align::Right),
                        Column::new(T_BASE_PRIO, "Base priority", 110.0, Align::Right)
                            .help("What the thread's priority returns to once the scheduler stops boosting it"),
                    ],
                    T_CPU,
                    true,
                ),
            ],
            selected: [None; 3],
            inspector: Inspector::start(move || ctx.request_repaint()),
            shown: None,
            order: Vec::new(),
            built: None,
            asked: None,
        };
        // Two columns off by default, one click away in the header's context menu,
        // the same arrangement the Processes and Details tabs use for their own long
        // tail: a kernel address and a base priority are what you turn on when a
        // question has already been narrowed down to them.
        for (tab, hide) in [(1usize, H_REFS), (2, T_BASE_PRIO)] {
            for c in panel.tables[tab].cols.iter_mut() {
                if c.id == hide {
                    c.visible = false;
                }
            }
        }
        panel
    }

    /// The list currently on screen.
    fn kind(&self) -> Kind {
        TABS[self.tab.min(TABS.len() - 1)]
    }

    /// Draw the panel for `pid`, and say what the user asked to do with a row.
    ///
    /// Returns true when the reader closed it from its own title bar; the caller
    /// owns whether the panel is open, since the toolbar's latch and Alt+D reach the
    /// same flag.
    ///
    /// `generation` is the snapshot's, which is what makes the panel live: a new
    /// sample is a new reason to ask the same question again. The answer arrives on
    /// a later frame, which is what the worker's notify callback is for.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        pid: Option<u32>,
        name: Option<&str>,
        generation: u64,
        action: &mut Option<Action>,
    ) -> bool {
        // Claim the panel's height before anything is drawn in it.
        //
        // A panel is as tall as its *content*: `default_size` only sets the box the
        // content may lay out in, and the height egui keeps is what the content's
        // frame measured, floored at `min_size`. So a panel whose content merely
        // paints -- the note below, when no process is selected -- collapses to that
        // floor, ignores the 50% it was opened at, and springs back from every drag,
        // because the dragged size is stored and then overruled by the content again
        // on the next frame. egui says as much on `Panel::resizable`: a resizable
        // panel has to use the space it is given. Measured: 109 points of 392 asked
        // for.
        ui.set_min_height(ui.available_height());

        let kind = self.kind();
        let Strip {
            tab_changed,
            closed,
        } = self.strip(ui, t, pid, name);
        if tab_changed {
            // A tab change is a different question; the old answer is not it.
            self.shown = None;
            self.built = None;
        }
        let kind = if tab_changed { self.kind() } else { kind };

        let Some(pid) = pid else {
            self.shown = None;
            self.note(ui, t, "Select a process to see what it is made of");
            return closed;
        };

        // Ask, when the question has changed.
        if self.asked != Some((pid, kind, generation)) {
            self.asked = Some((pid, kind, generation));
            self.inspector.request(pid, kind);
        }

        // Take delivery of anything that answers it. An answer about another
        // process, or another list, is not shown even for one frame.
        if let Some(fresh) = self.inspector.latest() {
            if fresh.pid == pid && fresh.kind() == kind {
                let same = self.shown.as_ref().is_some_and(|s| Arc::ptr_eq(s, &fresh));
                if !same {
                    self.shown = Some(fresh);
                    self.built = None;
                }
            }
        }
        // A `shown` from before a tab or process change is stale by construction.
        if self
            .shown
            .as_ref()
            .is_none_or(|s| s.pid != pid || s.kind() != kind)
        {
            self.shown = None;
        }

        let Some(shown) = self.shown.clone() else {
            self.note(ui, t, "Reading…");
            return closed;
        };
        if shown.denied {
            self.note(ui, t, denial(kind));
            return closed;
        }

        self.rebuild_order(&shown);
        self.rows(ui, t, &shown, action);
        closed
    }

    /// The tab strip, and which process the panel is about.
    ///
    /// Everything on this line is aligned to the *tabs*, because the tabs are the
    /// line: `tabs_height` is what the strip reserves -- reserve less and the strip's
    /// own rule and indicator land on the table header -- and `tabs_label_band` is
    /// the band the labels sit in, which is what the close button is centred in and
    /// what the caption's baseline is taken from. Three controls each centred in the
    /// full strip instead would put the labels, the glyph and the caption on three
    /// different lines, none of them agreeing to within a point.
    fn strip(&mut self, ui: &mut Ui, t: &Theme, pid: Option<u32>, name: Option<&str>) -> Strip {
        let mut out = Strip::default();
        let full = ui.max_rect();
        let rect = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), strip_h()));
        ui.painter()
            .rect_filled(rect, desktop::structural_radius(), t.bg.layer_alt);
        // The labels' band: the strip without its rule, which is the line to align to.
        let band = azur::components::tabs_label_band(rect);

        // The count goes on the tab, which is half of what the panel is for: "1 482
        // handles" is the answer to a question about a leak on its own.
        let labels: Vec<String> = TABS
            .iter()
            .map(|k| match self.shown.as_ref().filter(|s| s.kind() == *k) {
                Some(s) => format!("{} ({})", k.label(), s.rows.len()),
                None => k.label().to_owned(),
            })
            .collect();

        // --- the right-hand end, reserved before the tabs are given the rest ------
        //
        // The close button is centred in the *label band* rather than in the strip: a
        // glyph centred in the band lands on the line `Tabs` puts its labels' ink on,
        // where a glyph centred in the whole strip would sit half a rule lower.
        //
        // A window's close control goes in the corner of the thing it closes. Subtle
        // and small: it sits in a strip of tabs, and the latch in the toolbar is the
        // control a reader reaches for on purpose -- this is the one they reach for
        // when they are done.
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(band.shrink2(egui::vec2(space::S2, 0.0)))
                .layout(Layout::right_to_left(egui::Align::Center)),
        );
        if right
            .add(
                IconButton::new(&azur::icons::close)
                    .variant(Variant::Subtle)
                    .size(Size::Small)
                    .tooltip("Close this panel (Alt+D)"),
            )
            .clicked()
        {
            out.closed = true;
        }

        // Which process this is about, painted on the tab labels' own baseline rather
        // than added as a widget: the caption is a smaller font than a tab label, so
        // two boxes centred in one band put their baselines a point apart, which is
        // exactly what this window is not allowed to do.
        //
        // It is dropped rather than squeezed when the tabs need the room -- their
        // natural width is `tabs_width`, measured with the function the component lays
        // itself out with, because `Tabs` claims the whole width it is given for its
        // rule and so cannot be asked afterwards where it stopped.
        let tabs_w =
            azur::components::tabs_width(ui.painter(), t, labels.iter().map(String::as_str), 0);
        let mut caption_left = right.min_rect().left();
        if let (Some(pid), Some(name)) = (pid, name) {
            let galley = ui.painter().layout_no_wrap(
                format!("{}  ({pid})", fmt::elide(name, 44)),
                t.fonts.caption.clone(),
                t.text.secondary,
            );
            let x = caption_left - space::S3 - galley.size().x;
            if x > band.left() + space::S3 + tabs_w + space::S4 {
                let baseline = azur::components::ink_baseline(
                    ui.painter(),
                    &t.fonts.body,
                    band.top(),
                    band.height(),
                );
                azur::components::galley_on_baseline(ui.painter(), x, baseline, galley);
                caption_left = x;
            }
        }

        // --- the tabs, in what is left ------------------------------------------
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(
                    egui::pos2(rect.left() + space::S3, rect.top()),
                    egui::pos2(
                        (caption_left - space::S3).max(rect.left() + space::S3),
                        rect.bottom(),
                    ),
                ))
                .layout(Layout::left_to_right(egui::Align::Min)),
        );
        let mut which = self.tab;
        if child
            .add(Tabs::new(&mut which).tabs(labels.iter().map(String::as_str)))
            .changed()
        {
            out.tab_changed = true;
        }
        if which != self.tab {
            self.tab = which;
            out.tab_changed = true;
        }

        // `Tabs` draws its rule across whatever width it was given, which is now short
        // of the strip. The rest of it is drawn here so the boundary between the strip
        // and the table header under it -- both `background-layer-alt` -- is unbroken.
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, t.stroke.subtle),
        );

        // Everything below the strip belongs to the table.
        ui.advance_cursor_after_rect(rect);
        out
    }

    /// One line of explanation, centred in the space the rows would have used.
    fn note(&self, ui: &mut Ui, t: &Theme, text: &str) {
        let rect = ui.available_rect_before_wrap();
        ui.painter()
            .rect_filled(rect, desktop::structural_radius(), t.bg.layer);
        let galley = note_layout(ui.painter(), t, text, rect.width());
        // `halign: Center` lays every row out around the x it is painted at, so the
        // anchor is the centre; the y is the block's top.
        ui.painter().galley(
            egui::pos2(
                rect.center().x,
                (rect.center().y - galley.size().y * 0.5).max(rect.top() + space::S3),
            ),
            galley,
            t.text.tertiary,
        );
    }

    /// Sort the rows for the active tab, when the answer or the sort has moved.
    fn rebuild_order(&mut self, shown: &Arc<Inspection>) {
        let table = &self.tables[self.tab];
        let stamp = (Arc::as_ptr(shown) as usize, table.sort_by, table.sort_desc);
        if self.built == Some(stamp) {
            return;
        }
        self.built = Some(stamp);

        let n = shown.rows.len();
        self.order.clear();
        self.order.extend(0..n as u32);
        let v = &mut self.order;
        match &shown.rows {
            Rows::Modules(m) => match table.sort_by {
                M_BASE => v.sort_unstable_by_key(|&i| m[i as usize].base),
                M_SIZE => v.sort_unstable_by_key(|&i| m[i as usize].size),
                M_PATH => v.sort_unstable_by_key(|&i| m[i as usize].path.to_ascii_lowercase()),
                _ => v.sort_unstable_by_key(|&i| m[i as usize].name.to_ascii_lowercase()),
            },
            Rows::Handles(h) => match table.sort_by {
                H_VALUE => v.sort_unstable_by_key(|&i| h[i as usize].value),
                H_ACCESS => v.sort_unstable_by_key(|&i| h[i as usize].access),
                H_REFS => v.sort_unstable_by_key(|&i| h[i as usize].refs),
                H_NAME => v.sort_unstable_by_key(|&i| {
                    h[i as usize]
                        .name
                        .as_ref()
                        .map(|n| n.to_ascii_lowercase())
                        .unwrap_or_default()
                }),
                // By type, then by handle value inside it: a hundred `Event` rows in
                // handle order tell a leak's story, in no order they tell none.
                _ => v.sort_unstable_by_key(|&i| {
                    (h[i as usize].kind.to_ascii_lowercase(), h[i as usize].value)
                }),
            },
            Rows::Threads(th) => match table.sort_by {
                T_TID => v.sort_unstable_by_key(|&i| th[i as usize].tid),
                T_START => {
                    v.sort_unstable_by_key(|&i| th[i as usize].start_label.to_ascii_lowercase())
                }
                T_STATE => v.sort_unstable_by_key(|&i| th[i as usize].state.to_ascii_lowercase()),
                T_PRIO => v.sort_unstable_by_key(|&i| th[i as usize].priority),
                T_SWITCHES => v.sort_unstable_by_key(|&i| th[i as usize].switches),
                T_CREATED => v.sort_unstable_by_key(|&i| th[i as usize].created),
                T_BASE_PRIO => v.sort_unstable_by_key(|&i| th[i as usize].base_priority),
                _ => v.sort_unstable_by_key(|&i| th[i as usize].cpu_100ns),
            },
        }
        if table.sort_desc {
            v.reverse();
        }
    }

    fn rows(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        shown: &Arc<Inspection>,
        action: &mut Option<Action>,
    ) {
        let tab = self.tab;
        let id = Id::new(("inspector", tab));
        if self.tables[tab].header(ui, t, id) {
            // A new sort, which `rebuild_order` picks up on the next pass; the rows
            // below are drawn in the order that was current when they were laid out.
            self.built = None;
        }

        let order = std::mem::take(&mut self.order);
        let vis: Vec<u16> = self.tables[tab].visible_cols().map(|(_, c)| c.id).collect();
        let selected = self.selected[tab];
        let rows = &shown.rows;
        let names_denied = shown.names_denied;

        let out = self.tables[tab].body(ui, t, id, order.len(), selected, |rp| {
            paint_row(rp, t, rows, order[rp.row] as usize, &vis, names_denied);
        });

        self.order = order;
        if let Some(r) = out.clicked_row.or(out.context_row) {
            self.selected[tab] = Some(r);
        }

        // The row's own menu: one field by name, the whole row, and for a module the
        // file it names.
        let target = self.selected[tab].and_then(|r| self.order.get(r).copied());
        let (label, copy, path) = match (target, &shown.rows) {
            (Some(i), Rows::Modules(m)) => {
                let m = &m[i as usize];
                (
                    "Copy path",
                    Some(m.path.to_string()),
                    (!m.path.is_empty()).then(|| m.path.to_string()),
                )
            }
            (Some(i), Rows::Handles(h)) => {
                let h = &h[i as usize];
                match &h.name {
                    Some(n) => ("Copy name", Some(n.to_string()), None),
                    // An unnamed object has nothing but its type to offer, and the
                    // entry says so rather than copying a blank.
                    None => ("Copy type", Some(h.kind.to_string()), None),
                }
            }
            (Some(i), Rows::Threads(th)) => {
                let th = &th[i as usize];
                ("Copy thread ID", Some(th.tid.to_string()), None)
            }
            (None, _) => ("Copy", None, None),
        };

        // Every column of the row, label and value, exactly as the hover tooltip
        // spells it out -- replayed through the same row painter, so a hidden column
        // is absent from both and a change to either lands in both.
        let all = self.selected[tab].filter(|_| copy.is_some()).map(|r| {
            let painter = ui.painter().with_clip_rect(egui::Rect::NOTHING);
            self.tables[tab]
                .read_row(&painter, t, r, &mut |rp| {
                    paint_row(rp, t, rows, self.order[rp.row] as usize, &vis, names_denied)
                })
                .into_iter()
                .map(|(title, value)| format!("{title}	{value}"))
                .collect::<Vec<_>>()
                .join(
                    "
",
                )
        });
        ContextMenu::new(&out.response).show(&ui.ctx().clone(), |ui| {
            if copy.is_none() {
                ui.add(
                    Text::new("No row selected")
                        .variant(TextVariant::Caption)
                        .color(TextColor::Tertiary),
                );
                return;
            }
            // The single field, named: "Copy" on its own is a question, and the
            // answer differs per tab.
            if let Some(text) = &copy {
                if ui.add(MenuItem::new(label)).clicked() {
                    *action = Some(Action::CopyRow(text.clone()));
                    ui.close();
                }
            }
            if let Some(text) = &all {
                if ui
                    .add(MenuItem::new("Copy all information"))
                    .on_hover_text("Every column of this row, as the tooltip shows it")
                    .clicked()
                {
                    *action = Some(Action::CopyRow(text.clone()));
                    ui.close();
                }
            }
            if let Some(path) = &path {
                menu_divider(ui);
                if ui.add(MenuItem::new("Open file location")).clicked() {
                    *action = Some(Action::OpenFileLocation { path: path.clone() });
                    ui.close();
                }
                if ui.add(MenuItem::new("Properties")).clicked() {
                    *action = Some(Action::OpenProperties { path: path.clone() });
                    ui.close();
                }
            }
        });
    }
}

/// Paint one row of whichever list is up.
///
/// A named function rather than a closure inside [`InspectorPanel::rows`] so that
/// the same description of a row serves the table, the hover tooltip and the
/// clipboard: [`Table::read_row`] replays it to collect what each column *would*
/// have said, and two readings of one row that could drift apart would be a bug
/// waiting for someone to notice a copied value disagreeing with the cell above it.
fn paint_row(
    rp: &mut RowPainter<'_>,
    t: &Theme,
    rows: &Rows,
    idx: usize,
    vis: &[u16],
    names_denied: bool,
) {
    let mut buf = String::new();
    for (slot, &cid) in vis.iter().enumerate() {
        match rows {
            Rows::Modules(m) => {
                let m = &m[idx];
                match cid {
                    M_NAME => rp.text(slot, &m.name, t.text.primary, Align::Left),
                    M_BASE => {
                        buf.clear();
                        buf.push_str(&format!("{:#018x}", m.base));
                        rp.text_font(slot, &buf, t.text.secondary, Align::Right, &t.fonts.mono);
                    }
                    M_SIZE => {
                        buf.clear();
                        fmt::bytes(&mut buf, m.size as u64);
                        rp.text(slot, &buf, t.text.secondary, Align::Right);
                    }
                    M_PATH => rp.text(slot, &m.path, t.text.tertiary, Align::Left),
                    _ => {}
                }
            }
            Rows::Handles(h) => {
                let h = &h[idx];
                match cid {
                    H_TYPE => rp.text(slot, &h.kind, t.text.primary, Align::Left),
                    H_VALUE => {
                        buf.clear();
                        buf.push_str(&format!("{:#x}", h.value));
                        rp.text_font(slot, &buf, t.text.secondary, Align::Right, &t.fonts.mono);
                    }
                    H_ACCESS => {
                        buf.clear();
                        buf.push_str(&format!("{:#010x}", h.access));
                        rp.text_font(slot, &buf, t.text.tertiary, Align::Right, &t.fonts.mono);
                    }
                    H_REFS => {
                        buf.clear();
                        fmt::integer(&mut buf, h.refs as u64);
                        rp.text(slot, &buf, t.text.tertiary, Align::Right);
                    }
                    H_NAME => match &h.name {
                        Some(n) => rp.text(slot, n, t.text.secondary, Align::Left),
                        // Two different silences, and a reader deserves to
                        // know which: an unnamed object, or a name we
                        // declined to go and get.
                        None if names_denied => rp.text(slot, "—", t.text.tertiary, Align::Left),
                        None => rp.text(slot, "", t.text.tertiary, Align::Left),
                    },
                    _ => {}
                }
            }
            Rows::Threads(th) => {
                let th = &th[idx];
                match cid {
                    T_TID => {
                        buf.clear();
                        fmt::integer(&mut buf, th.tid as u64);
                        rp.text(slot, &buf, t.text.primary, Align::Right);
                    }
                    T_START => rp.text_font(
                        slot,
                        &th.start_label,
                        t.text.secondary,
                        Align::Left,
                        &t.fonts.mono,
                    ),
                    T_STATE => rp.text(slot, &th.state, t.text.secondary, Align::Left),
                    T_PRIO => {
                        buf.clear();
                        fmt::integer(&mut buf, th.priority.max(0) as u64);
                        rp.text(slot, &buf, t.text.secondary, Align::Right);
                    }
                    T_CPU => {
                        buf.clear();
                        fmt::cpu_time(&mut buf, th.cpu_100ns.max(0) as u64);
                        rp.text(slot, &buf, t.text.secondary, Align::Right);
                    }
                    T_SWITCHES => {
                        buf.clear();
                        fmt::group_u64(&mut buf, th.switches as u64);
                        rp.text(slot, &buf, t.text.secondary, Align::Right);
                    }
                    T_BASE_PRIO => {
                        buf.clear();
                        fmt::integer(&mut buf, th.base_priority.max(0) as u64);
                        rp.text(slot, &buf, t.text.tertiary, Align::Right);
                    }
                    T_CREATED => {
                        buf.clear();
                        buf.push_str(&crate::sys::util::format_filetime(th.created, false));
                        rp.text(slot, &buf, t.text.tertiary, Align::Right);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Lay a note out to fit the panel: wrapped, and centred line by line.
///
/// These messages are sentences -- what a protected process will not answer and what
/// to do about it -- and a sentence painted as one unbroken line is a sentence with
/// its end cut off, which is what this used to do.
///
/// The measure is the tooltip's, and for the tooltip's reason: past about 400 points
/// the eye has to travel too far back to find the start of the next line. `space-6`
/// of air either side, and never wider than the panel however narrow that gets.
fn note_layout(
    painter: &egui::Painter,
    t: &Theme,
    text: &str,
    available: f32,
) -> Arc<egui::Galley> {
    // The floor is below the ceiling by construction -- both are constants -- so the
    // clamp cannot be inverted, and a panel narrower than the floor overflows by a
    // few points rather than laying out one character per line.
    let width = (available - space::S6 * 2.0).clamp(40.0, super::table::TOOLTIP_W);
    let mut job = egui::text::LayoutJob::simple(
        text.to_owned(),
        t.fonts.body.clone(),
        t.text.tertiary,
        width,
    );
    job.halign = egui::Align::Center;
    painter.layout_job(job)
}

/// Why a list came back empty, in the terms the reader can act on.
fn denial(kind: Kind) -> &'static str {
    match kind {
        // Both need the process opened, and the answer is the same one: run
        // elevated, or accept that a protected process does not answer.
        Kind::Modules => {
            "This process will not report its modules. Protected processes never do; \
             for the rest, running as administrator usually helps."
        }
        Kind::Threads => "This process has no threads left to list — it has exited.",
        Kind::Handles => {
            "This process will not report its handles. Protected processes never do; \
             for the rest, running as administrator usually helps."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the panel headlessly against this very process, one tab at a time.
    ///
    /// The collector has its own tests against the kernel; this is the other half --
    /// that a question asked by the panel comes back through the worker and is drawn
    /// as rows. It runs real egui passes, so it also catches the layout faults a
    /// paint-only path cannot be checked for by reading it: a table given a
    /// zero-height rect, a strip that eats the rows, a panic in a row callback.
    fn paints(kind: Kind, tab: usize) -> usize {
        let ctx = egui::Context::default();
        super::super::theme::install(&ctx);
        let t = Theme::dark();
        let mut panel = InspectorPanel::new(&ctx);
        panel.tab = tab;
        assert_eq!(panel.kind(), kind, "the tab index and the list disagree");

        let pid = std::process::id();
        let mut action = None;
        // The worker answers on its own thread, so the pass is repeated until the
        // answer lands. Ten tries at 50 ms is a long time for three system calls and
        // still bounded; the assertion below is what fails if it never arrives.
        for i in 0..10 {
            let _ = ctx.run_ui(Default::default(), |ui| {
                // A real rect, or the table would have nothing to lay out in.
                let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 300.0));
                let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                panel.ui(
                    &mut ui,
                    &t,
                    Some(pid),
                    Some("aztm-test.exe"),
                    i,
                    &mut action,
                );
            });
            if panel.shown.is_some() {
                // One more pass with the answer in hand: the first pass that has it
                // is the one that draws the rows.
                let _ = ctx.run_ui(Default::default(), |ui| {
                    let rect =
                        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 300.0));
                    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    panel.ui(
                        &mut ui,
                        &t,
                        Some(pid),
                        Some("aztm-test.exe"),
                        i,
                        &mut action,
                    );
                });
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let shown = panel
            .shown
            .as_ref()
            .unwrap_or_else(|| panic!("no {} answer arrived", kind.label()));
        assert_eq!(shown.pid, pid);
        assert_eq!(shown.kind(), kind);
        assert!(!shown.denied, "{} denied for our own process", kind.label());
        // The display order has to cover the rows exactly: a short one silently hides
        // rows, a long one indexes past the end and panics in the row callback.
        assert_eq!(
            panel.order.len(),
            shown.rows.len(),
            "{}: {} rows, {} in the display order",
            kind.label(),
            shown.rows.len(),
            panel.order.len()
        );
        shown.rows.len()
    }

    #[test]
    fn paints_the_modules_of_this_process() {
        assert!(paints(Kind::Modules, 0) > 2, "a process with no DLLs");
    }

    /// "Copy all information" is the tooltip's content, and both come out of
    /// [`paint_row`] through [`Table::read_row`]. What this checks is the replay: one
    /// line per *visible* column, each headed by that column's title, and a value on
    /// every one of them.
    #[test]
    fn copies_every_visible_column_of_a_row() {
        let ctx = egui::Context::default();
        super::super::theme::install(&ctx);
        let t = Theme::dark();
        let mut panel = InspectorPanel::new(&ctx);
        let pid = std::process::id();
        let mut action = None;

        // A module row, which is the tab whose columns all always have a value.
        for i in 0..10 {
            let _ = ctx.run_ui(Default::default(), |ui| {
                let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 300.0));
                let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                panel.ui(
                    &mut ui,
                    &t,
                    Some(pid),
                    Some("aztm-test.exe"),
                    i,
                    &mut action,
                );
            });
            if panel.shown.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let shown = panel.shown.clone().expect("no modules answer arrived");
        let rows = &shown.rows;
        let vis: Vec<u16> = panel.tables[0].visible_cols().map(|(_, c)| c.id).collect();
        let titles: Vec<&str> = panel.tables[0]
            .visible_cols()
            .map(|(_, c)| c.title)
            .collect();

        let mut read = Vec::new();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let painter = ui.painter().with_clip_rect(egui::Rect::NOTHING);
            read = panel.tables[0].read_row(&painter, &t, 0, &mut |rp| {
                paint_row(rp, &t, rows, panel.order[rp.row] as usize, &vis, false)
            });
        });

        assert_eq!(
            read.len(),
            titles.len(),
            "{} columns shown, {} read back: {read:?}",
            titles.len(),
            read.len()
        );
        for (title, (got, value)) in titles.iter().zip(&read) {
            assert_eq!(title, got);
            assert!(!value.is_empty(), "`{title}` came back blank");
        }
    }

    #[test]
    fn paints_the_handles_of_this_process() {
        assert!(paints(Kind::Handles, 1) > 0);
    }

    #[test]
    fn paints_the_threads_of_this_process() {
        assert!(paints(Kind::Threads, 2) > 0);
    }

    /// Every note the panel can show has to *fit* the panel: these are sentences, and
    /// the longest of them was painted as one unbroken line and cut off at the edge.
    #[test]
    fn a_note_wraps_inside_the_panel() {
        let ctx = egui::Context::default();
        super::super::theme::install(&ctx);
        let t = Theme::dark();
        let notes = [
            denial(Kind::Modules),
            denial(Kind::Handles),
            denial(Kind::Threads),
            "Select a process to see what it is made of",
            "Reading…",
        ];
        // A panel as wide as a maximised window, and one as narrow as it can be
        // dragged: neither may put a glyph outside the space it was given.
        for available in [1600.0_f32, 900.0, 420.0, 200.0] {
            for text in notes {
                let mut galley = None;
                let _ = ctx.run_ui(Default::default(), |ui| {
                    galley = Some(note_layout(ui.painter(), &t, text, available));
                });
                let galley = galley.unwrap();
                let widest = galley.size().x;
                assert!(
                    widest <= available,
                    "at {available} wide, `{text}` laid out {widest} wide"
                );
                // The long ones have to have actually broken, or "fits" only means
                // the panel happened to be wide enough this time.
                if text.len() > 80 && available >= 420.0 {
                    assert!(
                        galley.rows.len() > 1,
                        "at {available} wide, `{text}` stayed on one line"
                    );
                }
            }
        }
    }

    /// With nothing selected the panel says so rather than showing the last
    /// process's rows, which is the one thing a docked panel must never do.
    #[test]
    fn shows_nothing_when_no_process_is_selected() {
        let ctx = egui::Context::default();
        super::super::theme::install(&ctx);
        let t = Theme::dark();
        let mut panel = InspectorPanel::new(&ctx);
        let mut action = None;

        // A pass with a process, waited out until it has rows.
        let pid = std::process::id();
        for i in 0..10 {
            let _ = ctx.run_ui(Default::default(), |ui| {
                let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 300.0));
                let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                panel.ui(
                    &mut ui,
                    &t,
                    Some(pid),
                    Some("aztm-test.exe"),
                    i,
                    &mut action,
                );
            });
            if panel.shown.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            panel.shown.is_some(),
            "nothing to forget in the first place"
        );

        // Then one with none.
        let _ = ctx.run_ui(Default::default(), |ui| {
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 300.0));
            let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            panel.ui(&mut ui, &t, None, None, 99, &mut action);
        });
        assert!(
            panel.shown.is_none(),
            "the panel kept a dead selection's rows"
        );
    }
}
