//! The Performance tab: a resource picker on the left, a detail view with
//! graphs and statistics on the right.

use super::fmt;
use super::graph::{self, Graph, Scale, Series};
use super::theme::Theme;
use crate::sys::snapshot::{History, Snapshot};
use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Ui, Vec2};

/// Which resource the detail pane is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    Cpu,
    Memory,
    Disk(usize),
    Net(usize),
    Gpu(usize),
}

pub struct PerfTab {
    pub selected: Resource,
    /// CPU detail: show one graph per logical processor.
    pub per_core: bool,
}

impl PerfTab {
    pub fn new() -> Self {
        Self {
            selected: Resource::Cpu,
            per_core: false,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot) {
        let full = ui.available_rect_before_wrap();
        let list_w = 232.0f32.min(full.width() * 0.34);

        let list_rect = Rect::from_min_size(full.min, Vec2::new(list_w, full.height()));
        let detail_rect = Rect::from_min_max(
            Pos2::new(full.left() + list_w + 10.0, full.top()),
            full.max,
        );

        self.resource_list(ui, t, snap, list_rect);
        self.detail(ui, t, snap, detail_rect);
    }

    // -----------------------------------------------------------------
    // Left: resource picker
    // -----------------------------------------------------------------

    fn resource_list(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot, rect: Rect) {
        let mut entries: Vec<(Resource, &str, Color32, &History, f32, String)> = Vec::new();
        let mut buf = String::new();

        fmt::percent_sign(&mut buf, snap.cpu.total);
        entries.push((
            Resource::Cpu,
            "CPU",
            t.cpu,
            &snap.hist.cpu,
            100.0,
            format!("{}  {:.2} GHz", buf, snap.cpu_base_mhz as f32 / 1000.0),
        ));

        let mem_line = {
            let mut a = String::new();
            let mut b = String::new();
            fmt::bytes(&mut a, snap.mem.in_use);
            fmt::bytes(&mut b, snap.mem.total);
            format!("{a}/{b}  ({:.0}%)", snap.mem.percent())
        };
        entries.push((
            Resource::Memory,
            "Memory",
            t.mem,
            &snap.hist.mem,
            100.0,
            mem_line,
        ));

        for (i, d) in snap.disks.iter().enumerate() {
            let h = snap.hist.disk.get(i).unwrap_or(&snap.hist.cpu);
            entries.push((
                Resource::Disk(i),
                leak_label(&d.name),
                t.disk,
                h,
                100.0,
                format!("{:.0}%  active", d.active_pct),
            ));
        }

        for (i, n) in snap.nets.iter().enumerate() {
            let h = snap.hist.net_recv.get(i).unwrap_or(&snap.hist.cpu);
            let peak = graph::nice_ceil(
                snap.hist
                    .net_recv
                    .get(i)
                    .map(|x| x.max())
                    .unwrap_or(0.0)
                    .max(snap.hist.net_send.get(i).map(|x| x.max()).unwrap_or(0.0))
                    .max(1024.0),
            );
            let mut r = String::new();
            let mut s = String::new();
            fmt::bits_per_sec(&mut r, n.recv_bps);
            fmt::bits_per_sec(&mut s, n.send_bps);
            entries.push((
                Resource::Net(i),
                leak_label(&n.name),
                t.net,
                h,
                peak,
                if n.up {
                    format!("R: {r}  S: {s}")
                } else {
                    "Not connected".to_owned()
                },
            ));
        }

        for (i, g) in snap.gpus.iter().enumerate() {
            let h = snap.hist.gpu.get(i).unwrap_or(&snap.hist.cpu);
            entries.push((
                Resource::Gpu(i),
                leak_label(&g.name),
                t.gpu,
                h,
                100.0,
                format!("{:.0}%  utilization", g.utilization),
            ));
        }

        let card_h = 62.0;
        let scroll = egui::ScrollArea::vertical()
            .id_salt("perf-list")
            .auto_shrink([false, false])
            .max_height(rect.height());

        let mut clicked: Option<Resource> = None;
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect),
            |ui| {
                // `max_rect` bounds layout but not painting; without this the
                // cards would draw straight over the status bar.
                ui.set_clip_rect(rect);
                scroll.show(ui, |ui| {
                    for (res, title, color, hist, max, sub) in &entries {
                        let (r, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), card_h),
                            Sense::click(),
                        );
                        let active = *res == self.selected;
                        if resp.clicked() {
                            clicked = Some(*res);
                        }

                        let p = ui.painter();
                        let fill = if active {
                            t.surface_hi
                        } else if resp.hovered() {
                            t.surface
                        } else {
                            Color32::TRANSPARENT
                        };
                        if fill != Color32::TRANSPARENT {
                            p.rect_filled(r, t.radius_sm, fill);
                        }
                        // Accent stripe marks the current selection.
                        if active {
                            p.rect_filled(
                                Rect::from_min_size(
                                    r.left_top() + Vec2::new(0.0, 6.0),
                                    Vec2::new(3.0, r.height() - 12.0),
                                ),
                                CornerRadius::same(2),
                                *color,
                            );
                        }

                        // Thumbnail graph on the left, labels on the right.
                        let thumb = Rect::from_min_size(
                            r.left_top() + Vec2::new(10.0, 9.0),
                            Vec2::new(58.0, card_h - 18.0),
                        );
                        graph::sparkline(ui, t, thumb, hist, *color, *max, true);

                        // Disk and GPU names are long ("Disk 0 (SAMSUNG
                        // MZVLC2T0HBLD-00BH1)"). Elide by measured width rather
                        // than a character count so the ellipsis actually lands
                        // inside the card instead of being clipped away with the
                        // text it was supposed to replace.
                        let tx = thumb.right() + 10.0;
                        let avail = (r.right() - tx - 6.0).max(20.0);
                        p.text(
                            Pos2::new(tx, r.top() + 17.0),
                            Align2::LEFT_CENTER,
                            elide_to_width(ui, title, &t.fonts.medium, avail),
                            t.fonts.medium.clone(),
                            t.text,
                        );
                        p.text(
                            Pos2::new(tx, r.top() + 38.0),
                            Align2::LEFT_CENTER,
                            elide_to_width(ui, sub, &t.fonts.small, avail),
                            t.fonts.small.clone(),
                            t.text_dim,
                        );
                    }
                });
            },
        );

        if let Some(r) = clicked {
            self.selected = r;
        }

        // Keep the selection valid when a device disappears.
        let valid = match self.selected {
            Resource::Disk(i) => i < snap.disks.len(),
            Resource::Net(i) => i < snap.nets.len(),
            Resource::Gpu(i) => i < snap.gpus.len(),
            _ => true,
        };
        if !valid {
            self.selected = Resource::Cpu;
        }
    }

    // -----------------------------------------------------------------
    // Right: detail
    // -----------------------------------------------------------------

    fn detail(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot, rect: Rect) {
        match self.selected {
            Resource::Cpu => self.detail_cpu(ui, t, snap, rect),
            Resource::Memory => self.detail_mem(ui, t, snap, rect),
            Resource::Disk(i) => self.detail_disk(ui, t, snap, rect, i),
            Resource::Net(i) => self.detail_net(ui, t, snap, rect, i),
            Resource::Gpu(i) => self.detail_gpu(ui, t, snap, rect, i),
        }
    }

    fn detail_cpu(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot, rect: Rect) {
        let mut y = title(ui, t, rect, "CPU", &snap.cpu_name);

        // Toggle between the aggregate graph and one graph per logical CPU.
        // Laid out right-to-left so the group ends flush with the panel edge
        // instead of overflowing it.
        let toggle_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 260.0, rect.top() + 2.0),
            Vec2::new(260.0, 26.0),
        );
        let mut per_core = self.per_core;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(toggle_rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
            |ui| {
                // `frame_when_inactive(true)`: a selectable button draws no
                // frame until hovered, and a frame's stroke counts towards its
                // size, so without this the unselected one grows 2px under the
                // cursor and nudges its neighbour sideways.
                if ui
                    .add(
                        egui::Button::selectable(per_core, "Logical processors")
                            .frame_when_inactive(true),
                    )
                    .on_hover_text("One graph per logical processor")
                    .clicked()
                {
                    per_core = true;
                }
                if ui
                    .add(egui::Button::selectable(!per_core, "Overall").frame_when_inactive(true))
                    .on_hover_text("All logical processors combined")
                    .clicked()
                {
                    per_core = false;
                }
            },
        );
        self.per_core = per_core;

        let stats_h = 118.0;
        let graph_rect = Rect::from_min_max(
            Pos2::new(rect.left(), y),
            Pos2::new(rect.right(), rect.bottom() - stats_h),
        );

        if self.per_core && !snap.hist.per_core.is_empty() {
            // Grid of small graphs, sized so it stays roughly square.
            let n = snap.hist.per_core.len();
            let cols = (n as f32).sqrt().ceil().max(1.0) as usize;
            let rows = n.div_ceil(cols);
            let gap = 4.0;
            let cw = (graph_rect.width() - gap * (cols - 1) as f32) / cols as f32;
            let ch = (graph_rect.height() - gap * (rows.saturating_sub(1)) as f32) / rows as f32;
            for (i, h) in snap.hist.per_core.iter().enumerate() {
                let cx = i % cols;
                let cy = i / cols;
                let r = Rect::from_min_size(
                    Pos2::new(
                        graph_rect.left() + cx as f32 * (cw + gap),
                        graph_rect.top() + cy as f32 * (ch + gap),
                    ),
                    Vec2::new(cw, ch),
                );
                graph::sparkline(ui, t, r, h, t.cpu, 100.0, true);
                // Label each core, but only when the tile is big enough to read.
                if cw > 46.0 && ch > 26.0 {
                    ui.painter().text(
                        r.left_top() + Vec2::new(4.0, 3.0),
                        Align2::LEFT_TOP,
                        format!("{i}"),
                        t.fonts.small.clone(),
                        t.text_faint,
                    );
                }
            }
        } else {
            let series = [Series {
                hist: &snap.hist.cpu,
                color: t.cpu,
                fill: true,
            }];
            Graph::new(&series, Scale::Percent).paint(ui, t, graph_rect);
            axis_labels(ui, t, graph_rect, "100%", "0%", "60 seconds");
        }

        // --- statistics ---------------------------------------------------
        y = rect.bottom() - stats_h + 8.0;
        let mut b = String::new();
        fmt::uptime(&mut b, snap.cpu.uptime_secs);
        let uptime = b.clone();
        let mut util = String::new();
        fmt::percent_sign(&mut util, snap.cpu.total);
        let mut procs = String::new();
        fmt::integer(&mut procs, snap.cpu.processes as u64);
        let mut threads = String::new();
        fmt::integer(&mut threads, snap.cpu.threads as u64);
        let mut handles = String::new();
        fmt::integer(&mut handles, snap.cpu.handles as u64);

        let left = [
            ("Utilization", util.as_str()),
            ("Processes", procs.as_str()),
            ("Threads", threads.as_str()),
            ("Handles", handles.as_str()),
        ];
        let kernel = format!("{:.0}%", snap.cpu.kernel);
        let base = format!("{:.2} GHz", snap.cpu_base_mhz as f32 / 1000.0);
        let sockets = snap.cpu_sockets.to_string();
        let cores = snap.cpu_cores.to_string();
        let logical = snap.cpu_count.to_string();
        let virt = if snap.cpu_virtualization {
            "Enabled"
        } else {
            "Disabled"
        };
        let right = [
            ("Kernel time", kernel.as_str()),
            ("Base speed", base.as_str()),
            ("Sockets", sockets.as_str()),
            ("Cores", cores.as_str()),
        ];
        let right2 = [
            ("Logical processors", logical.as_str()),
            ("Virtualization", virt),
            ("Up time", uptime.as_str()),
        ];

        let col_w = (rect.width() / 3.0).min(280.0);
        stat_column(ui, t, Pos2::new(rect.left(), y), col_w, &left);
        stat_column(ui, t, Pos2::new(rect.left() + col_w, y), col_w, &right);
        stat_column(
            ui,
            t,
            Pos2::new(rect.left() + col_w * 2.0, y),
            col_w,
            &right2,
        );
    }

    fn detail_mem(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot, rect: Rect) {
        let mut total = String::new();
        fmt::bytes(&mut total, snap.mem.total);
        let y = title(ui, t, rect, "Memory", &format!("{total} total"));

        let stats_h = 150.0;
        let bar_h = 46.0;
        let graph_rect = Rect::from_min_max(
            Pos2::new(rect.left(), y),
            Pos2::new(rect.right(), rect.bottom() - stats_h - bar_h),
        );
        let series = [Series {
            hist: &snap.hist.mem,
            color: t.mem,
            fill: true,
        }];
        Graph::new(&series, Scale::Percent).paint(ui, t, graph_rect);
        axis_labels(ui, t, graph_rect, &total, "0", "60 seconds");

        // --- composition bar ----------------------------------------------
        let bar_rect = Rect::from_min_size(
            Pos2::new(rect.left(), graph_rect.bottom() + 12.0),
            Vec2::new(rect.width(), 20.0),
        );
        ui.painter().text(
            Pos2::new(rect.left(), graph_rect.bottom() + 4.0),
            Align2::LEFT_TOP,
            "Memory composition",
            t.fonts.small.clone(),
            t.text_dim,
        );
        let in_use_only = snap
            .mem
            .in_use
            .saturating_sub(snap.mem.modified);
        graph::segmented_bar(
            ui,
            t,
            Rect::from_min_size(
                bar_rect.min + Vec2::new(0.0, 14.0),
                Vec2::new(bar_rect.width(), 20.0),
            ),
            &[
                (in_use_only as f32, t.mem),
                (snap.mem.modified as f32, t.warn.gamma_multiply(0.8)),
                (snap.mem.standby as f32, t.mem.gamma_multiply(0.45)),
                // Free memory is the absence of a series, so it gets a neutral
                // surface rather than a hue of its own.
                (snap.mem.free as f32, t.surface_hi),
            ],
        );

        // --- statistics ---------------------------------------------------
        let sy = rect.bottom() - stats_h + 14.0;
        let mut in_use = String::new();
        fmt::bytes(&mut in_use, snap.mem.in_use);
        let mut avail = String::new();
        fmt::bytes(&mut avail, snap.mem.available);
        let mut cached = String::new();
        fmt::bytes(&mut cached, snap.mem.cached);
        let mut paged = String::new();
        fmt::bytes(&mut paged, snap.mem.paged_pool);
        let mut nonpaged = String::new();
        fmt::bytes(&mut nonpaged, snap.mem.nonpaged_pool);
        let mut c1 = String::new();
        let mut c2 = String::new();
        fmt::bytes(&mut c1, snap.mem.committed);
        fmt::bytes(&mut c2, snap.mem.commit_limit);
        let committed = format!("{c1} / {c2}");
        let mut free = String::new();
        fmt::bytes(&mut free, snap.mem.free);
        let mut modified = String::new();
        fmt::bytes(&mut modified, snap.mem.modified);
        let mut standby = String::new();
        fmt::bytes(&mut standby, snap.mem.standby);

        let col_w = (rect.width() / 3.0).min(280.0);
        stat_column(
            ui,
            t,
            Pos2::new(rect.left(), sy),
            col_w,
            &[
                ("In use", in_use.as_str()),
                ("Available", avail.as_str()),
                ("Committed", committed.as_str()),
                ("Cached", cached.as_str()),
            ],
        );
        stat_column(
            ui,
            t,
            Pos2::new(rect.left() + col_w, sy),
            col_w,
            &[
                ("Paged pool", paged.as_str()),
                ("Non-paged pool", nonpaged.as_str()),
                ("Free", free.as_str()),
            ],
        );
        stat_column(
            ui,
            t,
            Pos2::new(rect.left() + col_w * 2.0, sy),
            col_w,
            &[
                ("Modified", modified.as_str()),
                ("Standby", standby.as_str()),
            ],
        );
    }

    fn detail_disk(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot, rect: Rect, i: usize) {
        let Some(d) = snap.disks.get(i) else { return };
        let mut cap = String::new();
        if d.capacity > 0 {
            fmt::bytes(&mut cap, d.capacity);
        } else {
            cap.push('—');
        }
        let y = title(ui, t, rect, &d.name, &format!("Capacity: {cap}"));

        let stats_h = 118.0;
        let avail_h = rect.bottom() - stats_h - y - 26.0;
        let gh = (avail_h / 2.0).max(60.0);

        // Graph 1: active time.
        let g1 = Rect::from_min_size(Pos2::new(rect.left(), y + 14.0), Vec2::new(rect.width(), gh));
        ui.painter().text(
            Pos2::new(rect.left(), y),
            Align2::LEFT_TOP,
            "Active time",
            t.fonts.small.clone(),
            t.text_dim,
        );
        let empty = History::default();
        let h_active = snap.hist.disk.get(i).unwrap_or(&empty);
        let s1 = [Series {
            hist: h_active,
            color: t.disk,
            fill: true,
        }];
        Graph::new(&s1, Scale::Percent).paint(ui, t, g1);
        axis_labels(ui, t, g1, "100%", "0%", "60 seconds");

        // Graph 2: transfer rate, autoscaled.
        let g2_top = g1.bottom() + 26.0;
        let g2 = Rect::from_min_size(
            Pos2::new(rect.left(), g2_top),
            Vec2::new(rect.width(), (rect.bottom() - stats_h - g2_top).max(50.0)),
        );
        let h_rate = snap.hist.disk_rate.get(i).unwrap_or(&empty);
        let s2 = [Series {
            hist: h_rate,
            color: t.disk.gamma_multiply(0.75),
            fill: true,
        }];
        // 1 MB/s floor keeps an idle disk's graph from magnifying noise.
        let max = Graph::new(
            &s2,
            Scale::Auto {
                floor: 1024.0 * 1024.0,
            },
        )
        .paint(ui, t, g2);
        let mut top = String::new();
        fmt::bytes(&mut top, max as u64);
        top.push_str("/s");
        ui.painter().text(
            Pos2::new(rect.left(), g2_top - 22.0),
            Align2::LEFT_TOP,
            "Disk transfer rate",
            t.fonts.small.clone(),
            t.text_dim,
        );
        axis_labels(ui, t, g2, &top, "0", "60 seconds");

        // --- statistics ---------------------------------------------------
        let sy = rect.bottom() - stats_h + 14.0;
        let active = format!("{:.0}%", d.active_pct);
        let resp = format!("{:.1} ms", d.avg_response_ms);
        let mut read = String::new();
        fmt::bytes(&mut read, d.read_bps as u64);
        read.push_str("/s");
        let mut write = String::new();
        fmt::bytes(&mut write, d.write_bps as u64);
        write.push_str("/s");

        let col_w = (rect.width() / 2.0).min(300.0);
        stat_column(
            ui,
            t,
            Pos2::new(rect.left(), sy),
            col_w,
            &[
                ("Active time", active.as_str()),
                ("Average response time", resp.as_str()),
            ],
        );
        stat_column(
            ui,
            t,
            Pos2::new(rect.left() + col_w, sy),
            col_w,
            &[
                ("Read speed", read.as_str()),
                ("Write speed", write.as_str()),
                ("Capacity", cap.as_str()),
            ],
        );
    }

    fn detail_net(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot, rect: Rect, i: usize) {
        let Some(n) = snap.nets.get(i) else { return };
        let mut link = String::new();
        fmt::link_speed(&mut link, n.link_speed);
        let y = title(
            ui,
            t,
            rect,
            &n.name,
            &if n.up {
                format!("Connected — {link}")
            } else {
                "Not connected".to_owned()
            },
        );

        let stats_h = 108.0;
        let graph_rect = Rect::from_min_max(
            Pos2::new(rect.left(), y),
            Pos2::new(rect.right(), rect.bottom() - stats_h),
        );

        let empty = History::default();
        let recv = snap.hist.net_recv.get(i).unwrap_or(&empty);
        let send = snap.hist.net_send.get(i).unwrap_or(&empty);
        let series = [
            Series {
                hist: recv,
                color: t.net,
                fill: true,
            },
            // Send is line-only so the two series stay legible when they cross.
            Series {
                hist: send,
                color: t.accent,
                fill: false,
            },
        ];
        // 1 Mbps floor (125 000 B/s) matches Task Manager's minimum scale.
        let max = Graph::new(&series, Scale::Auto { floor: 125_000.0 }).paint(ui, t, graph_rect);
        let mut top = String::new();
        fmt::bits_per_sec(&mut top, max);
        axis_labels(ui, t, graph_rect, &top, "0", "60 seconds");

        // Legend, in the graph's top-right corner: the top-left holds the axis
        // maximum and the bottom-right the time span, so this is the only corner
        // that stays free.
        let ly = graph_rect.top() + 4.0;
        let send_w = legend_width(ui, t, "Send");
        let recv_w = legend_width(ui, t, "Receive");
        let gap = 14.0;
        let right = graph_rect.right() - 8.0;
        legend_dot(ui, t, Pos2::new(right - send_w, ly), t.accent, "Send");
        legend_dot(
            ui,
            t,
            Pos2::new(right - send_w - gap - recv_w, ly),
            t.net,
            "Receive",
        );

        let sy = rect.bottom() - stats_h + 14.0;
        let mut r = String::new();
        fmt::bits_per_sec(&mut r, n.recv_bps);
        let mut s = String::new();
        fmt::bits_per_sec(&mut s, n.send_bps);

        let col_w = (rect.width() / 2.0).min(300.0);
        stat_column(
            ui,
            t,
            Pos2::new(rect.left(), sy),
            col_w,
            &[("Receive", r.as_str()), ("Send", s.as_str())],
        );
        stat_column(
            ui,
            t,
            Pos2::new(rect.left() + col_w, sy),
            col_w,
            &[
                ("Link speed", link.as_str()),
                ("State", if n.up { "Up" } else { "Down" }),
            ],
        );
    }

    fn detail_gpu(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot, rect: Rect, i: usize) {
        let Some(g) = snap.gpus.get(i) else { return };
        let mut vram = String::new();
        if g.dedicated_total > 0 {
            fmt::bytes(&mut vram, g.dedicated_total);
        } else {
            vram.push('—');
        }
        let y = title(ui, t, rect, &g.name, &format!("Dedicated memory: {vram}"));

        let stats_h = 108.0;
        let graph_rect = Rect::from_min_max(
            Pos2::new(rect.left(), y),
            Pos2::new(rect.right(), rect.bottom() - stats_h),
        );
        let empty = History::default();
        let h = snap.hist.gpu.get(i).unwrap_or(&empty);
        let series = [Series {
            hist: h,
            color: t.gpu,
            fill: true,
        }];
        Graph::new(&series, Scale::Percent).paint(ui, t, graph_rect);
        axis_labels(ui, t, graph_rect, "100%", "0%", "60 seconds");

        let sy = rect.bottom() - stats_h + 14.0;
        let util = format!("{:.0}%", g.utilization);
        let mut used = String::new();
        fmt::bytes(&mut used, g.dedicated_used);
        let mut shared = String::new();
        fmt::bytes(&mut shared, g.shared_used);
        let ded = if g.dedicated_total > 0 {
            format!("{used} / {vram}")
        } else {
            used.clone()
        };

        let col_w = (rect.width() / 2.0).min(300.0);
        stat_column(
            ui,
            t,
            Pos2::new(rect.left(), sy),
            col_w,
            &[
                ("Utilization", util.as_str()),
                ("Dedicated memory", ded.as_str()),
            ],
        );
        stat_column(
            ui,
            t,
            Pos2::new(rect.left() + col_w, sy),
            col_w,
            &[("Shared memory", shared.as_str())],
        );
    }
}

// ---------------------------------------------------------------------------
// Shared chrome
// ---------------------------------------------------------------------------

/// Draw a heading and return the y coordinate content should start at.
fn title(ui: &Ui, t: &Theme, rect: Rect, name: &str, subtitle: &str) -> f32 {
    let p = ui.painter();
    p.text(
        Pos2::new(rect.left(), rect.top()),
        Align2::LEFT_TOP,
        name,
        egui::FontId::new(19.0, egui::FontFamily::Name("semibold".into())),
        t.text,
    );
    p.text(
        Pos2::new(rect.left(), rect.top() + 26.0),
        Align2::LEFT_TOP,
        subtitle,
        t.fonts.small.clone(),
        t.text_dim,
    );
    rect.top() + 52.0
}

/// Corner labels around a graph, matching Task Manager's axis annotations.
fn axis_labels(ui: &Ui, t: &Theme, rect: Rect, top: &str, bottom: &str, span: &str) {
    let p = ui.painter();
    p.text(
        rect.left_top() + Vec2::new(4.0, 2.0),
        Align2::LEFT_TOP,
        top,
        t.fonts.small.clone(),
        t.text_faint,
    );
    p.text(
        rect.left_bottom() + Vec2::new(4.0, -2.0),
        Align2::LEFT_BOTTOM,
        bottom,
        t.fonts.small.clone(),
        t.text_faint,
    );
    p.text(
        rect.right_bottom() + Vec2::new(-4.0, -2.0),
        Align2::RIGHT_BOTTOM,
        span,
        t.fonts.small.clone(),
        t.text_faint,
    );
}

/// A label/value list. Values are right-aligned within `width` so numbers line
/// up down the column.
fn stat_column(ui: &Ui, t: &Theme, origin: Pos2, width: f32, rows: &[(&str, &str)]) {
    let p = ui.painter();
    for (i, (label, value)) in rows.iter().enumerate() {
        let y = origin.y + i as f32 * 24.0;
        p.text(
            Pos2::new(origin.x, y),
            Align2::LEFT_TOP,
            *label,
            t.fonts.small.clone(),
            t.text_dim,
        );
        p.text(
            Pos2::new(origin.x + width - 14.0, y),
            Align2::RIGHT_TOP,
            *value,
            t.fonts.body.clone(),
            t.text,
        );
    }
}

/// Total width of a legend entry (swatch + gap + label), for right-aligning.
fn legend_width(ui: &Ui, t: &Theme, label: &str) -> f32 {
    14.0 + ui
        .painter()
        .layout_no_wrap(label.to_owned(), t.fonts.small.clone(), Color32::WHITE)
        .size()
        .x
}

fn legend_dot(ui: &Ui, t: &Theme, pos: Pos2, color: Color32, label: &str) {
    let p = ui.painter();
    p.rect_filled(
        Rect::from_min_size(pos + Vec2::new(0.0, 4.0), Vec2::new(9.0, 3.0)),
        CornerRadius::same(1),
        color,
    );
    p.text(
        pos + Vec2::new(14.0, 0.0),
        Align2::LEFT_TOP,
        label,
        t.fonts.small.clone(),
        t.text_dim,
    );
}

/// Card titles come from `Arc<str>` in the snapshot but the entry tuple wants a
/// `&str` living as long as the frame; the snapshot outlives the frame, so this
/// just reborrows it.
fn leak_label(s: &std::sync::Arc<str>) -> &str {
    s
}

/// Shorten `s` with a trailing ellipsis until it fits `max_w` pixels.
///
/// Binary search over the character count, so a long device name costs a handful
/// of measurements rather than one per character.
fn elide_to_width(ui: &Ui, s: &str, font: &egui::FontId, max_w: f32) -> String {
    let measure = |text: &str| -> f32 {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
    };

    if measure(s) <= max_w {
        return s.to_owned();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let mut cand: String = chars[..mid].iter().collect();
        cand.push('…');
        if measure(&cand) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push('…');
    out
}
