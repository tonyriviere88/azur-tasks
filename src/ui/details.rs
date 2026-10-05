//! The Details tab: one flat row per process, with the full column set.

use super::fmt;
use super::list::ListTab;
use super::table::{Align, Column, Table};
use super::theme::Theme;
use azur_egui_theme::components::{ContextMenu, Text, TextColor, TextVariant};
use crate::sys::meta::MetaStore;
use crate::sys::snapshot::{ProcKey, RunState, Snapshot};
use crate::sys::util::format_filetime;
use egui::Ui;
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
    pub list: ListTab<ProcKey>,
    order: Vec<u32>,
    /// Display position per process from the last unfrozen rebuild, keyed by
    /// identity rather than index so it survives processes appearing/exiting.
    ///
    /// Stays here rather than in [`ListTab`]: what a freeze *means* differs per tab
    /// — this one pins a flat order, the Processes tab pins a tree with section
    /// headers and non-process rows in it — and only the flag itself is shared.
    rank: rustc_hash::FxHashMap<ProcKey, u32>,
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
            list: ListTab::new("details", table),
            order: Vec::new(),
            rank: rustc_hash::FxHashMap::default(),
        }
    }

    pub fn mark_dirty(&mut self) {
        self.list.mark_dirty();
    }

    /// Drop the selection, for Escape.
    pub fn clear_selection(&mut self) {
        self.list.clear_selection();
    }

    /// Select a process and scroll it into view (used by "Go to details").
    pub fn reveal(&mut self, key: ProcKey) {
        self.list.reveal(key);
    }

    fn rebuild(&mut self, snap: &Snapshot, meta: &MetaStore, filter: &str) {
        self.order.clear();
        // Parsed once rather than `pid.to_string()` per row: a bare number in the
        // box is a PID lookup, and formatting six hundred of them to compare against
        // it is six hundred allocations to answer one question.
        let pid = filter.parse::<u32>().ok();
        for (i, p) in snap.procs.iter().enumerate() {
            if !filter.is_empty() {
                let m = meta.get(p.key());
                let hit = fmt::contains_ci(&p.name, filter)
                    || pid == Some(p.pid)
                    || m.as_ref().is_some_and(|m| {
                        [&m.description, &m.company, &m.path, &m.user, &m.cmdline]
                            .iter()
                            .any(|f| f.as_ref().is_some_and(|s| fmt::contains_ci(s, filter)))
                    });
                if !hit {
                    continue;
                }
            }
            self.order.push(i as u32);
        }

        if self.list.frozen {
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
                self.list.table.sort_by,
                self.list.table.sort_desc,
            );
            self.rank.clear();
            self.rank.reserve(self.order.len());
            for (pos, &i) in self.order.iter().enumerate() {
                self.rank.insert(snap.procs[i as usize].key(), pos as u32);
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
        self.list.freeze_with_ctrl(ui);

        if self.list.stale(snap.generation, filter) {
            self.rebuild(snap, meta, filter);
            let Self { list, order, .. } = self;
            list.built(snap.generation, filter, order.len(), |r| {
                Some(snap.procs[order[r] as usize].key())
            });
        }
        self.list.header(ui, t);

        // Destructured rather than moved out of `self`: the row closure is handed the
        // column ids and the buffer, so the only thing left for it to borrow is this
        // tab's own data. See `ui::list`.
        let Self { list, order, .. } = self;
        let out = list.body(ui, t, order.len(), |rp, vis, buf| {
                let p = &snap.procs[order[rp.row] as usize];
                let m = meta.get(p.key());
                // Walk the visible columns in order; `slot` indexes the laid-out
                // cell rects, which only cover the visible columns.
                for (slot, &cid) in vis.iter().enumerate() {
                    match cid {
                        C_NAME => rp.text(slot, &p.name, t.text.primary, Align::Left),
                        C_PID => {
                            fmt::integer(buf, p.pid as u64);
                            rp.text_font(slot, buf, t.text.secondary, Align::Right, &t.fonts.mono);
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
                            fmt::percent(buf, p.cpu);
                            rp.text(slot, buf, t.load_text(p.cpu), Align::Right);
                        }
                        C_MEM => {
                            rp.heat(
                                slot,
                                p.private_ws as f32 / (2.0 * 1024.0 * 1024.0 * 1024.0),
                                t.mem,
                            );
                            fmt::mem_mb(buf, p.private_ws);
                            rp.text(slot, buf, t.text.primary, Align::Right);
                        }
                        C_PEAK => {
                            fmt::mem_mb(buf, p.peak_ws);
                            rp.text(slot, buf, t.text.secondary, Align::Right);
                        }
                        C_COMMIT => {
                            fmt::mem_mb(buf, p.commit);
                            rp.text(slot, buf, t.text.secondary, Align::Right);
                        }
                        C_WS => {
                            fmt::mem_mb(buf, p.working_set);
                            rp.text(slot, buf, t.text.secondary, Align::Right);
                        }
                        C_THREADS => {
                            fmt::integer(buf, p.threads as u64);
                            rp.text(slot, buf, t.text.secondary, Align::Right);
                        }
                        C_HANDLES => {
                            fmt::integer(buf, p.handles as u64);
                            rp.text(slot, buf, t.text.secondary, Align::Right);
                        }
                        C_PRIO => {
                            fmt::integer(buf, p.base_priority as u64);
                            rp.text(slot, buf, t.text.secondary, Align::Right);
                        }
                        C_ARCH => rp.text(
                            slot,
                            m.as_ref().map(|m| m.arch.label()).unwrap_or("—"),
                            t.text.secondary,
                            Align::Left,
                        ),
                        C_SESSION => {
                            fmt::integer(buf, p.session as u64);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_START => {
                            let s = format_filetime(p.create_time, true);
                            rp.text_font(slot, &s, t.text.tertiary, Align::Left, &t.fonts.mono);
                        }
                        C_READ => {
                            fmt::bytes(buf, p.io_read_total);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_WRITE => {
                            fmt::bytes(buf, p.io_write_total);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_FAULTS => {
                            fmt::percent(buf, p.hard_faults_per_s);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
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
                            fmt::mem_mb(buf, p.virtual_size);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_PAGED => {
                            fmt::bytes(buf, p.paged_pool);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_NPPOOL => {
                            fmt::bytes(buf, p.nonpaged_pool);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_PFAULTS => {
                            fmt::integer(buf, p.page_faults as u64);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_CPUTIME => {
                            fmt::cpu_time(buf, p.cpu_time_100ns);
                            rp.text_font(slot, buf, t.text.secondary, Align::Right, &t.fonts.mono);
                        }
                        C_IOREAD => {
                            fmt::rate_mbs(buf, p.io_read_bps);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_IOWRITE => {
                            fmt::rate_mbs(buf, p.io_write_bps);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_READ_OPS => {
                            fmt::integer(buf, p.read_ops);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_WRITE_OPS => {
                            fmt::integer(buf, p.write_ops);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_OTHER_OPS => {
                            fmt::integer(buf, p.other_ops);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_OTHER_BYTES => {
                            fmt::bytes(buf, p.io_other_total);
                            rp.text(slot, buf, t.text.tertiary, Align::Right);
                        }
                        C_CYCLES => {
                            fmt::integer(buf, p.cycle_time);
                            rp.text_font(slot, buf, t.text.tertiary, Align::Right, &t.fonts.mono);
                        }
                        _ => {}
                    }
                }
            });

        // The selection is a process key rather than a row, so a click or an arrow
        // key has to be turned back into one -- otherwise the next sample, which
        // rebuilds the order, would carry the selection along with the row index
        // instead of keeping it on the process.
        let Self { list, order, .. } = self;
        list.settle(ui, &out, order.len(), |r| {
            order.get(r).map(|&i| snap.procs[i as usize].key())
        });

        let target = out
            .context_row
            .and_then(|r| self.order.get(r).copied())
            .or_else(|| {
                self.list
                    .selected
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
