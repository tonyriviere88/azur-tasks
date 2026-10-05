//! The Processes tab: a grouped, sortable process hierarchy.
//!
//! The display list is cached and only rebuilt when something that affects it
//! actually changes (new sample, sort, filter, expand/collapse). Scrolling and
//! hovering therefore cost nothing beyond painting the visible rows.

use super::fmt;
use super::icons::IconCache;
use super::table::{Align, Column, RowPainter, Table};
use super::theme::Theme;
use azur_egui_theme::components::{ContextMenu, Text, TextColor, TextVariant};
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
    /// Header of the section accounting for RAM no process owns.
    SysGroup,
    /// One term of that section.
    Sys(SysRow),
}

impl RowKind {
    /// Whether clicking anywhere on the row folds it, rather than only on the
    /// disclosure triangle.
    fn is_section(self) -> bool {
        matches!(self, Self::Group(_) | Self::SysGroup)
    }
}

#[derive(Clone, Copy)]
struct Row {
    kind: RowKind,
    depth: u16,
    /// Whether this process has children (so we draw an expander).
    has_children: bool,
    expanded: bool,
}

/// The physical RAM the process rows cannot account for, split into the
/// populations that make it up.
///
/// The Memory column apportions `WorkingSetPrivateSize`: pages that are resident
/// *and* private *and* attributable to exactly one process. The header's
/// percentage is `PhysicalTotal - PhysicalAvailable` -- every page that is not
/// free, zeroed, or on the standby list, the kernel's included. Three whole
/// populations are in the second number and in none of the rows:
///
///   - pages shared between processes. A DLL mapped by fifty programs is
///     resident once; charging all fifty would multiply it by fifty and charging
///     one would be arbitrary, so the private working set charges nobody.
///   - the kernel's own memory -- pool, drivers, page tables. It heads no
///     process, so it has no row to appear in.
///   - the modified list: pages already evicted from a working set, still in RAM,
///     waiting to be written to the page file.
///
/// Only the last two can be read off a counter. Measuring the shared pages
/// properly would mean asking about every page of every process, because
/// `WorkingSetSize - WorkingSetPrivateSize` counts each shared page once per
/// process that maps it and so overstates by however much sharing is going on.
/// It is taken as the remainder instead. That is exact by construction, and it is
/// what makes the section close the gap rather than merely narrow it: the process
/// rows plus this section equal the header total, always.
#[derive(Clone, Copy, Default)]
struct SysMem {
    /// Everything left over: shared and mapped pages, page tables, kernel
    /// stacks, the RAM virtualization-based security hides from every caller.
    shared: u64,
    /// Resident pool and drivers.
    kernel: u64,
    /// The modified page list.
    transit: u64,
    /// System RAM the display driver has locked for the GPU. Part of the terms
    /// above rather than a fourth one -- WDDM charges most of it to the process
    /// that allocated it -- so it is shown for scale and never added in.
    gpu: u64,
    /// `shared + kernel + transit`, and equally `in_use` less what the rows own.
    total: u64,
}

impl SysMem {
    fn of(snap: &Snapshot) -> Self {
        // What the rows above already account for, by the same measure they
        // display. Folded parents report a subtree total, but every process is
        // listed exactly once, so the flat sum is the same number.
        let attributed: u64 = snap.procs.iter().map(|p| p.private_ws).sum();
        let total = snap.mem.in_use.saturating_sub(attributed);

        // `paged_pool` is the whole pool, including the part that has been paged
        // out and is therefore not resident at all. On this machine that
        // overstates by around a twentieth of the pool -- 0.1 GB against a 50 GB
        // total -- and separating it costs another counter on every sample. The
        // error lands in `shared`, which is a remainder and not a measurement
        // for exactly this reason.
        //
        // Clamped, and in this order, so the three terms always sum to `total`
        // even mid-sample when the pools and the physical total disagree.
        let kernel = snap
            .mem
            .paged_pool
            .saturating_add(snap.mem.nonpaged_pool)
            .min(total);
        let transit = snap.mem.modified.min(total - kernel);
        Self {
            shared: total - kernel - transit,
            kernel,
            transit,
            gpu: snap.gpus.iter().map(|g| g.shared_used).sum(),
            total,
        }
    }
}

/// One line of the system section, in the order they are shown under a header.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SysRow {
    Shared,
    Kernel,
    Transit,
    Gpu,
}

impl SysRow {
    const COUNT: usize = 4;
    const ALL: [SysRow; Self::COUNT] = [Self::Shared, Self::Kernel, Self::Transit, Self::Gpu];

    fn label(self) -> &'static str {
        match self {
            Self::Shared => "Shared code & mapped files",
            Self::Kernel => "Windows kernel",
            Self::Transit => "In transit to the page file",
            // Named as already counted on purpose: it overlaps the three others
            // instead of extending them, and a bare "GPU driver" sitting in a
            // column of addends reads as a fourth addend. Worded without
            // reference to position, because the flat list has no section for it
            // to sit at the bottom of -- there it ranks by size like everything
            // else, and can come out anywhere.
            Self::Gpu => "GPU driver (already counted)",
        }
    }

    fn value(self, m: &SysMem) -> u64 {
        match self {
            Self::Shared => m.shared,
            Self::Kernel => m.kernel,
            Self::Transit => m.transit,
            Self::Gpu => m.gpu,
        }
    }
}

/// One entry at the top level of a list: a process, with whatever subtree hangs
/// off it, or a single system reading.
#[derive(Clone, Copy)]
enum Top {
    /// Index into `Snapshot::procs`.
    Proc(u32),
    Sys(SysRow),
}

/// Where a top-level entry sorts.
///
/// The `u8` is a bucket, compared ascending whatever the sort direction, so that
/// a row with nothing to compare stays out of the way at the bottom instead of
/// being carried to the top by a reversal: a reading has no user name and no
/// publisher, and inventing an empty string for it would rank it *first* under an
/// ascending sort of those columns.
type TopKey = (u8, f64);

pub struct ProcessesTab {
    pub table: Table,
    rows: Vec<Row>,
    /// Processes the user has expanded. Everything starts collapsed, like Task
    /// Manager, so the list opens as a short summary rather than 600 rows.
    expanded: FxHashSet<ProcKey>,
    collapsed_groups: FxHashSet<Category>,
    /// Fold state of the system section, which is not keyed by [`Category`]: no
    /// process is in it.
    system_collapsed: bool,
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
    /// The same, for the system readings, which have no [`ProcKey`] to be keyed
    /// by. Indexed by [`SysRow`]; `u32::MAX` until a first unfrozen build has
    /// placed them.
    sys_rank: [u32; SysRow::COUNT],
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
        // Name, CPU, Memory, Disk, PID and nothing else: the five a reader opens
        // this tab to see. Status, Type, User name and Publisher are off by
        // default and one click away in the header's context menu, the same
        // arrangement the Details tab uses for its own long tail.
        for c in table.cols.iter_mut() {
            if matches!(c.id, C_STATUS | C_TYPE | C_USER | C_PUBLISHER) {
                c.visible = false;
            }
        }
        Self {
            table,
            rows: Vec::new(),
            expanded: FxHashSet::default(),
            collapsed_groups: FxHashSet::default(),
            system_collapsed: false,
            selected: None,
            selected_row: None,
            built_grouped: true,
            rank: rustc_hash::FxHashMap::default(),
            sys_rank: [u32::MAX; SysRow::COUNT],
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

        // The RAM no process row owns. Left out while filtering: the readings
        // exist to reconcile the rows with the header total, and a filtered list
        // does not add up to that anyway -- shown there they would read as four
        // more things matching the search.
        let sys = SysMem::of(snap);
        let sys = (filter.is_empty() && sys.total > 0).then_some(sys);

        if self.grouped() {
            for cat in Category::ALL {
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
                self.emit_roots(snap, meta, Some(cat), sort_by, desc, 1, None);
            }
            // Grouping is what the name sort does (see `grouped`), so here the
            // readings are a section like the others rather than nine rows mixed
            // into an alphabetical list. Last, because the order of the sections
            // is fixed and this is the one that is not about processes.
            if let Some(sys) = sys {
                self.rows.push(Row {
                    kind: RowKind::SysGroup,
                    depth: 0,
                    has_children: true,
                    expanded: !self.system_collapsed,
                });
                if !self.system_collapsed {
                    // A term at zero has nothing to say: no GPU reporting shared
                    // memory, or a machine that has not modified a page yet.
                    for kind in SysRow::ALL.into_iter().filter(|k| k.value(&sys) > 0) {
                        self.rows.push(Row {
                            kind: RowKind::Sys(kind),
                            depth: 1,
                            has_children: false,
                            expanded: false,
                        });
                    }
                }
            }
        } else {
            // Flat: no section headers for anything, readings included, and they
            // take their place in the ranking by the column being sorted.
            self.emit_roots(snap, meta, None, sort_by, desc, 0, sys.as_ref());
        }

        // Record the order so a later freeze has something to hold on to.
        // Skipped while frozen, or the frozen order would drift as new
        // processes are appended.
        if !self.frozen {
            self.rank.clear();
            self.rank.reserve(self.rows.len());
            self.sys_rank = [u32::MAX; SysRow::COUNT];
            for (pos, row) in self.rows.iter().enumerate() {
                match row.kind {
                    RowKind::Proc(i) => {
                        self.rank.insert(snap.procs[i as usize].key(), pos as u32);
                    }
                    RowKind::Sys(kind) => self.sys_rank[kind as usize] = pos as u32,
                    RowKind::Group(_) | RowKind::SysGroup => {}
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

    #[allow(clippy::too_many_arguments)]
    fn emit_roots(
        &mut self,
        snap: &Snapshot,
        meta: &MetaStore,
        cat: Option<Category>,
        sort_by: u16,
        desc: bool,
        base_depth: u16,
        sys: Option<&SysMem>,
    ) {
        self.sort_buf.clear();
        // The top level is the group *heads*, and never `snap.roots`: those are the
        // roots of the parent-pid tree, which files every task the reader launched
        // under the shell and most of the machine under session 0. `assign_groups`
        // has already applied Task Manager's rule (see `sampler::is_launcher`) and
        // left the answer in `group_head` -- a process whose parent merely started
        // it heads its own entry. That is a fact about the process, not about the
        // ordering, so it holds whether or not the list is split into sections;
        // sections only narrow which heads belong to this one.
        //
        // `group`/`group_head` come from the sampler, which assigns a whole subtree
        // to one group. Selecting on `category` here instead would both hide an app
        // launched by a shell and make a browser look childless (its renderers are
        // background processes, but they belong under the browser).
        for (i, p) in snap.procs.iter().enumerate() {
            if p.group_head && self.matched[i] && cat.is_none_or(|c| p.group == c) {
                self.sort_buf.push(i as u32);
            }
        }
        // Taken out so the frozen rank can be read while `sort_buf` is borrowed
        // mutably; both are fields of `self`.
        let rank = std::mem::take(&mut self.rank);
        let frozen = self.frozen.then_some(&rank);
        sort_indices(&mut self.sort_buf, snap, meta, sort_by, desc, frozen);

        // The system readings join the top level as entries in their own right,
        // ordered by the same column as everything else. Without that, the Memory
        // column would rank six hundred processes and leave the four largest
        // figures on the machine sitting wherever they were put -- a ranking that
        // omits its biggest rows tells the reader something false.
        //
        // Merged after `sort_indices` rather than inside it: that function is
        // shared with the Details tab, which has no readings to place, and it is
        // what resolves the name-like columns and the frozen order. The pass below
        // is stable and keyed the same way, so it disturbs neither -- the
        // processes arrive already ordered and keep that order among themselves,
        // ties included.
        let sys_rank = self.sys_rank;
        let mut tops: Vec<Top> = self.sort_buf.iter().copied().map(Top::Proc).collect();
        if let Some(sys) = sys {
            tops.extend(
                SysRow::ALL
                    .into_iter()
                    .filter(|k| k.value(sys) > 0)
                    .map(Top::Sys),
            );

            let key = |top: &Top| -> TopKey {
                // A frozen rank is a display position, so it sorts ascending and
                // never by value -- which is the whole point of the freeze.
                if frozen.is_some() {
                    return match *top {
                        Top::Proc(i) => (
                            0,
                            frozen
                                .and_then(|r| r.get(&snap.procs[i as usize].key()).copied())
                                .unwrap_or(u32::MAX) as f64,
                        ),
                        Top::Sys(kind) => (0, sys_rank[kind as usize] as f64),
                    };
                }
                match *top {
                    Top::Proc(i) => {
                        let p = &snap.procs[i as usize];
                        match sort_by {
                            C_CPU => (0, p.sub_cpu as f64),
                            C_MEM => (0, p.sub_private_ws as f64),
                            C_DISK => (0, p.sub_disk_bps as f64),
                            C_PID => (0, p.pid as f64),
                            // A string column. The processes are already in order
                            // from `sort_indices`; bucket 0 keeps them there, and
                            // the readings go in bucket 1, after them.
                            _ => (0, 0.0),
                        }
                    }
                    Top::Sys(kind) => match sort_by {
                        C_MEM => (0, kind.value(sys) as f64),
                        // No thread, no bytes moved and no PID: zero sinks a
                        // reading under those columns exactly as it sinks an idle
                        // process, which is the honest answer rather than a
                        // special case.
                        C_CPU | C_DISK | C_PID => (0, 0.0),
                        _ => (1, 0.0),
                    },
                }
            };

            // Ascending while frozen, whatever the sort direction says: see above.
            let ascending = frozen.is_some() || !desc;
            tops.sort_by(|a, b| {
                let ((ba, va), (bb, vb)) = (key(a), key(b));
                ba.cmp(&bb).then_with(|| {
                    if ascending {
                        va.total_cmp(&vb)
                    } else {
                        vb.total_cmp(&va)
                    }
                })
            });
        }

        // A child nests here unless it heads a group of its own (an app launched
        // by another app gets its own top-level entry, as in Task Manager), so
        // every process is listed exactly once. Kept identical to the expansion
        // filter below so an expander never opens onto an empty subtree.
        let visible_child = |snap: &Snapshot, matched: &[bool], c: u32| -> bool {
            matched[c as usize] && !snap.procs[c as usize].group_head
        };

        let mut children: Vec<u32> = Vec::new();
        // One entry at a time, each subtree finished before the next entry starts,
        // so a reading can sit between two processes without landing inside one of
        // their subtrees.
        for top in tops {
            let root = match top {
                Top::Sys(kind) => {
                    self.rows.push(Row {
                        kind: RowKind::Sys(kind),
                        depth: base_depth,
                        has_children: false,
                        expanded: false,
                    });
                    continue;
                }
                Top::Proc(i) => i,
            };

            // Depth-first, pushing children in reverse so they pop in sorted order.
            self.stack.clear();
            self.stack.push((root, base_depth));
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

        // What the whole machine is doing, under the column that apportions it --
        // Task Manager's headline figures. The rows below add up to these, so the
        // header is where they belong: a row's 0.4% means little until you know
        // the machine is at 6%.
        //
        // Disk is the busiest volume rather than a sum, the same choice the
        // Performance tab's disk cards make: two disks at 50% is not one disk at
        // 100%, and a total over 100% would read as a bug.
        let id = Id::new("processes");
        let mut cpu = String::new();
        fmt::percent_sign(&mut cpu, snap.cpu.total);
        let mut mem = String::new();
        fmt::percent_sign(&mut mem, snap.mem.percent());
        let mut disk = String::new();
        fmt::percent_sign(
            &mut disk,
            snap.disks.iter().map(|d| d.active_pct).fold(0.0, f32::max),
        );
        if self.table.header_with(
            ui,
            t,
            id,
            &[(C_CPU, &cpu), (C_MEM, &mem), (C_DISK, &disk)],
        ) {
            self.dirty = true;
        }

        let slots = Slots::build(&self.table);
        // Resolved once per frame rather than per painted row: it sums the private
        // working set of every process, which is not something to repeat forty
        // times while scrolling.
        let sys = SysMem::of(snap);
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
                    RowKind::SysGroup => {
                        paint_sys_group_row(rp, t, &sys, row.expanded, &slots, &mut buf)
                    }
                    RowKind::Sys(kind) => {
                        paint_sys_row(rp, t, kind, &sys, &row, &slots, &mut buf);
                        None
                    }
                };
                if rp.hovered {
                    expander_hit = hit;
                }
            },
        );

        self.scratch = buf;

        // --- interaction --------------------------------------------------
        // Only a process can be the selection, which is what disables End task,
        // the context menu and the inspector on every other kind of row: they all
        // read `selected`, and clicking a section clears it.
        if let Some(r) = out.clicked_row.or(out.context_row) {
            if let Some(row) = rows.get(r) {
                match row.kind {
                    RowKind::Proc(idx) => {
                        self.selected = Some(snap.procs[idx as usize].key());
                        self.selected_row = Some(r);
                    }
                    _ => {
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
                if (row.has_children && on_expander) || row.kind.is_section() {
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
                        _ => None,
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
                RowKind::SysGroup => self.system_collapsed = !self.system_collapsed,
                // A leaf: nothing under it to fold away.
                RowKind::Sys(_) => {}
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
                ContextMenu::new(&out.response).show(&ctx, |ui| {
                    super::process_context_menu(ui, t, pid, &name, key, path.as_deref(), action);
                });
            } else {
                // A section header or a system reading. Nothing here can be ended,
                // inspected or located on disk, so the menu says so rather than
                // offering actions that would have no target -- and it is still
                // opened, to keep the menu's bookkeeping consistent.
                ContextMenu::new(&out.response).show(&ctx, |ui| {
                    ui.add(no_process());
                });
            }
        } else {
            ContextMenu::new(&out.response).show(&ctx, |ui| {
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
                ui.add(no_process());
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
        .rect_filled(rp.rect, egui::CornerRadius::ZERO, t.bg.card);
    rp.painter.hline(
        rp.rect.x_range(),
        rp.rect.bottom() - 0.5,
        egui::Stroke::new(1.0, t.stroke.subtle),
    );

    let name_slot = slots.of(C_NAME);
    let hit = name_slot.map(|s| rp.expander(s, 6.0, expanded, t.text.secondary));

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
        rp.text_font(s, buf, t.text.primary, Align::Left, &t.fonts.body_strong);
    }

    let mut b = String::new();
    if let Some(s) = slots.of(C_CPU) {
        fmt::percent(&mut b, cpu);
        rp.text_font(s, &b, t.text.secondary, Align::Right, &t.fonts.body_strong);
    }
    if let Some(s) = slots.of(C_MEM) {
        fmt::mem_mb(&mut b, mem);
        rp.text_font(s, &b, t.text.secondary, Align::Right, &t.fonts.body_strong);
    }
    if let Some(s) = slots.of(C_DISK) {
        fmt::rate_mbs(&mut b, disk);
        rp.text_font(s, &b, t.text.secondary, Align::Right, &t.fonts.body_strong);
    }

    hit
}

/// Where a system row puts its label: level with the process names above it,
/// past the space their expander and icon take up. Nothing in this section has
/// either, but the column is read as one list and a label that started 39px to
/// the left would read as a different one.
const SYS_LABEL_X: f32 = 18.0 + 21.0;

/// Header of the system section. Returns the disclosure triangle's hit rect.
///
/// Deliberately the same surface, rule and weight as [`paint_group_row`]: it is a
/// section of the list at the same level as Apps and Background processes, and
/// giving it its own chrome would suggest it belongs to something else.
fn paint_sys_group_row(
    rp: &mut RowPainter<'_>,
    t: &Theme,
    sys: &SysMem,
    expanded: bool,
    slots: &Slots,
    buf: &mut String,
) -> Option<Rect> {
    rp.painter
        .rect_filled(rp.rect, egui::CornerRadius::ZERO, t.bg.card);
    rp.painter.hline(
        rp.rect.x_range(),
        rp.rect.bottom() - 0.5,
        egui::Stroke::new(1.0, t.stroke.subtle),
    );

    let name_slot = slots.of(C_NAME);
    let hit = name_slot.map(|s| rp.expander(s, 6.0, expanded, t.text.secondary));

    if let Some(s) = name_slot {
        // Named for what it is rather than "System": collapsed, this row is the
        // only thing a reader has to go on, and "System" alone would read as the
        // System process -- which is in Background processes, and tiny.
        rp.text_font(
            s,
            "System & shared memory",
            t.text.primary,
            Align::Left,
            &t.fonts.body_strong,
        );
    }
    // CPU and Disk stay empty. None of this holds a thread or issues I/O, and a
    // zero in those columns would read as an idle process rather than as a
    // dimension that does not apply.
    if let Some(s) = slots.of(C_MEM) {
        fmt::mem_mb(buf, sys.total);
        rp.text_font(s, buf, t.text.secondary, Align::Right, &t.fonts.body_strong);
    }

    hit
}

/// One term of the system section.
fn paint_sys_row(
    rp: &mut RowPainter<'_>,
    t: &Theme,
    kind: SysRow,
    sys: &SysMem,
    row: &Row,
    slots: &Slots,
    buf: &mut String,
) {
    // The GPU line overlaps the three above it instead of extending them, so it
    // is held back from the addends it sits among: same row, quieter voice.
    let overlapping = kind == SysRow::Gpu;
    let (label_color, value_color) = if overlapping {
        (t.text.tertiary, t.text.tertiary)
    } else {
        (t.text.secondary, t.text.primary)
    };

    if let Some(s) = slots.of(C_NAME) {
        rp.text_indent(s, row_indent(row.depth) + SYS_LABEL_X, kind.label(), label_color);
    }
    if let Some(s) = slots.of(C_MEM) {
        // No `heat` tint. The bar behind a process's figure means "this one is
        // heavy for a process"; there is no comparable expectation of a term that
        // is measuring the whole machine, so a full bar would only alarm.
        fmt::mem_mb(buf, kind.value(sys));
        rp.text(s, buf, value_color, Align::Right);
    }
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
        (Some(s), true) => Some(rp.expander(s, x, row.expanded, t.text.secondary)),
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
        // Anything that owns a window is named by it, including a suspended app --
        // being frozen changes which section it sits in, not what it is called.
        (Some(title), _) if p.category != Category::Background => {
            app_first(title, desc, &p.name, &mut reordered)
        }
        (_, Some(d)) => d,
        _ => &p.name,
    };
    let name_color = if p.state == RunState::Suspended {
        t.text.secondary
    } else {
        t.text.primary
    };
    if let Some(s) = name_slot {
        rp.text_indent(s, x, label, name_color);
    }

    // --- Status -----------------------------------------------------------
    if let (Some(s), RunState::Suspended) = (slots.of(C_STATUS), p.state) {
        rp.pill(s, "Suspended", t.status.warning, t.accent.subtle.gamma_multiply(0.8));
    }

    // --- CPU / Memory / Disk ---------------------------------------------
    if let Some(s) = slots.of(C_CPU) {
        rp.heat(s, cpu / 25.0, t.cpu);
        fmt::percent(buf, cpu);
        rp.text(s, buf, t.load_text(cpu), Align::Right);
    }

    // Tint memory relative to a 2 GB reference; above that it is notable.
    if let Some(s) = slots.of(C_MEM) {
        rp.heat(s, mem as f32 / (2.0 * 1024.0 * 1024.0 * 1024.0), t.mem);
        fmt::mem_mb(buf, mem);
        rp.text(s, buf, t.text.primary, Align::Right);
    }

    // 20 MB/s of I/O is a busy process.
    if let Some(s) = slots.of(C_DISK) {
        rp.heat(s, disk / (20.0 * 1024.0 * 1024.0), t.disk);
        fmt::rate_mbs(buf, disk);
        let disk_color = if disk < 0.05 { t.text.tertiary } else { t.text.primary };
        rp.text(s, buf, disk_color, Align::Right);
    }

    // --- PID / Type / User / Publisher -----------------------------------
    if let Some(s) = slots.of(C_PID) {
        fmt::integer(buf, p.pid as u64);
        rp.text_font(s, buf, t.text.secondary, Align::Right, &t.fonts.mono);
    }

    if let Some(s) = slots.of(C_TYPE) {
        rp.text(s, p.category.type_label(), t.text.tertiary, Align::Left);
    }

    if let Some(s) = slots.of(C_USER) {
        let user = m.as_ref().and_then(|m| m.user.clone());
        rp.text(s, user.as_deref().unwrap_or("—"), t.text.secondary, Align::Left);
    }

    if let Some(s) = slots.of(C_PUBLISHER) {
        let publisher = m.as_ref().and_then(|m| m.company.clone());
        rp.text(
            s,
            publisher.as_deref().unwrap_or("—"),
            t.text.tertiary,
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
        super::icons::draw(
            &rp.painter.with_clip_rect(cell),
            r,
            super::icons::Glyph::Shield,
            t.status.warning.gamma_multiply(0.85),
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

/// The one-line stand-in for a context menu opened where there is no process:
/// a group header, or an empty list.
fn no_process() -> Text<'static> {
    Text::new("No process selected")
        .variant(TextVariant::Caption)
        .color(TextColor::Tertiary)
}

#[cfg(test)]
mod tests {
    use super::{app_first, ProcessesTab, RowKind, SysMem, C_CPU, C_MEM, C_NAME};
    use crate::sys::meta::MetaStore;
    use crate::sys::snapshot::{Category, MemStats, Proc, Snapshot};
    use std::sync::Arc;

    /// Seven processes wired the way `Sampler::build_tree` and
    /// `Sampler::assign_groups` would leave them, small enough to reason about:
    ///
    /// ```text
    /// System        session 0, tree root      head
    ///   services    session 0                 head -- parent is session 0
    ///     svchost   session 0                 head -- parent is session 0
    ///   explorer    session 1, the shell      head -- parent is session 0
    ///     notepad   session 1, a window       head -- parent is the shell
    ///     chrome    session 1, a window       head -- parent is the shell
    ///       tab     session 1, background     nests under chrome
    /// ```
    ///
    /// Six of the seven head their own entry, so the only nesting the list should
    /// ever draw is the last one.
    fn machine() -> Snapshot {
        let head = |pid, parent, name: &str, group, start, len| Proc {
            pid,
            name: Arc::from(name),
            session: if pid < 500 { 0 } else { 1 },
            category: group,
            group,
            group_head: true,
            parent,
            child_start: start,
            child_len: len,
            ..Default::default()
        };
        Snapshot {
            procs: vec![
                head(4, u32::MAX, "System", Category::Background, 0, 2),
                head(100, 0, "services.exe", Category::Background, 2, 1),
                head(200, 1, "svchost.exe", Category::Background, 3, 0),
                head(500, 0, "explorer.exe", Category::Background, 3, 2),
                head(600, 3, "notepad.exe", Category::App, 5, 0),
                head(700, 3, "chrome.exe", Category::App, 5, 1),
                Proc {
                    pid: 701,
                    name: Arc::from("chrome.exe"),
                    session: 1,
                    // A renderer: background on its own merits, displayed in Apps
                    // under the browser that owns it.
                    category: Category::Background,
                    group: Category::App,
                    group_head: false,
                    parent: 5,
                    child_start: 6,
                    child_len: 0,
                    depth: 1,
                    ..Default::default()
                },
            ],
            child_links: vec![1, 3, 2, 4, 5, 6],
            roots: vec![0],
            ..Default::default()
        }
    }

    /// Who the list says has children, by image name.
    fn parents(sort_by: u16) -> Vec<String> {
        let snap = machine();
        let meta = MetaStore::new();
        let mut tab = ProcessesTab::new();
        tab.table.sort_by = sort_by;
        tab.table.sort_desc = sort_by != C_NAME;
        tab.rebuild(&snap, &meta, "");
        tab.rows
            .iter()
            .filter(|r| r.has_children)
            .filter_map(|r| match r.kind {
                RowKind::Proc(i) => Some(snap.procs[i as usize].name.to_string()),
                // The section headers are always expandable; they are not the
                // parentage under test.
                _ => None,
            })
            .collect()
    }

    /// The launcher rule is a fact about the process, not about the ordering, so
    /// the shape of the tree cannot depend on which column is sorted.
    ///
    /// Sorting by name additionally splits the list into sections; that changes
    /// what is *above* a row, never what hangs below it.
    #[test]
    fn only_a_real_owner_holds_children_whatever_the_sort() {
        // `chrome.exe` and nothing else: `System`, `services.exe` and
        // `explorer.exe` all launched what sits under them in the tree, and Task
        // Manager lists those beside their launcher.
        assert_eq!(parents(C_NAME), ["chrome.exe"]);
        assert_eq!(parents(C_CPU), ["chrome.exe"]);
    }

    /// Every process is listed exactly once, in both modes: the top level is the
    /// group heads, and a head never also appears under its launcher.
    #[test]
    fn every_process_is_listed_exactly_once() {
        for sort_by in [C_NAME, C_CPU] {
            let snap = machine();
            let meta = MetaStore::new();
            let mut tab = ProcessesTab::new();
            tab.table.sort_by = sort_by;
            // Expand everything expandable, so nothing is missing merely because
            // it is folded away.
            for p in &snap.procs {
                tab.expanded.insert(p.key());
            }
            tab.rebuild(&snap, &meta, "");

            let mut pids: Vec<u32> = tab
                .rows
                .iter()
                .filter_map(|r| match r.kind {
                    RowKind::Proc(i) => Some(snap.procs[i as usize].pid),
                    _ => None,
                })
                .collect();
            pids.sort_unstable();
            assert_eq!(pids, [4, 100, 200, 500, 600, 700, 701], "sort {sort_by}");
        }
    }

    const GB: u64 = 1024 * 1024 * 1024;

    /// `machine()` with a memory reading on it: 10 GB physically in use, of which
    /// the process rows own 3.
    ///
    /// `sub_private_ws` is set alongside `private_ws` because that is the field
    /// the Memory column sorts and displays on; here nothing is nested under the
    /// two that hold memory, so the subtree total is the process's own.
    fn machine_with_memory() -> Snapshot {
        let mut snap = machine();
        snap.procs[0].private_ws = GB;
        snap.procs[0].sub_private_ws = GB;
        snap.procs[1].private_ws = 2 * GB;
        snap.procs[1].sub_private_ws = 2 * GB;
        snap.mem = MemStats {
            total: 16 * GB,
            available: 6 * GB,
            in_use: 10 * GB,
            paged_pool: GB,
            nonpaged_pool: 2 * GB,
            modified: GB / 2,
            ..Default::default()
        };
        snap
    }

    /// The whole point of the section: its terms account for the difference
    /// between what the rows own and what the header claims, exactly. A remainder
    /// that merely narrowed the gap would leave a reader where they started.
    #[test]
    fn the_system_terms_close_the_gap_exactly() {
        let snap = machine_with_memory();
        let sys = SysMem::of(&snap);

        let attributed: u64 = snap.procs.iter().map(|p| p.private_ws).sum();
        assert_eq!(attributed, 3 * GB);
        assert_eq!(sys.total, snap.mem.in_use - attributed);
        assert_eq!(sys.shared + sys.kernel + sys.transit, sys.total);

        assert_eq!(sys.kernel, 3 * GB);
        assert_eq!(sys.transit, GB / 2);
        // The remainder, which is where any error in the measured terms lands.
        assert_eq!(sys.shared, 7 * GB - 3 * GB - GB / 2);
    }

    /// Physical use and the pool figures come from two different calls, so a
    /// sample can catch them disagreeing -- a pool larger than the gap it is
    /// meant to sit inside. The terms still have to add up rather than wrap
    /// around into an 18-exabyte row.
    #[test]
    fn a_skewed_sample_cannot_underflow_the_remainder() {
        let mut snap = machine_with_memory();
        snap.mem.nonpaged_pool = 40 * GB;
        let sys = SysMem::of(&snap);
        assert_eq!(sys.shared + sys.kernel + sys.transit, sys.total);
        assert_eq!(sys.kernel, sys.total);
        assert_eq!(sys.transit, 0);
        assert_eq!(sys.shared, 0);
    }

    /// Sorted by name the list is in sections, and the readings are one of them:
    /// a header, its terms, and nothing after them.
    ///
    /// Nothing in it is a process, which is what leaves End task, the context menu
    /// and the inspector with no target on those rows -- they all read `selected`,
    /// and only a `Proc` row can set it.
    #[test]
    fn grouped_the_readings_are_a_section_of_their_own() {
        let snap = machine_with_memory();
        let meta = MetaStore::new();
        let mut tab = ProcessesTab::new();
        tab.rebuild(&snap, &meta, "");

        let at = tab
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::SysGroup))
            .expect("no system section");
        assert!(
            tab.rows[at + 1..]
                .iter()
                .all(|r| matches!(r.kind, RowKind::Sys(_))),
            "something follows the system section"
        );
        assert!(!tab.rows[at..]
            .iter()
            .any(|r| matches!(r.kind, RowKind::Proc(_))));
        // Three terms: no GPU in this snapshot, so that one is left out.
        assert_eq!(tab.rows[at + 1..].len(), 3);
    }

    /// Flat, the readings rank among the processes instead of after them. They are
    /// routinely the largest entries in the Memory column, and a ranking that left
    /// its biggest rows out of the ranking would be telling the reader something
    /// false.
    #[test]
    fn flat_the_readings_rank_by_size_among_the_processes() {
        let snap = machine_with_memory();
        let sys = SysMem::of(&snap);
        let meta = MetaStore::new();
        let mut tab = ProcessesTab::new();
        tab.table.sort_by = C_MEM;
        tab.table.sort_desc = true;
        tab.rebuild(&snap, &meta, "");

        // No section header for anything, readings included.
        assert!(!tab.rows.iter().any(|r| matches!(r.kind, RowKind::SysGroup)));

        let sizes: Vec<u64> = tab
            .rows
            .iter()
            .map(|r| match r.kind {
                RowKind::Proc(i) => snap.procs[i as usize].sub_private_ws,
                RowKind::Sys(kind) => kind.value(&sys),
                RowKind::Group(_) | RowKind::SysGroup => {
                    unreachable!("a flat list has no section headers")
                }
            })
            .collect();
        assert!(
            sizes.windows(2).all(|w| w[0] >= w[1]),
            "not ordered by size: {sizes:?}"
        );

        // Interleaved, not appended: shared (3.5G) and kernel (3G) outrank every
        // process here, and the modified list (0.5G) lands between the 1 GB
        // process and the ones holding nothing.
        let sys_at: Vec<usize> = tab
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, RowKind::Sys(_)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(sys_at, [0, 1, 4], "sizes were {sizes:?}");
    }

    /// Holding Ctrl pins the order so a moving row can be clicked. The readings
    /// have to hold still too -- they are keyed by [`SysRow`] rather than by
    /// `ProcKey`, so they need their own remembered positions to do it.
    #[test]
    fn a_frozen_list_holds_the_readings_where_they_were() {
        let mut snap = machine_with_memory();
        let meta = MetaStore::new();
        let mut tab = ProcessesTab::new();
        tab.table.sort_by = C_MEM;
        tab.table.sort_desc = true;
        tab.rebuild(&snap, &meta, "");
        let before: Vec<usize> = tab
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, RowKind::Sys(_)))
            .map(|(i, _)| i)
            .collect();

        // Freeze, then hand it a sample that would reorder everything: the two
        // processes now dwarf every reading.
        tab.frozen = true;
        snap.procs[0].sub_private_ws = 9 * GB;
        snap.procs[1].sub_private_ws = 8 * GB;
        tab.rebuild(&snap, &meta, "");

        let after: Vec<usize> = tab
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, RowKind::Sys(_)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(before, after);
    }

    /// Filtering narrows the list to matching processes, and those no longer add
    /// up to the header total -- so the section that reconciles the two has
    /// nothing to say, and showing it would read as one more search hit.
    #[test]
    fn filtering_hides_the_system_section() {
        let snap = machine_with_memory();
        let meta = MetaStore::new();
        let mut tab = ProcessesTab::new();
        tab.rebuild(&snap, &meta, "chrome");
        assert!(!tab.rows.iter().any(|r| matches!(r.kind, RowKind::SysGroup)));
    }

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
                r"aztm (Roaming\aztm) - File Pilot v0.7.0",
                Some("File Pilot"),
                "FPilot.exe",
            ),
            r"File Pilot v0.7.0 - aztm (Roaming\aztm)"
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
