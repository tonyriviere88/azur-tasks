//! The Processes tab: a grouped, sortable process hierarchy.
//!
//! The display list is cached and only rebuilt when something that affects it
//! actually changes (new sample, sort, filter, expand/collapse). Scrolling and
//! hovering therefore cost nothing beyond painting the visible rows.

use super::fmt;
use super::icons::IconCache;
use super::table::{Align, Column, RowPainter, Table};
use super::theme::Theme;
use crate::sys::meta::MetaStore;
use crate::sys::snapshot::{Category, Proc, ProcKey, RunState, Snapshot};
use egui::{Id, Rect, Ui, Vec2};
use rustc_hash::FxHashSet;
use std::sync::Arc;

pub const C_NAME: u16 = 0;
pub const C_STATUS: u16 = 1;
pub const C_CPU: u16 = 2;
pub const C_MEM: u16 = 3;
pub const C_DISK: u16 = 4;
pub const C_PID: u16 = 5;
pub const C_TYPE: u16 = 6;
pub const C_USER: u16 = 7;
pub const C_PUBLISHER: u16 = 8;

/// Keyboard navigation intents.
#[derive(Clone, Copy)]
enum Nav {
    Up,
    Down,
    /// Expand a collapsed parent, else step to the next row.
    Right,
    /// Collapse an expanded parent, else step out to its parent.
    Left,
    First,
    Last,
}

#[derive(Clone, Copy)]
enum RowKind {
    Group(Category),
    /// Index into `Snapshot::procs`.
    Proc(u32),
}

#[derive(Clone, Copy)]
struct Row {
    kind: RowKind,
    depth: u16,
    /// Whether this process has children (so we draw an expander).
    has_children: bool,
    expanded: bool,
}

pub struct ProcessesTab {
    pub table: Table,
    rows: Vec<Row>,
    /// Processes the user has expanded. Everything starts collapsed, like Task
    /// Manager, so the list opens as a short summary rather than 600 rows.
    expanded: FxHashSet<ProcKey>,
    collapsed_groups: FxHashSet<Category>,
    pub selected: Option<ProcKey>,
    /// Row index of the selection in the current list, for row highlighting.
    selected_row: Option<usize>,
    /// Grouping used by the list currently built, so a sort change invalidates
    /// the cache even when nothing else did.
    built_grouped: bool,

    /// Display position of each process from the last unfrozen rebuild.
    ///
    /// Keyed by [`ProcKey`] rather than row index on purpose: indices into
    /// `Snapshot::procs` shift as processes come and go, so an index-based
    /// freeze would silently start showing the wrong process.
    rank: rustc_hash::FxHashMap<ProcKey, u32>,
    /// True while the sort order is held still (Ctrl held down).
    frozen: bool,

    // Cache invalidation.
    built_generation: u64,
    built_filter: String,
    dirty: bool,

    // Scratch buffers, reused every frame.
    scratch: String,
    sort_buf: Vec<u32>,
    stack: Vec<(u32, u16)>,
    matched: Vec<bool>,
}

impl ProcessesTab {
    pub fn new() -> Self {
        let cols = vec![
            Column::new(C_NAME, "Name", 300.0, Align::Left)
                .min(160.0)
                .stretch(),
            Column::new(C_STATUS, "Status", 92.0, Align::Left),
            Column::new(C_CPU, "CPU", 74.0, Align::Right).help("Share of total CPU capacity"),
            Column::new(C_MEM, "Memory", 100.0, Align::Right).help("Private working set"),
            Column::new(C_DISK, "Disk", 92.0, Align::Right).help("Total I/O transfer rate"),
            Column::new(C_PID, "PID", 72.0, Align::Right),
            Column::new(C_TYPE, "Type", 96.0, Align::Left),
            Column::new(C_USER, "User name", 150.0, Align::Left),
            Column::new(C_PUBLISHER, "Publisher", 170.0, Align::Left),
        ];
        // Sorted by name, ascending, which is also what turns grouping on.
        let mut table = Table::new(cols, C_NAME, false);
        table.row_h = 26.0;
        Self {
            table,
            rows: Vec::new(),
            expanded: FxHashSet::default(),
            collapsed_groups: FxHashSet::default(),
            selected: None,
            selected_row: None,
            built_grouped: true,
            rank: rustc_hash::FxHashMap::default(),
            frozen: false,
            built_generation: u64::MAX,
            built_filter: String::new(),
            dirty: true,
            scratch: String::new(),
            sort_buf: Vec::new(),
            stack: Vec::new(),
            matched: Vec::new(),
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Whether the list is split into Apps / Background processes.
    ///
    /// Tied to the sort rather than to a button. Group headers are only useful
    /// while the order inside them is meaningful: sorted by name they read as
    /// two alphabetical sections, but sorted by CPU they cut the ranking in two
    /// and hide the busiest background process below a fold. So: grouped by name,
    /// flat by anything else.
    pub fn grouped(&self) -> bool {
        self.table.sort_by == C_NAME
    }

    /// Processes that would be terminated by "End process tree" on the
    /// selection: the node plus every descendant, deepest first.
    pub fn subtree_of(&self, snap: &Snapshot, key: ProcKey) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        let Some(root) = snap.find_by_key(key) else {
            return out;
        };
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            let p = &snap.procs[i];
            out.push((p.pid, p.name.to_string()));
            for &c in snap.children_of(i) {
                stack.push(c as usize);
            }
        }
        // Children before parents, so a parent cannot respawn its children
        // between the two kills.
        out.reverse();
        out
    }

    // -----------------------------------------------------------------
    // Display list
    // -----------------------------------------------------------------

    fn rebuild(&mut self, snap: &Snapshot, meta: &MetaStore, filter: &str) {
        self.rows.clear();
        let n = snap.procs.len();
        if n == 0 {
            self.rank.clear();
            return;
        }

        // A node is kept when it matches, or when any descendant matches -- so
        // filtering narrows the tree without orphaning the matches.
        self.matched.clear();
        self.matched.resize(n, true);
        if !filter.is_empty() {
            let needle = filter.to_ascii_lowercase();
            for (i, p) in snap.procs.iter().enumerate() {
                self.matched[i] = row_matches(p, meta, &needle);
            }
            // Propagate matches upward. `procs` is not topologically ordered, so
            // walk each match's ancestor chain instead of relying on order.
            let flags = self.matched.clone();
            for (i, _) in flags.iter().enumerate().filter(|(_, hit)| **hit) {
                let mut parent = snap.procs[i].parent;
                let mut guard = 0;
                while parent != u32::MAX && guard <= n {
                    self.matched[parent as usize] = true;
                    parent = snap.procs[parent as usize].parent;
                    guard += 1;
                }
            }
        }

        let sort_by = self.table.sort_by;
        let desc = self.table.sort_desc;

        if self.grouped() {
            for cat in [Category::App, Category::Background] {
                // Only emit a group that has visible content.
                let any = snap
                    .procs
                    .iter()
                    .enumerate()
                    .any(|(i, p)| p.group == cat && p.group_head && self.matched[i]);
                if !any {
                    continue;
                }
                let collapsed = self.collapsed_groups.contains(&cat);
                self.rows.push(Row {
                    kind: RowKind::Group(cat),
                    depth: 0,
                    has_children: true,
                    expanded: !collapsed,
                });
                if collapsed {
                    continue;
                }
                self.emit_roots(snap, meta, Some(cat), sort_by, desc, 1);
            }
        } else {
            self.emit_roots(snap, meta, None, sort_by, desc, 0);
        }

        // Record the order so a later freeze has something to hold on to.
        // Skipped while frozen, or the frozen order would drift as new
        // processes are appended.
        if !self.frozen {
            self.rank.clear();
            self.rank.reserve(self.rows.len());
            for (pos, row) in self.rows.iter().enumerate() {
                if let RowKind::Proc(i) = row.kind {
                    self.rank.insert(snap.procs[i as usize].key(), pos as u32);
                }
            }
        }

        // Re-locate the selection in the new list.
        self.selected_row = self.selected.and_then(|key| {
            self.rows.iter().position(|r| match r.kind {
                RowKind::Proc(i) => snap.procs[i as usize].key() == key,
                _ => false,
            })
        });
    }

    fn emit_roots(
        &mut self,
        snap: &Snapshot,
        meta: &MetaStore,
        cat: Option<Category>,
        sort_by: u16,
        desc: bool,
        base_depth: u16,
    ) {
        self.sort_buf.clear();
        match cat {
            None => {
                for &r in &snap.roots {
                    if self.matched[r as usize] {
                        self.sort_buf.push(r);
                    }
                }
            }
            Some(c) => {
                // `group`/`group_head` come from the sampler, which assigns a
                // whole subtree to one group. Selecting on `category` here
                // instead would both hide an app launched by a shell and make a
                // browser look childless (its renderers are background
                // processes, but they belong under the browser).
                for (i, p) in snap.procs.iter().enumerate() {
                    if p.group == c && p.group_head && self.matched[i] {
                        self.sort_buf.push(i as u32);
                    }
                }
            }
        }
        // Taken out so the frozen rank can be read while `sort_buf` is borrowed
        // mutably; both are fields of `self`.
        let rank = std::mem::take(&mut self.rank);
        let frozen = self.frozen.then_some(&rank);
        sort_indices(&mut self.sort_buf, snap, meta, sort_by, desc, frozen);

        // Depth-first, pushing children in reverse so they pop in sorted order.
        self.stack.clear();
        for &r in self.sort_buf.iter().rev() {
            self.stack.push((r, base_depth));
        }

        // A child nests here unless it heads a group of its own (an app launched
        // by another app gets its own top-level entry, as in Task Manager), so
        // every process is listed exactly once. Kept identical to the expansion
        // filter below so an expander never opens onto an empty subtree.
        let visible_child = |snap: &Snapshot, matched: &[bool], c: u32| -> bool {
            matched[c as usize] && (cat.is_none() || !snap.procs[c as usize].group_head)
        };

        let mut children: Vec<u32> = Vec::new();
        while let Some((idx, depth)) = self.stack.pop() {
            let p = &snap.procs[idx as usize];
            let key = p.key();
            let has_children = p.has_children()
                && snap
                    .children_of(idx as usize)
                    .iter()
                    .any(|&c| visible_child(snap, &self.matched, c));
            let expanded = has_children && self.expanded.contains(&key);

            self.rows.push(Row {
                kind: RowKind::Proc(idx),
                depth,
                has_children,
                expanded,
            });

            if expanded {
                children.clear();
                children.extend(
                    snap.children_of(idx as usize)
                        .iter()
                        .copied()
                        .filter(|&c| visible_child(snap, &self.matched, c)),
                );
                sort_indices(&mut children, snap, meta, sort_by, desc, frozen);
                for &c in children.iter().rev() {
                    self.stack.push((c, depth + 1));
                }
            }
        }

        self.rank = rank;
    }

    // -----------------------------------------------------------------
    // Painting
    // -----------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        snap: &Snapshot,
        meta: &MetaStore,
        icons: &mut IconCache,
        filter: &str,
        action: &mut Option<super::Action>,
    ) {
        // Holding Ctrl pins the row order so a moving row can actually be
        // clicked. Values keep updating; only the ordering is held.
        let freeze = ui.input(|i| i.modifiers.ctrl);
        if freeze != self.frozen {
            self.frozen = freeze;
            self.dirty = true;
        }

        if self.dirty
            || self.built_generation != snap.generation
            || self.built_filter != filter
            || self.built_grouped != self.grouped()
        {
            self.rebuild(snap, meta, filter);
            self.built_generation = snap.generation;
            self.built_filter = filter.to_owned();
            self.built_grouped = self.grouped();
            self.dirty = false;
        }

        let id = Id::new("processes");
        if self.table.header(ui, t, id) {
            self.dirty = true;
        }

        let slots = Slots::build(&self.table);
        let ctx = ui.ctx().clone();
        let rows = std::mem::take(&mut self.rows);
        let mut toggle: Option<usize> = None;
        let mut buf = std::mem::take(&mut self.scratch);
        // Recorded while painting the hovered row. Deriving the expander's hit
        // area afterwards is not possible: the row's cell rectangles live in
        // screen space inside the scroll area, which this scope cannot see.
        let mut expander_hit: Option<egui::Rect> = None;

        let out = self.table.body(
            ui,
            t,
            id,
            rows.len(),
            self.selected_row,
            |rp| {
                let row = rows[rp.row];
                let hit = match row.kind {
                    RowKind::Group(cat) => paint_group_row(
                        rp,
                        t,
                        cat,
                        row.expanded,
                        snap,
                        &self.matched,
                        &slots,
                        &mut buf,
                    ),
                    RowKind::Proc(idx) => {
                        let p = &snap.procs[idx as usize];
                        paint_proc_row(rp, t, p, &row, meta, icons, &ctx, &slots, &mut buf)
                    }
                };
                if rp.hovered {
                    expander_hit = hit;
                }
            },
        );

        self.scratch = buf;

        // --- interaction --------------------------------------------------
        if let Some(r) = out.clicked_row.or(out.context_row) {
            if let Some(row) = rows.get(r) {
                match row.kind {
                    RowKind::Proc(idx) => {
                        self.selected = Some(snap.procs[idx as usize].key());
                        self.selected_row = Some(r);
                    }
                    RowKind::Group(_) => {
                        self.selected = None;
                        self.selected_row = None;
                    }
                }
            }
        }

        // A single click toggles when it lands on the disclosure triangle;
        // clicking a group header anywhere toggles it, as Task Manager does.
        if let Some(r) = out.clicked_row {
            if let Some(row) = rows.get(r) {
                let on_expander = expander_hit
                    .zip(ctx.pointer_latest_pos())
                    // Widen the triangle's 16px box a little: it is a small
                    // target, and being one pixel off should not feel broken.
                    .is_some_and(|(rect, pos)| rect.expand(3.0).contains(pos));
                let is_group = matches!(row.kind, RowKind::Group(_));
                if (row.has_children && on_expander) || is_group {
                    toggle = Some(r);
                }
            }
        }
        // Double-clicking anywhere on a parent row toggles it too.
        if let Some(r) = out.double_clicked_row {
            if rows.get(r).is_some_and(|row| row.has_children) {
                toggle = Some(r);
            }
        }

        // --- keyboard navigation ------------------------------------------
        // Skipped while a text field holds focus, so typing in the filter box
        // does not also drive the selection.
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && !rows.is_empty() {
            let cur = self.selected_row;
            let mut nav = None;
            ui.input(|i| {
                for (key, action) in [
                    (egui::Key::ArrowDown, Nav::Down),
                    (egui::Key::ArrowUp, Nav::Up),
                    (egui::Key::ArrowRight, Nav::Right),
                    (egui::Key::ArrowLeft, Nav::Left),
                    (egui::Key::Home, Nav::First),
                    (egui::Key::End, Nav::Last),
                ] {
                    if i.key_pressed(key) {
                        nav = Some(action);
                        break;
                    }
                }
            });

            if let Some(nav) = nav {
                let mut target = match (nav, cur) {
                    (Nav::First, _) => Some(0),
                    (Nav::Last, _) => Some(rows.len() - 1),
                    (Nav::Down, None) | (Nav::Right, None) => Some(0),
                    (Nav::Up, None) | (Nav::Left, None) => Some(rows.len() - 1),
                    (Nav::Down, Some(r)) => Some((r + 1).min(rows.len() - 1)),
                    (Nav::Up, Some(r)) => Some(r.saturating_sub(1)),
                    (Nav::Right, Some(r)) => {
                        let row = rows[r];
                        if row.has_children && !row.expanded {
                            toggle = Some(r);
                            None
                        } else {
                            // Already open (or a leaf): step into it.
                            Some((r + 1).min(rows.len() - 1))
                        }
                    }
                    (Nav::Left, Some(r)) => {
                        let row = rows[r];
                        if row.has_children && row.expanded {
                            toggle = Some(r);
                            None
                        } else {
                            // Jump to the enclosing parent, which is the nearest
                            // row above with a smaller depth.
                            rows[..r]
                                .iter()
                                .rposition(|x| x.depth < row.depth)
                                .or(Some(r))
                        }
                    }
                };

                if let Some(t) = target.take() {
                    self.selected_row = Some(t);
                    self.selected = match rows[t].kind {
                        RowKind::Proc(i) => Some(snap.procs[i as usize].key()),
                        RowKind::Group(_) => None,
                    };
                    self.table.scroll_to_row = Some(t);
                }
            }
        }

        if let Some(r) = toggle {
            match rows[r].kind {
                RowKind::Group(cat) => {
                    if !self.collapsed_groups.remove(&cat) {
                        self.collapsed_groups.insert(cat);
                    }
                }
                RowKind::Proc(idx) => {
                    let key = snap.procs[idx as usize].key();
                    if !self.expanded.remove(&key) {
                        self.expanded.insert(key);
                    }
                }
            }
            self.dirty = true;
        }

        // --- context menu -------------------------------------------------
        if let Some(r) = out.context_row {
            if let Some(RowKind::Proc(idx)) = rows.get(r).map(|x| x.kind) {
                let p = &snap.procs[idx as usize];
                let key = p.key();
                let name = p.name.clone();
                let pid = p.pid;
                let path = meta.get(key).and_then(|m| m.path.clone());
                out.response.context_menu(|ui| {
                    super::process_context_menu(ui, t, pid, &name, key, path.as_deref(), action);
                });
            } else {
                // Keep egui's menu bookkeeping consistent on group rows.
                out.response.context_menu(|ui| {
                    ui.label("No process selected");
                });
            }
        } else {
            out.response.context_menu(|ui| {
                if let Some(key) = self.selected {
                    if let Some(i) = snap.find_by_key(key) {
                        let p = &snap.procs[i];
                        let path = meta.get(key).and_then(|m| m.path.clone());
                        super::process_context_menu(
                            ui,
                            t,
                            p.pid,
                            &p.name.clone(),
                            key,
                            path.as_deref(),
                            action,
                        );
                        return;
                    }
                }
                ui.label("No process selected");
            });
        }

        self.rows = rows;
    }
}

fn row_indent(depth: u16) -> f32 {
    8.0 + depth as f32 * 17.0
}

/// Column id -> position among the *visible* columns.
///
/// `RowPainter` addresses cells by position, which stops matching the `C_*`
/// constants as soon as a column is hidden. Every cell here goes through this,
/// so hiding one shifts nothing.
struct Slots([Option<usize>; 9]);

impl Slots {
    fn build(table: &Table) -> Self {
        let mut s = [None; 9];
        for (slot, (_, c)) in table.visible_cols().enumerate() {
            if let Some(e) = s.get_mut(c.id as usize) {
                *e = Some(slot);
            }
        }
        Self(s)
    }

    #[inline]
    fn of(&self, id: u16) -> Option<usize> {
        self.0.get(id as usize).copied().flatten()
    }
}

/// Returns the disclosure triangle's hit rect.
#[allow(clippy::too_many_arguments)]
fn paint_group_row(
    rp: &mut RowPainter<'_>,
    t: &Theme,
    cat: Category,
    expanded: bool,
    snap: &Snapshot,
    matched: &[bool],
    slots: &Slots,
    buf: &mut String,
) -> Option<Rect> {
    // Group rows get their own surface so the hierarchy reads at a glance.
    rp.painter
        .rect_filled(rp.rect, egui::CornerRadius::ZERO, t.surface);
    rp.painter.hline(
        rp.rect.x_range(),
        rp.rect.bottom() - 0.5,
        egui::Stroke::new(1.0, t.border_soft),
    );

    let name_slot = slots.of(C_NAME);
    let hit = name_slot.map(|s| rp.expander(s, 6.0, expanded, t.text_dim));

    // Aggregate everything displayed under this group, including nested rows,
    // so the header total matches what expanding it would reveal.
    let mut count = 0u32;
    let mut cpu = 0.0f32;
    let mut mem = 0u64;
    let mut disk = 0.0f32;
    for (i, p) in snap.procs.iter().enumerate() {
        if p.group == cat && matched[i] {
            count += 1;
            cpu += p.cpu;
            mem += p.private_ws;
            disk += p.disk_bps;
        }
    }

    buf.clear();
    use std::fmt::Write;
    let _ = write!(buf, "{} ({count})", cat.label());
    if let Some(s) = name_slot {
        rp.text_font(s, buf, t.text, Align::Left, &t.fonts.medium);
    }

    let mut b = String::new();
    if let Some(s) = slots.of(C_CPU) {
        fmt::percent(&mut b, cpu);
        rp.text_font(s, &b, t.text_dim, Align::Right, &t.fonts.medium);
    }
    if let Some(s) = slots.of(C_MEM) {
        fmt::mem_mb(&mut b, mem);
        rp.text_font(s, &b, t.text_dim, Align::Right, &t.fonts.medium);
    }
    if let Some(s) = slots.of(C_DISK) {
        fmt::rate_mbs(&mut b, disk);
        rp.text_font(s, &b, t.text_dim, Align::Right, &t.fonts.medium);
    }

    hit
}

/// Returns the disclosure triangle's hit rect, when this row has one.
#[allow(clippy::too_many_arguments)]
fn paint_proc_row(
    rp: &mut RowPainter<'_>,
    t: &Theme,
    p: &Proc,
    row: &Row,
    meta: &MetaStore,
    icons: &mut IconCache,
    ctx: &egui::Context,
    slots: &Slots,
    buf: &mut String,
) -> Option<Rect> {
    let m = meta.get(p.key());
    let indent = row_indent(row.depth);

    // A collapsed parent reports its whole subtree, so folding a browser does
    // not make its 40 renderers' load disappear from the list.
    let folded = row.has_children && !row.expanded;
    let cpu = if folded { p.sub_cpu } else { p.cpu };
    let mem = if folded { p.sub_private_ws } else { p.private_ws };
    let disk = if folded { p.sub_disk_bps } else { p.disk_bps };

    // --- Name -------------------------------------------------------------
    let name_slot = slots.of(C_NAME);
    let mut x = indent;
    let hit = match (name_slot, row.has_children) {
        (Some(s), true) => Some(rp.expander(s, x, row.expanded, t.text_dim)),
        _ => None,
    };
    x += 18.0;

    if let Some(s) = name_slot {
        let tex = icons.get(ctx, m.as_ref().and_then(|m| m.path.as_ref()));
        rp.icon(s, x, tex);
    }
    x += 21.0;

    // Prefer the human-facing name: window title for an app, then the version
    // resource's description, then the image name.
    //
    // Window titles are written document-first ("main.rs - myproject - Visual
    // Studio Code"), which is the wrong way round for a list: the column is
    // sorted and scanned by application, so the application has to be the part
    // that is always visible and never truncated. `app_first` flips it.
    let mut reordered = String::new();
    let desc = m.as_ref().and_then(|m| m.description.as_deref());
    let label: &str = match (&p.window_title, desc) {
        (Some(title), _) if p.category == Category::App => {
            app_first(title, desc, &p.name, &mut reordered)
        }
        (_, Some(d)) => d,
        _ => &p.name,
    };
    let name_color = if p.state == RunState::Suspended {
        t.text_dim
    } else {
        t.text
    };
    if let Some(s) = name_slot {
        rp.text_indent(s, x, label, name_color);
    }

    // --- Status -----------------------------------------------------------
    if let (Some(s), RunState::Suspended) = (slots.of(C_STATUS), p.state) {
        rp.pill(s, "Suspended", t.warn, t.accent_soft.gamma_multiply(0.8));
    }

    // --- CPU / Memory / Disk ---------------------------------------------
    if let Some(s) = slots.of(C_CPU) {
        rp.heat(s, cpu / 25.0);
        fmt::percent(buf, cpu);
        rp.text(s, buf, t.load_text(cpu), Align::Right);
    }

    // Tint memory relative to a 2 GB reference; above that it is notable.
    if let Some(s) = slots.of(C_MEM) {
        rp.heat(s, mem as f32 / (2.0 * 1024.0 * 1024.0 * 1024.0));
        fmt::mem_mb(buf, mem);
        rp.text(s, buf, t.text, Align::Right);
    }

    // 20 MB/s of I/O is a busy process.
    if let Some(s) = slots.of(C_DISK) {
        rp.heat(s, disk / (20.0 * 1024.0 * 1024.0));
        fmt::rate_mbs(buf, disk);
        let disk_color = if disk < 0.05 { t.text_faint } else { t.text };
        rp.text(s, buf, disk_color, Align::Right);
    }

    // --- PID / Type / User / Publisher -----------------------------------
    if let Some(s) = slots.of(C_PID) {
        fmt::integer(buf, p.pid as u64);
        rp.text_font(s, buf, t.text_dim, Align::Right, &t.fonts.mono);
    }

    if let Some(s) = slots.of(C_TYPE) {
        rp.text(s, p.category.type_label(), t.text_faint, Align::Left);
    }

    if let Some(s) = slots.of(C_USER) {
        let user = m.as_ref().and_then(|m| m.user.clone());
        rp.text(s, user.as_deref().unwrap_or("—"), t.text_dim, Align::Left);
    }

    if let Some(s) = slots.of(C_PUBLISHER) {
        let publisher = m.as_ref().and_then(|m| m.company.clone());
        rp.text(
            s,
            publisher.as_deref().unwrap_or("—"),
            t.text_faint,
            Align::Left,
        );
    }

    // Elevation marker on the right edge of the name cell.
    if let (Some(s), true) = (name_slot, m.as_ref().is_some_and(|m| m.elevated)) {
        let cell = rp.cell(s);
        let r = Rect::from_center_size(
            egui::Pos2::new(cell.right() - 12.0, cell.center().y),
            Vec2::splat(11.0),
        );
        super::icons::glyph(
            &rp.painter.with_clip_rect(cell),
            r,
            super::icons::Glyph::Shield,
            t.warn.gamma_multiply(0.85),
            t,
        );
    }

    hit
}

/// Separators apps put between the document and their own name. There is no
/// convention: VS Code uses a hyphen, Firefox an em dash, Teams a pipe.
const TITLE_SEPARATORS: [&str; 7] = [" - ", " – ", " — ", " | ", " • ", " · ", " : "];

/// Move the application name to the front of a window title.
///
/// Windows apps title their windows document-first and put themselves last
/// (`"main.rs - myproject - Visual Studio Code"`). In a column that is sorted and
/// elided that is backwards: the part you scan by is the part that gets cut.
///
/// Splitting on a separator is not enough on its own. The separator is not
/// knowable -- VS Code uses a hyphen, Firefox an em dash, Teams a pipe -- and a
/// title that merely *contains* one need not be document-first at all. So the
/// trailing segment has to be corroborated as the application before anything
/// moves, against either the version resource's `FileDescription` or the
/// executable's own file name. Failing both, the title is left alone: showing it
/// as the app wrote it is the safe answer.
///
/// The name rendered is the one from the *title*, not from the version resource.
/// It is available immediately -- the version resource is resolved on a worker
/// thread and can be a second or two behind on a busy machine -- so the label
/// never changes under the reader. It also means File Pilot keeps the version it
/// puts in its own title bar.
///
/// Writes into `buf` and returns a borrow of it, so titles left alone cost
/// nothing.
fn app_first<'a>(title: &'a str, desc: Option<&str>, exe: &str, buf: &'a mut String) -> &'a str {
    // Rightmost separator: titles nest left to right, so the app name is last.
    let Some((head, tail)) = TITLE_SEPARATORS
        .iter()
        .filter_map(|sep| title.rfind(sep).map(|at| (at, sep.len())))
        .max_by_key(|(at, _)| *at)
        .map(|(at, len)| (title[..at].trim(), title[at + len..].trim()))
    else {
        return title;
    };

    if head.is_empty() || tail.is_empty() {
        return title;
    }
    // An app that already titles itself first is left as it is.
    if names_app(head, desc, exe) {
        return title;
    }
    if !names_app(tail, desc, exe) {
        return title;
    }

    buf.clear();
    buf.reserve(title.len() + 3);
    buf.push_str(tail);
    buf.push_str(" - ");
    buf.push_str(head);
    buf
}

/// Does this title segment name the program it belongs to?
fn names_app(segment: &str, desc: Option<&str>, exe: &str) -> bool {
    if desc.is_some_and(|d| !d.is_empty() && contains_ci(segment, d)) {
        return true;
    }
    // Fall back to the image name, which is available on the first frame.
    let stem = exe
        .strip_suffix(".exe")
        .or_else(|| exe.strip_suffix(".EXE"))
        .unwrap_or(exe);
    if stem.is_empty() {
        return false;
    }
    if contains_ci(segment, stem) {
        return true;
    }
    // "Microsoft Teams" against `ms-teams.exe`: neither contains the other, but
    // a distinctive word does. Short words would match far too much.
    segment
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.len() >= 4 && contains_ci(stem, w))
}

/// ASCII-case-insensitive substring test, without allocating a lowercase copy.
/// Non-ASCII bytes compare exactly, which is what we want for names.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return n.is_empty();
    }
    h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// Does this process match the search box?
fn row_matches(p: &Proc, meta: &MetaStore, needle_lower: &str) -> bool {
    if p.name.to_ascii_lowercase().contains(needle_lower) {
        return true;
    }
    // Bare numbers are almost always a PID lookup.
    if let Ok(pid) = needle_lower.parse::<u32>() {
        if p.pid == pid {
            return true;
        }
    }
    if p
        .window_title
        .as_ref()
        .is_some_and(|w| w.to_ascii_lowercase().contains(needle_lower))
    {
        return true;
    }
    if let Some(m) = meta.get(p.key()) {
        for field in [&m.description, &m.company, &m.path, &m.user] {
            if field
                .as_ref()
                .is_some_and(|s| s.to_ascii_lowercase().contains(needle_lower))
            {
                return true;
            }
        }
    }
    false
}

/// Sort a sibling group by the active column, or by a frozen rank.
pub fn sort_indices(
    v: &mut [u32],
    snap: &Snapshot,
    meta: &MetaStore,
    sort_by: u16,
    desc: bool,
    frozen_rank: Option<&rustc_hash::FxHashMap<ProcKey, u32>>,
) {
    // While the order is frozen, position is dictated entirely by the rank
    // captured before the freeze. Processes that appeared since have no rank and
    // are appended in PID order, so the rows already on screen never move.
    if let Some(rank) = frozen_rank {
        v.sort_unstable_by_key(|&a| {
            let p = &snap.procs[a as usize];
            (
                rank.get(&p.key()).copied().unwrap_or(u32::MAX),
                p.pid,
            )
        });
        return;
    }

    // Name-like keys need the metadata string, which means a map lookup per
    // comparison. Resolve once into a key vector instead when sorting by those.
    match sort_by {
        C_CPU => v.sort_unstable_by(|&a, &b| {
            snap.procs[a as usize]
                .sub_cpu
                .total_cmp(&snap.procs[b as usize].sub_cpu)
        }),
        C_MEM => v.sort_unstable_by(|&a, &b| {
            snap.procs[a as usize]
                .sub_private_ws
                .cmp(&snap.procs[b as usize].sub_private_ws)
        }),
        C_DISK => v.sort_unstable_by(|&a, &b| {
            snap.procs[a as usize]
                .sub_disk_bps
                .total_cmp(&snap.procs[b as usize].sub_disk_bps)
        }),
        C_PID => v.sort_unstable_by_key(|&a| snap.procs[a as usize].pid),
        C_STATUS => v.sort_unstable_by_key(|&a| snap.procs[a as usize].state == RunState::Running),
        C_TYPE => v.sort_unstable_by_key(|&a| snap.procs[a as usize].category),
        C_USER => v.sort_unstable_by_key(|&a| {
            meta.get(snap.procs[a as usize].key())
                .and_then(|m| m.user.clone())
                .unwrap_or_else(|| Arc::from(""))
                .to_ascii_lowercase()
        }),
        C_PUBLISHER => v.sort_unstable_by_key(|&a| {
            meta.get(snap.procs[a as usize].key())
                .and_then(|m| m.company.clone())
                .unwrap_or_else(|| Arc::from(""))
                .to_ascii_lowercase()
        }),
        // C_NAME and anything unknown.
        _ => v.sort_unstable_by_key(|&a| snap.procs[a as usize].name.to_ascii_lowercase()),
    }
    if desc {
        v.reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::app_first;

    /// Every title here was copied off a running process, separators included.
    /// The point of the test is that the separator is *not* the thing being
    /// keyed on -- three of these use three different ones.
    fn reorder(title: &str, desc: Option<&str>, exe: &str) -> String {
        let mut buf = String::new();
        app_first(title, desc, exe, &mut buf).to_owned()
    }

    #[test]
    fn promotes_the_application_past_any_separator() {
        // Hyphen, and two of them, so the rightmost has to win.
        assert_eq!(
            reorder(
                "processes.rs (Working Tree) (processes.rs) - yet-another-task-manager \
                 - Visual Studio Code",
                Some("Visual Studio Code"),
                "Code.exe",
            ),
            "Visual Studio Code - processes.rs (Working Tree) (processes.rs) \
             - yet-another-task-manager"
        );
        // Em dash, and no version resource yet: the image name has to carry it.
        assert_eq!(
            reorder(
                "http://localhost:3000/TeamCity?project=X — Mozilla Firefox",
                None,
                "firefox.exe",
            ),
            "Mozilla Firefox - http://localhost:3000/TeamCity?project=X"
        );
        // Pipe, and `ms-teams.exe` shares no substring with "Microsoft Teams" --
        // only the word "Teams" connects them.
        assert_eq!(
            reorder(
                "Conversation | All Technodigit (3DReshaper) | Absences | Microsoft Teams",
                None,
                "ms-teams.exe",
            ),
            "Microsoft Teams - Conversation | All Technodigit (3DReshaper) | Absences"
        );
        // The app's own title carries a version; it comes along.
        assert_eq!(
            reorder(
                r"yatm (Roaming\yatm) - File Pilot v0.7.0",
                Some("File Pilot"),
                "FPilot.exe",
            ),
            r"File Pilot v0.7.0 - yatm (Roaming\yatm)"
        );
    }

    #[test]
    fn leaves_everything_else_alone() {
        // No separator at all.
        assert_eq!(reorder("Downloads", Some("Windows Explorer"), "explorer.exe"), "Downloads");
        // Already application-first.
        assert_eq!(
            reorder("Mozilla Firefox — Some page", None, "firefox.exe"),
            "Mozilla Firefox — Some page"
        );
        // A separator, but neither side is the application: reordering this
        // would invent a meaning the title does not have.
        assert_eq!(
            reorder("Report - Q4", Some("Microsoft Excel"), "excel.exe"),
            "Report - Q4"
        );
        // An unspaced hyphen belongs to the file name, not to us.
        assert_eq!(
            reorder("my-file.txt - Notepad", Some("Notepad"), "notepad.exe"),
            "Notepad - my-file.txt"
        );
        // Nothing to anchor on: no description, and a name that appears nowhere.
        assert_eq!(reorder("Some - Thing", None, "launcher.exe"), "Some - Thing");
    }
}
