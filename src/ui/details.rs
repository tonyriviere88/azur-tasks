//! The Details tab: one flat row per process, with the full column set.

use super::fmt;
use super::table::{Align, Column, Table};
use super::theme::Theme;
use azur_egui_theme::components::{ContextMenu, Text, TextColor, TextVariant};
use crate::sys::meta::MetaStore;
use crate::sys::snapshot::{ProcKey, RunState, Snapshot};
use crate::sys::util::format_filetime;
use egui::{Id, Ui};
use std::sync::Arc;

const C_NAME: u16 = 0;
const C_PID: u16 = 1;
const C_STATUS: u16 = 2;
const C_USER: u16 = 3;
const C_CPU: u16 = 4;
const C_MEM: u16 = 5;
const C_PEAK: u16 = 6;
const C_COMMIT: u16 = 7;
const C_WS: u16 = 8;
const C_THREADS: u16 = 9;
const C_HANDLES: u16 = 10;
const C_PRIO: u16 = 11;
const C_ARCH: u16 = 12;
const C_SESSION: u16 = 13;
const C_START: u16 = 14;
const C_READ: u16 = 15;
const C_WRITE: u16 = 16;
const C_FAULTS: u16 = 17;
const C_DESC: u16 = 18;
const C_PATH: u16 = 19;
const C_CMDLINE: u16 = 20;
const C_VIRT: u16 = 21;
const C_PAGED: u16 = 22;
const C_NPPOOL: u16 = 23;
const C_PFAULTS: u16 = 24;
const C_CPUTIME: u16 = 25;
const C_IOREAD: u16 = 26;
const C_IOWRITE: u16 = 27;
const C_READ_OPS: u16 = 28;
const C_WRITE_OPS: u16 = 29;
const C_OTHER_OPS: u16 = 30;
const C_OTHER_BYTES: u16 = 31;
const C_CYCLES: u16 = 32;

pub struct DetailsTab {
    pub table: Table,
    order: Vec<u32>,
    pub selected: Option<ProcKey>,
    selected_row: Option<usize>,
    built_generation: u64,
    built_filter: String,
    dirty: bool,
    scratch: String,
    /// Set by [`Self::reveal`]; resolved to a row index on the next rebuild,
    /// because the row order is not known until then.
    pending_reveal: Option<ProcKey>,
    /// Ids of the visible columns, in order. Hoisted out of `table` before the
    /// row callback runs: the callback cannot read `self.table.cols` while
    /// `Table::body` holds `self.table` mutably.
    vis: Vec<u16>,
    /// Display position per process from the last unfrozen rebuild, keyed by
    /// identity rather than index so it survives processes appearing/exiting.
    rank: rustc_hash::FxHashMap<ProcKey, u32>,
    /// True while Ctrl is held and the order is pinned.
    frozen: bool,
}

impl DetailsTab {
    pub fn new() -> Self {
        let cols = vec![
            // Name takes the leftover width, as on the Processes tab: it is the
            // column that identifies the row, so it is the one worth widening
            // when columns are hidden. Services and Startup keep the slack on
            // their last column instead, where the long free text lives.
            Column::new(C_NAME, "Name", 210.0, Align::Left)
                .min(120.0)
                .stretch(),
            Column::new(C_PID, "PID", 68.0, Align::Right),
            Column::new(C_STATUS, "Status", 82.0, Align::Left),
            Column::new(C_USER, "User name", 140.0, Align::Left),
            Column::new(C_CPU, "CPU", 62.0, Align::Right),
            Column::new(C_MEM, "Memory", 98.0, Align::Right)
                .help("Private working set: memory this process cannot share"),
            Column::new(C_PEAK, "Peak WS", 98.0, Align::Right)
                .help("Highest working set since the process started"),
            Column::new(C_COMMIT, "Commit size", 104.0, Align::Right)
                .help("Virtual memory reserved in the paging file"),
            Column::new(C_WS, "Working set", 104.0, Align::Right)
                .help("All physical memory currently mapped, shared included"),
            Column::new(C_THREADS, "Threads", 72.0, Align::Right),
            Column::new(C_HANDLES, "Handles", 80.0, Align::Right),
            Column::new(C_PRIO, "Base priority", 96.0, Align::Right),
            Column::new(C_ARCH, "Architecture", 96.0, Align::Left),
            Column::new(C_SESSION, "Session", 72.0, Align::Right),
            Column::new(C_START, "Started", 150.0, Align::Left),
            Column::new(C_READ, "Read bytes", 100.0, Align::Right),
            Column::new(C_WRITE, "Write bytes", 100.0, Align::Right),
            Column::new(C_FAULTS, "Hard faults/s", 100.0, Align::Right)
                .help("Page faults that had to hit the disk"),
            Column::new(C_DESC, "Description", 220.0, Align::Left),
            Column::new(C_PATH, "Image path", 330.0, Align::Left),
            Column::new(C_CMDLINE, "Command line", 460.0, Align::Left),
            Column::new(C_VIRT, "Virtual size", 104.0, Align::Right)
                .help("Total virtual address space reserved"),
            Column::new(C_PAGED, "Paged pool", 96.0, Align::Right)
                .help("Kernel memory chargeable to this process, pageable"),
            Column::new(C_NPPOOL, "NP pool", 88.0, Align::Right)
                .help("Kernel memory chargeable to this process, never paged out"),
            Column::new(C_PFAULTS, "Page faults", 96.0, Align::Right)
                .help("Total page faults since the process started"),
            Column::new(C_CPUTIME, "CPU time", 92.0, Align::Right)
                .help("Total processor time consumed since start"),
            Column::new(C_IOREAD, "I/O read", 96.0, Align::Right)
                .help("Current read throughput"),
            Column::new(C_IOWRITE, "I/O write", 96.0, Align::Right)
                .help("Current write throughput"),
            Column::new(C_READ_OPS, "I/O reads", 96.0, Align::Right)
                .help("Number of read operations issued"),
            Column::new(C_WRITE_OPS, "I/O writes", 96.0, Align::Right)
                .help("Number of write operations issued"),
            Column::new(C_OTHER_OPS, "I/O other", 96.0, Align::Right)
                .help("Operations that are neither reads nor writes (mostly ioctls)"),
            Column::new(C_OTHER_BYTES, "I/O other bytes", 112.0, Align::Right),
            Column::new(C_CYCLES, "Cycles", 116.0, Align::Right)
                .help("Raw CPU cycles consumed; unlike CPU time this is unaffected by clock scaling"),
        ];
        // Columns past Description are off by default; they are available from
        // the header context menu, as in Task Manager.
        let mut table = Table::new(cols, C_CPU, true);
        for c in table.cols.iter_mut() {
            if matches!(
                c.id,
                C_PEAK | C_WS | C_SESSION | C_START | C_READ | C_WRITE | C_FAULTS | C_PATH
                    | C_CMDLINE | C_ARCH | C_VIRT | C_PAGED | C_NPPOOL | C_PFAULTS | C_CPUTIME
                    | C_IOREAD | C_IOWRITE | C_READ_OPS | C_WRITE_OPS | C_OTHER_OPS
                    | C_OTHER_BYTES | C_CYCLES
            ) {
                c.visible = false;
            }
        }
        Self {
            table,
            order: Vec::new(),
            selected: None,
            selected_row: None,
            built_generation: u64::MAX,
            built_filter: String::new(),
            dirty: true,
            scratch: String::new(),
            pending_reveal: None,
            vis: Vec::new(),
            rank: rustc_hash::FxHashMap::default(),
            frozen: false,
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Drop the selection, for Escape. Both halves, for the reason
    /// [`super::processes::ProcessesTab::clear_selection`] gives.
    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.selected_row = None;
    }

    /// Select a process and scroll it into view (used by "Go to details").
    pub fn reveal(&mut self, key: ProcKey) {
        self.selected = Some(key);
        self.dirty = true;
        self.pending_reveal = Some(key);
    }

    fn rebuild(&mut self, snap: &Snapshot, meta: &MetaStore, filter: &str) {
        self.order.clear();
        let needle = filter.to_ascii_lowercase();
        for (i, p) in snap.procs.iter().enumerate() {
            if !needle.is_empty() {
                let m = meta.get(p.key());
                let hit = p.name.to_ascii_lowercase().contains(&needle)
                    || p.pid.to_string() == needle
                    || m.as_ref().is_some_and(|m| {
                        [&m.description, &m.company, &m.path, &m.user, &m.cmdline]
                            .iter()
                            .any(|f| {
                                f.as_ref()
                                    .is_some_and(|s| s.to_ascii_lowercase().contains(&needle))
                            })
                    });
                if !hit {
                    continue;
                }
            }
            self.order.push(i as u32);
        }

        if self.frozen {
            // Hold the captured order; anything new lands at the end in PID
            // order so rows already on screen do not move under the cursor.
            let rank = &self.rank;
            self.order.sort_unstable_by_key(|&i| {
                let p = &snap.procs[i as usize];
                (rank.get(&p.key()).copied().unwrap_or(u32::MAX), p.pid)
            });
        } else {
            sort(
                &mut self.order,
                snap,
                meta,
                self.table.sort_by,
                self.table.sort_desc,
            );
            self.rank.clear();
            self.rank.reserve(self.order.len());
            for (pos, &i) in self.order.iter().enumerate() {
                self.rank.insert(snap.procs[i as usize].key(), pos as u32);
            }
        }

        self.selected_row = self.selected.and_then(|key| {
            self.order
                .iter()
                .position(|&i| snap.procs[i as usize].key() == key)
        });
        if let Some(key) = self.pending_reveal.take() {
            if let Some(row) = self
                .order
                .iter()
                .position(|&i| snap.procs[i as usize].key() == key)
            {
                self.table.scroll_to_row = Some(row);
            }
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        snap: &Snapshot,
        meta: &MetaStore,
        filter: &str,
        action: &mut Option<super::Action>,
    ) {
        // Holding Ctrl pins the row order (see the Processes tab).
        let freeze = ui.input(|i| i.modifiers.ctrl);
        if freeze != self.frozen {
            self.frozen = freeze;
            self.dirty = true;
        }

        if self.dirty || self.built_generation != snap.generation || self.built_filter != filter {
            self.rebuild(snap, meta, filter);
            self.built_generation = snap.generation;
            self.built_filter = filter.to_owned();
            self.dirty = false;
        }

        let id = Id::new("details");
        if self.table.header(ui, t, id) {
            self.dirty = true;
        }

        let order = std::mem::take(&mut self.order);
        let mut buf = std::mem::take(&mut self.scratch);
        let mut vis = std::mem::take(&mut self.vis);
        vis.clear();
        vis.extend(self.table.cols.iter().filter(|c| c.visible).map(|c| c.id));

        let out = self
            .table
            .body(ui, t, id, order.len(), self.selected_row, |rp| {
                let p = &snap.procs[order[rp.row] as usize];
                let m = meta.get(p.key());
                // Walk the visible columns in order; `slot` indexes the laid-out
                // cell rects, which only cover the visible columns.
                for (slot, &cid) in vis.iter().enumerate() {
                    match cid {
                        C_NAME => rp.text(slot, &p.name, t.text.primary, Align::Left),
                        C_PID => {
                            fmt::integer(&mut buf, p.pid as u64);
                            rp.text_font(slot, &buf, t.text.secondary, Align::Right, &t.fonts.mono);
                        }
                        C_STATUS => {
                            let s = match p.state {
                                RunState::Running => "Running",
                                RunState::Suspended => "Suspended",
                            };
                            let c = match p.state {
                                RunState::Running => t.text.tertiary,
                                RunState::Suspended => t.status.warning,
                            };
                            rp.text(slot, s, c, Align::Left);
                        }
                        C_USER => rp.text(
                            slot,
                            m.as_ref().and_then(|m| m.user.as_deref()).unwrap_or("—"),
                            t.text.secondary,
                            Align::Left,
                        ),
                        C_CPU => {
                            rp.heat(slot, p.cpu / 25.0, t.cpu);
                            fmt::percent(&mut buf, p.cpu);
                            rp.text(slot, &buf, t.load_text(p.cpu), Align::Right);
                        }
                        C_MEM => {
                            rp.heat(
                                slot,
                                p.private_ws as f32 / (2.0 * 1024.0 * 1024.0 * 1024.0),
                                t.mem,
                            );
                            fmt::mem_mb(&mut buf, p.private_ws);
                            rp.text(slot, &buf, t.text.primary, Align::Right);
                        }
                        C_PEAK => {
                            fmt::mem_mb(&mut buf, p.peak_ws);
                            rp.text(slot, &buf, t.text.secondary, Align::Right);
                        }
                        C_COMMIT => {
                            fmt::mem_mb(&mut buf, p.commit);
                            rp.text(slot, &buf, t.text.secondary, Align::Right);
                        }
                        C_WS => {
                            fmt::mem_mb(&mut buf, p.working_set);
                            rp.text(slot, &buf, t.text.secondary, Align::Right);
                        }
                        C_THREADS => {
                            fmt::integer(&mut buf, p.threads as u64);
                            rp.text(slot, &buf, t.text.secondary, Align::Right);
                        }
                        C_HANDLES => {
                            fmt::integer(&mut buf, p.handles as u64);
                            rp.text(slot, &buf, t.text.secondary, Align::Right);
                        }
                        C_PRIO => {
                            fmt::integer(&mut buf, p.base_priority as u64);
                            rp.text(slot, &buf, t.text.secondary, Align::Right);
                        }
                        C_ARCH => rp.text(
                            slot,
                            m.as_ref().map(|m| m.arch.label()).unwrap_or("—"),
                            t.text.secondary,
                            Align::Left,
                        ),
                        C_SESSION => {
                            fmt::integer(&mut buf, p.session as u64);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_START => {
                            let s = format_filetime(p.create_time, true);
                            rp.text_font(slot, &s, t.text.tertiary, Align::Left, &t.fonts.mono);
                        }
                        C_READ => {
                            fmt::bytes(&mut buf, p.io_read_total);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_WRITE => {
                            fmt::bytes(&mut buf, p.io_write_total);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_FAULTS => {
                            fmt::percent(&mut buf, p.hard_faults_per_s);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_DESC => rp.text(
                            slot,
                            m.as_ref()
                                .and_then(|m| m.description.as_deref())
                                .unwrap_or("—"),
                            t.text.secondary,
                            Align::Left,
                        ),
                        C_PATH => rp.text(
                            slot,
                            m.as_ref().and_then(|m| m.path.as_deref()).unwrap_or("—"),
                            t.text.tertiary,
                            Align::Left,
                        ),
                        C_CMDLINE => rp.text_font(
                            slot,
                            m.as_ref().and_then(|m| m.cmdline.as_deref()).unwrap_or("—"),
                            t.text.tertiary,
                            Align::Left,
                            &t.fonts.mono,
                        ),
                        C_VIRT => {
                            fmt::mem_mb(&mut buf, p.virtual_size);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_PAGED => {
                            fmt::bytes(&mut buf, p.paged_pool);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_NPPOOL => {
                            fmt::bytes(&mut buf, p.nonpaged_pool);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_PFAULTS => {
                            fmt::integer(&mut buf, p.page_faults as u64);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_CPUTIME => {
                            fmt::cpu_time(&mut buf, p.cpu_time_100ns);
                            rp.text_font(slot, &buf, t.text.secondary, Align::Right, &t.fonts.mono);
                        }
                        C_IOREAD => {
                            fmt::rate_mbs(&mut buf, p.io_read_bps);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_IOWRITE => {
                            fmt::rate_mbs(&mut buf, p.io_write_bps);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_READ_OPS => {
                            fmt::integer(&mut buf, p.read_ops);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_WRITE_OPS => {
                            fmt::integer(&mut buf, p.write_ops);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_OTHER_OPS => {
                            fmt::integer(&mut buf, p.other_ops);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_OTHER_BYTES => {
                            fmt::bytes(&mut buf, p.io_other_total);
                            rp.text(slot, &buf, t.text.tertiary, Align::Right);
                        }
                        C_CYCLES => {
                            fmt::integer(&mut buf, p.cycle_time);
                            rp.text_font(slot, &buf, t.text.tertiary, Align::Right, &t.fonts.mono);
                        }
                        _ => {}
                    }
                }
            });

        self.scratch = buf;
        self.vis = vis;

        if let Some(r) = out.clicked_row.or(out.context_row) {
            if let Some(&i) = order.get(r) {
                self.selected = Some(snap.procs[i as usize].key());
                self.selected_row = Some(r);
            }
        }

        // Up and down the list. The selection is a process key rather than a row,
        // so the row it lands on has to be turned back into one -- otherwise the
        // next sample, which rebuilds the order, would move the selection with the
        // row index instead of keeping it on the process.
        if let Some(r) = super::table::list_nav(ui, self.selected_row, order.len()) {
            if let Some(&i) = order.get(r) {
                self.selected = Some(snap.procs[i as usize].key());
                self.selected_row = Some(r);
                self.table.scroll_to_row = Some(r);
            }
        }

        let target = out
            .context_row
            .and_then(|r| order.get(r).copied())
            .or_else(|| {
                self.selected
                    .and_then(|k| snap.find_by_key(k))
                    .map(|i| i as u32)
            });

        // Columns are picked by right-clicking the header, which every table
        // supports; this menu is only about the row under the cursor.
        ContextMenu::new(&out.response).show(&ui.ctx().clone(), |ui| match target {
            Some(i) => {
                let p = &snap.procs[i as usize];
                let key = p.key();
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
            }
            None => {
                ui.add(
                    Text::new("Right-click a column header to choose columns")
                        .variant(TextVariant::Caption)
                        .color(TextColor::Tertiary),
                );
            }
        });

        self.order = order;
    }
}

fn sort(v: &mut [u32], snap: &Snapshot, meta: &MetaStore, by: u16, desc: bool) {
    macro_rules! by_key {
        ($f:expr) => {
            v.sort_unstable_by_key($f)
        };
    }
    match by {
        C_PID => by_key!(|&a: &u32| snap.procs[a as usize].pid),
        C_CPU => v.sort_unstable_by(|&a, &b| {
            snap.procs[a as usize]
                .cpu
                .total_cmp(&snap.procs[b as usize].cpu)
        }),
        C_MEM => by_key!(|&a: &u32| snap.procs[a as usize].private_ws),
        C_PEAK => by_key!(|&a: &u32| snap.procs[a as usize].peak_ws),
        C_COMMIT => by_key!(|&a: &u32| snap.procs[a as usize].commit),
        C_WS => by_key!(|&a: &u32| snap.procs[a as usize].working_set),
        C_THREADS => by_key!(|&a: &u32| snap.procs[a as usize].threads),
        C_HANDLES => by_key!(|&a: &u32| snap.procs[a as usize].handles),
        C_PRIO => by_key!(|&a: &u32| snap.procs[a as usize].base_priority),
        C_SESSION => by_key!(|&a: &u32| snap.procs[a as usize].session),
        C_START => by_key!(|&a: &u32| snap.procs[a as usize].create_time),
        C_READ => by_key!(|&a: &u32| snap.procs[a as usize].io_read_total),
        C_WRITE => by_key!(|&a: &u32| snap.procs[a as usize].io_write_total),
        C_FAULTS => v.sort_unstable_by(|&a, &b| {
            snap.procs[a as usize]
                .hard_faults_per_s
                .total_cmp(&snap.procs[b as usize].hard_faults_per_s)
        }),
        C_STATUS => by_key!(|&a: &u32| snap.procs[a as usize].state == RunState::Running),
        C_USER => by_key!(|&a: &u32| meta_str(meta, snap, a, |m| m.user.clone())),
        C_DESC => by_key!(|&a: &u32| meta_str(meta, snap, a, |m| m.description.clone())),
        C_PATH => by_key!(|&a: &u32| meta_str(meta, snap, a, |m| m.path.clone())),
        C_CMDLINE => by_key!(|&a: &u32| meta_str(meta, snap, a, |m| m.cmdline.clone())),
        C_ARCH => by_key!(|&a: &u32| {
            meta.get(snap.procs[a as usize].key())
                .map(|m| m.arch as u8)
                .unwrap_or(0)
        }),
        C_VIRT => by_key!(|&a: &u32| snap.procs[a as usize].virtual_size),
        C_PAGED => by_key!(|&a: &u32| snap.procs[a as usize].paged_pool),
        C_NPPOOL => by_key!(|&a: &u32| snap.procs[a as usize].nonpaged_pool),
        C_PFAULTS => by_key!(|&a: &u32| snap.procs[a as usize].page_faults),
        C_CPUTIME => by_key!(|&a: &u32| snap.procs[a as usize].cpu_time_100ns),
        C_IOREAD => v.sort_unstable_by(|&a, &b| {
            snap.procs[a as usize]
                .io_read_bps
                .total_cmp(&snap.procs[b as usize].io_read_bps)
        }),
        C_IOWRITE => v.sort_unstable_by(|&a, &b| {
            snap.procs[a as usize]
                .io_write_bps
                .total_cmp(&snap.procs[b as usize].io_write_bps)
        }),
        C_READ_OPS => by_key!(|&a: &u32| snap.procs[a as usize].read_ops),
        C_WRITE_OPS => by_key!(|&a: &u32| snap.procs[a as usize].write_ops),
        C_OTHER_OPS => by_key!(|&a: &u32| snap.procs[a as usize].other_ops),
        C_OTHER_BYTES => by_key!(|&a: &u32| snap.procs[a as usize].io_other_total),
        C_CYCLES => by_key!(|&a: &u32| snap.procs[a as usize].cycle_time),
        _ => by_key!(|&a: &u32| snap.procs[a as usize].name.to_ascii_lowercase()),
    }
    if desc {
        v.reverse();
    }
}

fn meta_str(
    meta: &MetaStore,
    snap: &Snapshot,
    idx: u32,
    pick: impl Fn(&crate::sys::meta::Meta) -> Option<Arc<str>>,
) -> String {
    meta.get(snap.procs[idx as usize].key())
        .and_then(|m| pick(&m))
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
}
