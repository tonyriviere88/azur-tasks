//! The Performance tab: a resource picker on the left, a detail view with
//! graphs and statistics on the right.

use super::fmt;
use super::graph::{self, Graph, Scale, Series};
use super::theme::Theme;
use azur_egui_theme::components::Tabs;
use azur_egui_theme::desktop;
use azur_egui_theme::tokens::space;
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
        // `space-3` between the two columns, off Azur's own scale. Neither of them
        // is a surface -- both are painted straight onto the canvas this tab shows
        // -- so what divides them is space between two groups of content, not a
        // seam. A seam is what welds two *panels* together, and the ones in this
        // window are the rail, the toolbar and the status bar.
        let detail_rect = Rect::from_min_max(
            Pos2::new(full.left() + list_w + space::S3, full.top()),
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
                        // A row in a listing, so it takes the window's row fills --
                        // the accent's surface ramp when selected, the one hover
                        // grey under the pointer -- and it is square, like every
                        // other row here.
                        if let Some(fill) = desktop::row_fill(t, active, resp.hovered()) {
                            p.rect_filled(r, desktop::structural_radius(), fill);
                        }
                        // The bar is `accent-mark`, not the resource's own colour:
                        // the fill under it is now an accent surface, and CPU's
                        // series colour is the same azure, so that bar would have
                        // been drawn in the colour of what it sits on. The
                        // thumbnail beside it is already in the resource's colour,
                        // which is what tells Disk 0 from Disk 1.
                        if active {
                            desktop::selection_bar(p, r, t);
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
                            fmt::elide_to_width(ui, title, &t.fonts.body_strong, avail),
                            t.fonts.body_strong.clone(),
                            t.text.primary,
                        );
                        p.text(
                            Pos2::new(tx, r.top() + 38.0),
                            Align2::LEFT_CENTER,
                            fmt::elide_to_width(ui, sub, &t.fonts.caption, avail),
                            t.fonts.caption.clone(),
                            t.text.secondary,
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

        // Two views of the same data, which is what Azur's `Tabs` is for: labels
        // over a rule, with an accent indicator under the selected one. On their
        // own row under the title rather than beside it -- the strip's rule spans
        // whatever it is given, and next to the title it ran through the
        // processor name.
        let mut which = usize::from(self.per_core);
        // The strip's own height, asked rather than assumed: `Tabs` is a body line
        // with `space-3` either side and a 1px rule under it, and the 32 that used to
        // be here left the graph below starting a point above that rule.
        let strip_h = azur_egui_theme::components::tabs_height();
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(
                    Pos2::new(rect.left(), y),
                    Vec2::new(rect.width(), strip_h),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Min)),
            |ui| {
                ui.add(Tabs::new(&mut which).tabs(["Overall", "Logical processors"]))
                    .on_hover_text("All logical processors combined, or one graph each");
            },
        );
        self.per_core = which == 1;
        y += strip_h + space::S2;

        let stats_h = 118.0;
        let graph_rect = Rect::from_min_max(
            Pos2::new(rect.left(), y),
            Pos2::new(rect.right(), rect.bottom() - stats_h),
        );

        if self.per_core && !snap.hist.per_core.is_empty() {
            let n = snap.hist.per_core.len();
            let gap = 4.0;
            let (cols, rows) = grid_shape(n, graph_rect.size(), gap);
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
                        t.fonts.caption.clone(),
                        t.text.tertiary,
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
            t.fonts.caption.clone(),
            t.text.secondary,
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
                (snap.mem.modified as f32, t.status.warning.gamma_multiply(0.8)),
                (snap.mem.standby as f32, t.mem.gamma_multiply(0.45)),
                // Free memory is the absence of a series, so it gets a neutral
                // surface rather than a hue of its own. `background-control`, which
                // is a surface: the hover rungs are three steps out of the ramp in
                // this window, and a segment of a chart is not hovering.
                (snap.mem.free as f32, t.bg.control),
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
            t.fonts.caption.clone(),
            t.text.secondary,
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
            t.fonts.caption.clone(),
            t.text.secondary,
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
                color: t.accent.default,
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
        legend_dot(ui, t, Pos2::new(right - send_w, ly), t.accent.default, "Send");
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
        t.text.primary,
    );
    p.text(
        Pos2::new(rect.left(), rect.top() + 26.0),
        Align2::LEFT_TOP,
        subtitle,
        t.fonts.caption.clone(),
        t.text.secondary,
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
        t.fonts.caption.clone(),
        t.text.tertiary,
    );
    p.text(
        rect.left_bottom() + Vec2::new(4.0, -2.0),
        Align2::LEFT_BOTTOM,
        bottom,
        t.fonts.caption.clone(),
        t.text.tertiary,
    );
    p.text(
        rect.right_bottom() + Vec2::new(-4.0, -2.0),
        Align2::RIGHT_BOTTOM,
        span,
        t.fonts.caption.clone(),
        t.text.tertiary,
    );
}

/// Pick `(cols, rows)` for `n` tiles filling `area`, gaps included.
///
/// The obvious `cols = ceil(sqrt(n))` gives square tiles only in a square area.
/// The per-core pane is landscape, so on a 24-thread machine it chose 5x5 --
/// leaving a hole in the last row *and* stretching every tile to nearly 2:1. Two
/// things decide instead:
///
/// * **Holes cost.** Every unfilled slot is a gap in the grid, so each one is
///   charged enough that a shape which tiles `n` exactly wins any close call.
/// * **Tile shape costs.** A tile should be a little wider than tall, which is
///   how a graph of the last 60 seconds reads. Judged on the log of the ratio so
///   that half as wide and twice as wide are penalised equally.
///
/// 24 threads in a landscape pane then comes out 6x4: no holes, tiles at 1.2:1.
fn grid_shape(n: usize, area: Vec2, gap: f32) -> (usize, usize) {
    /// Tile width over height that a sparkline wears best.
    const IDEAL_ASPECT: f32 = 1.25;
    /// Score added per empty slot. Worth roughly a 1.8x error in tile shape,
    /// which is what makes zero holes the default rather than a tiebreak.
    const HOLE_COST: f32 = 0.6;

    let mut best = (n.max(1), 1);
    let mut best_score = f32::INFINITY;
    for cols in 1..=n.max(1) {
        let rows = n.div_ceil(cols);
        let cw = (area.x - gap * (cols - 1) as f32) / cols as f32;
        let ch = (area.y - gap * (rows - 1) as f32) / rows as f32;
        if cw <= 1.0 || ch <= 1.0 {
            continue;
        }
        let holes = cols * rows - n;
        let score = (cw / ch / IDEAL_ASPECT).ln().abs() + HOLE_COST * holes as f32;
        if score < best_score {
            best_score = score;
            best = (cols, rows);
        }
    }
    best
}

/// A label/value list. Values are right-aligned within `width` so numbers line
/// up down the column.
fn stat_column(ui: &Ui, t: &Theme, origin: Pos2, width: f32, rows: &[(&str, &str)]) {
    let p = ui.painter();
    for (i, (label, value)) in rows.iter().enumerate() {
        let y = origin.y + i as f32 * 24.0;
        // Both halves in caption: a statistics grid is metadata, and at body size
        // the widest label ("Logical processors") and its value meet in the
        // middle of the column.
        let value_w = p
            .layout_no_wrap(value.to_string(), t.fonts.caption.clone(), t.text.primary)
            .size()
            .x;
        // The label is what gives way when a column is too narrow for both. A
        // shortened number reads as a different number; a shortened label is
        // still recognisable from its first few words, and the ellipsis says so.
        let room = (width - 12.0 - value_w - space::S3).max(0.0);
        p.text(
            Pos2::new(origin.x, y),
            Align2::LEFT_TOP,
            fmt::elide_to_width(ui, label, &t.fonts.caption, room),
            t.fonts.caption.clone(),
            t.text.secondary,
        );
        p.text(
            Pos2::new(origin.x + width - 12.0, y),
            Align2::RIGHT_TOP,
            *value,
            t.fonts.caption.clone(),
            t.text.primary,
        );
    }
}

/// Total width of a legend entry (swatch + gap + label), for right-aligning.
fn legend_width(ui: &Ui, t: &Theme, label: &str) -> f32 {
    14.0 + ui
        .painter()
        .layout_no_wrap(label.to_owned(), t.fonts.caption.clone(), Color32::WHITE)
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
        t.fonts.caption.clone(),
        t.text.secondary,
    );
}

/// Card titles come from `Arc<str>` in the snapshot but the entry tuple wants a
/// `&str` living as long as the frame; the snapshot outlives the frame, so this
/// just reborrows it.
fn leak_label(s: &std::sync::Arc<str>) -> &str {
    s
}

#[cfg(test)]
mod tests {
    use super::grid_shape;
    use egui::Vec2;

    /// The case this was written for: 24 threads used to land in a 5x5 grid with
    /// a hole in it, whatever shape the pane was.
    #[test]
    fn fills_every_slot_when_the_thread_count_allows_it() {
        // A maximized window: wide pane, so the grid goes wide too.
        assert_eq!(grid_shape(24, Vec2::new(2000.0, 1000.0), 4.0), (6, 4));
        // The default window: the pane is nearly square, so the same 24 tiles
        // turn a quarter -- still no hole, and still wider than tall each.
        assert_eq!(grid_shape(24, Vec2::new(520.0, 530.0), 4.0), (4, 6));
        for n in [2, 4, 6, 8, 12, 16, 20, 32, 64, 128] {
            let (c, r) = grid_shape(n, Vec2::new(2000.0, 1000.0), 4.0);
            assert_eq!(c * r, n, "{n} threads left {} slots empty", c * r - n);
        }
    }

    /// A prime thread count cannot tile a rectangle, so one hole is the best
    /// available -- but only one, and not at the cost of a 23:1 strip.
    #[test]
    fn spends_a_hole_rather_than_a_useless_shape() {
        let (c, r) = grid_shape(23, Vec2::new(2000.0, 1000.0), 4.0);
        assert_eq!((c, r), (6, 4));
        assert_eq!(c * r - 23, 1);
    }

    /// A pane too small to divide still has to name a grid rather than divide by
    /// zero or return nothing.
    #[test]
    fn survives_a_pane_with_no_room_in_it() {
        assert_eq!(grid_shape(1, Vec2::new(100.0, 100.0), 4.0), (1, 1));
        let (c, r) = grid_shape(24, Vec2::new(8.0, 8.0), 4.0);
        assert!(c >= 1 && r >= 1 && c * r >= 24);
    }
}
