//! The Summary tab: the whole machine on one screen.
//!
//! Two questions, in the order a reader asks them — *is anything loaded?*, and
//! *what is loading it?* The band of cards across the top answers the first, one
//! card per resource: the headline figure, the last minute of it, and the line of
//! detail the Performance tab keeps in its statistics column. The rankings under
//! them answer the second.
//!
//! Nothing here is measured here. Every figure is one the Performance, Sensors or
//! Processes tab already shows, and every card is a way *into* the tab it came
//! from — a resource card selects that resource on Performance, a ranking row
//! reveals that process on Processes. A summary that cannot be left is a dead end,
//! and one that measured the machine a second way would eventually disagree with
//! the page it is summarising.
//!
//! ## What the rankings count
//!
//! Subtree totals, over the rows that head their own entry: exactly what a folded
//! row shows on the Processes tab, and the same partition of the machine — every
//! process is inside exactly one of them, counted once. "Google Chrome, 24%" is
//! the answer a reader wants; forty renderers at 0.6% each is the same fact,
//! arranged so nothing can be read off it.
//!
//! ## Rate, never saturation, when devices are added up
//!
//! The disk and network cards are the only place this page aggregates across
//! devices, and what they add up is a *rate* — bytes and bits per second. Two
//! disks at 50% active are not one disk at 100%, a machine with three adapters has
//! no single "network utilization", and a percentage that could exceed 100 reads
//! as a bug. The saturation each device does have goes in the detail line, where
//! it belongs to one device and means something.

use std::borrow::Cow;
use std::fmt::Write as _;

use super::fmt;
use super::graph;
use super::icons::IconCache;
use super::performance::Resource;
use super::theme::Theme;
use super::Action;
use crate::sys::meta::MetaStore;
use crate::sys::snapshot::{History, Proc, Snapshot, HISTORY_LEN, NO_SENSORS};
use azur_egui_theme::components::{galley_on_baseline, ink_baseline, row_baseline, ContextMenu};
use azur_egui_theme::desktop;
use azur_egui_theme::tokens::typography::{LINE_BODY, LINE_CAPTION, LINE_TITLE_SMALL};
use azur_egui_theme::tokens::{radius, row, space};
use egui::{Align2, Color32, CornerRadius, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

/// The gap between two cards, and between one band of the page and the next.
/// `space-3`, the same air the Performance tab keeps between its two columns.
const GAP: f32 = space::S3;

/// A resource card at its shortest: `space-3` of padding, a headline line, a trace
/// with room to be one, a detail line. It grows from here — see [`page`].
const CARD_MIN_H: f32 = 118.0;
/// And at its tallest. A card that took every spare pixel of a tall window would be
/// a graph with a caption, and the rankings below it are what a reader came for.
const CARD_MAX_H: f32 = 200.0;
/// Narrowest a resource card is allowed to get before the band wraps to fewer of
/// them. Below this the label and the figure meet in the middle of the card.
const CARD_MIN_W: f32 = 218.0;

/// The "Top processes" heading.
const SECTION_H: f32 = 24.0;
/// Narrowest a ranking card the three-across arrangement will accept. Below it
/// they stack instead — never two and a spare, which leaves one card half the
/// width of the window with a hole beside it.
const LIST_MIN_W: f32 = 216.0;
/// A ranking card's own chrome: padding, its heading, the rule, and the air under
/// it. What is left over is rows.
const LIST_CHROME: f32 = space::S3 * 2.0 + LINE_BODY + space::S2;

/// Rows a ranking will grow to when the window has the room.
const TOP_MAX: usize = 12;
/// Rows it shows even when the window does not — below three a ranking is not one,
/// so the page scrolls rather than shrinking further.
const TOP_MIN: usize = 3;

/// A series with nothing in it, for an adapter that appeared between ticks and has
/// no history slot yet.
static NO_HISTORY: History = History::EMPTY;

pub struct SummaryTab {
    /// Scroll offset, kept so switching tabs does not jump back to the top. Only
    /// ever non-zero on a window too small for the page to fit, which is the one
    /// thing the scroll area is there for.
    scroll: f32,
}

impl SummaryTab {
    pub fn new() -> Self {
        Self { scroll: 0.0 }
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        snap: &Snapshot,
        meta: &MetaStore,
        icons: &mut IconCache,
        action: &mut Option<Action>,
    ) {
        // Measured before the scroll area, because this is the height the page lays
        // itself out against: `available_height` *inside* one is a function of the
        // content added so far, not of the window looking at it.
        let view = ui.available_height();
        let out = egui::ScrollArea::vertical()
            .id_salt("summary")
            .auto_shrink([false, false])
            .vertical_scroll_offset(self.scroll)
            .show(ui, |ui| {
                azur_egui_theme::scrollbar(ui, t);
                page(ui, t, snap, meta, icons, view, action);
            });
        self.scroll = out.state.offset.y;
    }
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn page(
    ui: &mut Ui,
    t: &Theme,
    snap: &Snapshot,
    meta: &MetaStore,
    icons: &mut IconCache,
    view: f32,
    action: &mut Option<Action>,
) {
    let ctx = ui.ctx().clone();
    let avail = ui.available_width();
    // This page measures its own bands against the viewport, down to the pixel, so
    // egui must not also insert its `space-3` between them: four allocations and the
    // spaces between them would come to 40px more than the arithmetic below spent,
    // which is a scrollbar on a window the page was told fits.
    ui.spacing_mut().item_spacing.y = 0.0;

    // --- how the page divides the window ------------------------------------
    let cards = metrics(snap, t);
    // The band wraps *balanced* rather than filling each row before starting the
    // next: six cards in a window with room for five is three and three, not five
    // and a spare with four holes beside it.
    let fit = ((avail + GAP) / (CARD_MIN_W + GAP)).floor().max(1.0) as usize;
    let card_rows = cards.len().div_ceil(fit).max(1);
    let per_row = cards.len().div_ceil(card_rows).max(1);
    let card_w = (avail - GAP * per_row.saturating_sub(1) as f32) / per_row as f32;

    // Three side by side or one at a time, and nothing in between: three parallel
    // rankings are read *across*, which is the whole reason they are three cards
    // rather than one list, and two of them beside a third on its own row is
    // neither arrangement.
    let per_list = if avail >= LIST_MIN_W * 3.0 + GAP * 2.0 {
        Rank::ALL.len()
    } else {
        1
    };
    let list_w = (avail - GAP * (per_list - 1) as f32) / per_list as f32;
    let bands = Rank::ALL.len().div_ceil(per_list);
    let heading = SECTION_H + space::S1;

    // The rankings are served first, out of what is left of the *viewport* once the
    // cards have their minimum: a summary that ended in a band of bare canvas would
    // be a page that had run out of things to say while the window still had room
    // for them. On a window too short even for the floor, the scroll area gives.
    let list_room = ((view - card_rows as f32 * (CARD_MIN_H + GAP) - heading - GAP * bands as f32)
        / bands as f32)
        .max(0.0);
    let n = (((list_room - LIST_CHROME) / row::DENSE).floor() as i32)
        .clamp(TOP_MIN as i32, TOP_MAX as i32) as usize;
    let list_h = LIST_CHROME + n as f32 * row::DENSE;

    // Whatever the rankings did not need goes to the traces, because height is worth
    // something to a graph and nothing to a row: a ranking is 24px lines however
    // tall the window is, and a minute of CPU is easier to read the taller it is.
    let card_h = ((view - heading - bands as f32 * (list_h + GAP)) / card_rows as f32 - GAP)
        .clamp(CARD_MIN_H, CARD_MAX_H);

    // --- the resource band -------------------------------------------------
    for chunk in cards.chunks(per_row) {
        let (band, _) = ui.allocate_exact_size(Vec2::new(avail, card_h), Sense::hover());
        for (i, m) in chunk.iter().enumerate() {
            let rect = Rect::from_min_size(
                Pos2::new(band.left() + i as f32 * (card_w + GAP), band.top()),
                Vec2::new(card_w, card_h),
            );
            // Keyed by what the card is rather than by where it sits, so a disk
            // appearing does not hand one card's hover state to another.
            let resp = ui.interact(rect, Id::new(("summary-metric", &m.label)), Sense::click());
            paint_metric(ui, t, rect, m, resp.hovered());
            if resp.clicked() {
                *action = Some(Action::ShowResource(m.res));
            }
            resp.on_hover_text(format!("{}\nShow it on the Performance tab", m.subject));
        }
        ui.add_space(GAP);
    }

    // --- the rankings ------------------------------------------------------
    let (head, _) = ui.allocate_exact_size(Vec2::new(avail, SECTION_H), Sense::hover());
    {
        let p = ui.painter();
        let base = row_baseline(p, &t.fonts.subtitle, head.top(), SECTION_H);
        galley_on_baseline(
            p,
            head.left(),
            base,
            p.layout_no_wrap(
                "Top processes".to_owned(),
                t.fonts.subtitle.clone(),
                t.text.primary,
            ),
        );
        // Said out loud, because otherwise a browser at 24% reads as one tab gone
        // wrong. It is the figure the Processes tab shows for the same folded row.
        // Dropped rather than elided on a narrow window: half of this sentence says
        // something else.
        let note = p.layout_no_wrap(
            "an application and everything it started count as one".to_owned(),
            t.fonts.caption.clone(),
            t.text.tertiary,
        );
        if note.size().x < head.width() * 0.6 {
            galley_on_baseline(p, head.right() - note.size().x, base, note);
        }
    }
    ui.add_space(space::S1);

    let tops = rank_all(snap);
    for (b, chunk) in Rank::ALL.chunks(per_list).enumerate() {
        let (band, _) = ui.allocate_exact_size(Vec2::new(avail, list_h), Sense::hover());
        for (i, r) in chunk.iter().enumerate() {
            let rect = Rect::from_min_size(
                Pos2::new(band.left() + i as f32 * (list_w + GAP), band.top()),
                Vec2::new(list_w, list_h),
            );
            paint_rank(
                ui,
                t,
                rect,
                *r,
                &tops[b * per_list + i],
                n,
                snap,
                meta,
                icons,
                &ctx,
                action,
            );
        }
        ui.add_space(GAP);
    }
}

// ---------------------------------------------------------------------------
// Resource cards
// ---------------------------------------------------------------------------

/// One resource card, built for this frame.
struct Metric<'a> {
    /// The card's name. Borrowed for the four fixed ones; owned only where an
    /// index has to be spliced in ("GPU 1").
    label: Cow<'a, str>,
    /// The headline, in the units the reader should see.
    value: String,
    value_color: Color32,
    /// The line under the trace.
    detail: String,
    /// Borrowed for a resource the snapshot keeps a ring for; owned for the two
    /// that are sums across devices.
    hist: Cow<'a, History>,
    max: f32,
    color: Color32,
    /// The Performance tab's view of the same resource — where a click goes.
    res: Resource,
    /// What that click will open, named. The aggregate cards land on the busiest
    /// device, and saying which one is what makes that honest.
    ///
    /// The name only: every card's tooltip ends with the same sentence, so that
    /// half is written where the tooltip is built rather than at all five of the
    /// places a card is built.
    subject: Cow<'a, str>,
}

fn metrics<'a>(snap: &'a Snapshot, t: &Theme) -> Vec<Metric<'a>> {
    let s = &snap.sensors;
    let mut out: Vec<Metric<'a>> = Vec::with_capacity(4 + snap.gpus.len());

    // --- CPU ---------------------------------------------------------------
    // The live clock where a sensor could be read and the nominal figure
    // otherwise, then the package temperature and power — the Sensors tab's
    // readings, on the card of the part they belong to rather than as a second wall
    // of cards. Hardware that has no such sensor contributes nothing rather than a
    // zero, which is that tab's rule as well.
    let mhz = if s.cpu_mhz > 0 {
        s.cpu_mhz
    } else {
        snap.cpu_base_mhz
    };
    let mut detail = format!("{:.2} GHz", mhz as f32 / 1000.0);
    if let Some(c) = s.cpu_temp_c {
        let _ = write!(detail, " · {c:.0} °C");
    }
    if let Some(w) = s.cpu_power_w {
        let _ = write!(detail, " · {w:.0} W");
    }
    out.push(Metric {
        label: Cow::Borrowed("CPU"),
        value: fmt::percent_sign_of(snap.cpu.total),
        value_color: t.load_text(snap.cpu.total),
        detail,
        hist: Cow::Borrowed(&snap.hist.cpu),
        max: 100.0,
        color: t.cpu,
        res: Resource::Cpu,
        subject: Cow::Borrowed(&snap.cpu_name),
    });

    // --- memory ------------------------------------------------------------
    let mut in_use = String::new();
    fmt::bytes(&mut in_use, snap.mem.in_use);
    let mut total = String::new();
    fmt::bytes(&mut total, snap.mem.total);
    let mut avail = String::new();
    fmt::bytes(&mut avail, snap.mem.available);
    let pct = snap.mem.percent();
    out.push(Metric {
        label: Cow::Borrowed("Memory"),
        value: fmt::percent_sign_of(pct),
        value_color: t.load_text(pct),
        detail: format!("{in_use} / {total} · {avail} free"),
        hist: Cow::Borrowed(&snap.hist.mem),
        max: 100.0,
        color: t.mem,
        res: Resource::Memory,
        subject: Cow::Owned(format!("{total} of physical memory")),
    });

    // --- disks -------------------------------------------------------------
    if !snap.disks.is_empty() {
        let rate: f32 = snap.disks.iter().map(|d| d.read_bps + d.write_bps).sum();
        let hist = sum_hist(snap.hist.disk_rate.iter());
        let busiest = busiest(&snap.disks, |d| d.active_pct);
        let active = snap.disks[busiest].active_pct;
        let mut value = String::new();
        fmt::bytes(&mut value, rate as u64);
        value.push_str("/s");
        out.push(Metric {
            label: Cow::Borrowed("Disk"),
            value,
            // No threshold colour: a transfer rate has no ceiling to be a fraction
            // of, and there is no rate at which "12 MB/s" is bad news. The figure
            // that *can* be too high is the saturation on the line below.
            value_color: t.text.primary,
            detail: if snap.disks.len() == 1 {
                format!("{active:.0}% active")
            } else {
                format!("{} disks · {active:.0}% busiest", snap.disks.len())
            },
            // 1 MB/s floor, as on the Performance tab: without it an idle disk's
            // trace magnifies noise into a mountain range.
            max: graph::nice_ceil(hist.max().max(1024.0 * 1024.0)),
            hist: Cow::Owned(hist),
            color: t.disk,
            res: Resource::Disk(busiest),
            subject: Cow::Borrowed(&snap.disks[busiest].name),
        });
    }

    // --- network -----------------------------------------------------------
    if !snap.nets.is_empty() {
        let recv: f32 = snap.nets.iter().map(|n| n.recv_bps).sum();
        let send: f32 = snap.nets.iter().map(|n| n.send_bps).sum();
        // Both directions in one trace, for the same reason the disks are one: the
        // card answers "is the link busy", and the split is on the line below it.
        let hist = sum_hist(snap.hist.net_recv.iter().chain(snap.hist.net_send.iter()));
        let busiest = busiest(&snap.nets, |n| n.recv_bps + n.send_bps);
        let mut value = String::new();
        fmt::bits_per_sec(&mut value, recv + send);
        let mut r = String::new();
        fmt::bits_per_sec(&mut r, recv);
        let mut sd = String::new();
        fmt::bits_per_sec(&mut sd, send);
        out.push(Metric {
            label: Cow::Borrowed("Network"),
            value,
            value_color: t.text.primary,
            detail: if snap.nets.iter().any(|n| n.up) {
                format!("R {r} · S {sd}")
            } else {
                "Not connected".to_owned()
            },
            // 1 Mbps floor, matching the Performance tab's network graph.
            max: graph::nice_ceil(hist.max().max(125_000.0)),
            hist: Cow::Owned(hist),
            color: t.net,
            res: Resource::Net(busiest),
            subject: Cow::Borrowed(&snap.nets[busiest].name),
        });
    }

    // --- graphics ----------------------------------------------------------
    // One card per adapter rather than a sum: there are one or two of them, they
    // are not interchangeable, and a machine's discrete and integrated GPUs at 50%
    // each is not one GPU at 100%.
    for (i, g) in snap.gpus.iter().enumerate() {
        let sens = (g.sensors != NO_SENSORS)
            .then(|| snap.sensors.gpus.get(g.sensors as usize))
            .flatten();
        let mut detail = String::new();
        if g.dedicated_total > 0 {
            let mut used = String::new();
            let mut total = String::new();
            fmt::bytes(&mut used, g.dedicated_used);
            fmt::bytes(&mut total, g.dedicated_total);
            detail = format!("{used} / {total}");
        } else if g.dedicated_used > 0 {
            fmt::bytes(&mut detail, g.dedicated_used);
        }
        if let Some(c) = sens.and_then(|x| x.temp_c) {
            if !detail.is_empty() {
                detail.push_str(" · ");
            }
            let _ = write!(detail, "{c:.0} °C");
        }
        // Named, where the edge temperature above is not: two bare °C figures on one
        // line leave the reader to work out which is which. Present only where the
        // device publishes a junction reading -- an AMD feature -- so on every other
        // board the segment is absent rather than empty, as on the Sensors tab.
        if let Some(c) = sens.and_then(|x| x.hotspot_c) {
            if !detail.is_empty() {
                detail.push_str(" · ");
            }
            let _ = write!(detail, "{c:.0} °C");
        }
        // An integrated adapter reports neither dedicated memory nor a temperature
        // of its own, so the line says which adapter this is rather than nothing.
        if detail.is_empty() {
            detail = g.name.to_string();
        }
        out.push(Metric {
            label: if snap.gpus.len() == 1 {
                Cow::Borrowed("GPU")
            } else {
                Cow::Owned(format!("GPU {i}"))
            },
            value: fmt::percent_sign_of(g.utilization),
            value_color: t.load_text(g.utilization),
            detail,
            hist: Cow::Borrowed(snap.hist.gpu.get(i).unwrap_or(&NO_HISTORY)),
            max: 100.0,
            color: t.gpu,
            res: Resource::Gpu(i),
            subject: Cow::Borrowed(&g.name),
        });
    }

    out
}

/// Add several series together, sample by sample.
///
/// The per-device rings are pushed on the same tick, so their samples line up and
/// their newest sample is the same instant — which is why they are aligned to the
/// *right*: a device that appeared half a minute ago has fewer samples, not older
/// ones. Sixty additions per device, on a page that lays out a thousand glyphs.
fn sum_hist<'a>(parts: impl Iterator<Item = &'a History> + Clone) -> History {
    let n = parts.clone().map(History::len).max().unwrap_or(0);
    let mut acc = [0.0f32; HISTORY_LEN];
    for h in parts {
        let off = n - h.len();
        for (i, v) in h.iter().enumerate() {
            acc[off + i] += v;
        }
    }
    let mut out = History::default();
    for v in &acc[..n] {
        out.push(*v);
    }
    out
}

/// The index of the device with the largest reading — the *first* of them when
/// nothing is happening, which is the ordinary case and where "Disk 0" is a better
/// answer than "whichever disk was enumerated last".
fn busiest<T>(devices: &[T], reading: impl Fn(&T) -> f32) -> usize {
    let mut best = (0usize, f32::NEG_INFINITY);
    for (i, d) in devices.iter().enumerate() {
        let v = reading(d);
        if v > best.1 {
            best = (i, v);
        }
    }
    best.0
}

fn paint_metric(ui: &Ui, t: &Theme, card: Rect, m: &Metric<'_>, hovered: bool) {
    let p = ui.painter();
    let corner = CornerRadius::same(radius::MEDIUM);
    // This card is a control as well as a surface — it opens the Performance tab —
    // so it takes the window's card-hover fill *and* an outline under the pointer,
    // where the Sensors tab's cards, which only move, take the outline alone.
    p.rect_filled(
        card,
        corner,
        if hovered { t.bg.card_hover } else { t.bg.card },
    );
    if hovered {
        p.rect_stroke(
            card,
            corner,
            Stroke::new(1.0, t.stroke.strong),
            StrokeKind::Inside,
        );
    }

    let pad = space::S3;
    // Label and figure on one baseline, set by the figure: it is the principal text
    // on the line, and topping the two boxes instead leaves a 12px label floating
    // above an 18px number.
    let baseline = row_baseline(p, &t.fonts.title_small, card.top() + pad, LINE_TITLE_SMALL);
    let value = p.layout_no_wrap(m.value.clone(), t.fonts.title_small.clone(), m.value_color);
    // The label is what gives way when the card is too narrow for both. A shortened
    // figure reads as a different figure; a shortened label is still recognisable.
    let room = (card.width() - pad * 2.0 - value.size().x - space::S3).max(0.0);
    let label = p.layout_no_wrap(
        fmt::elide_to_width(ui, &m.label, &t.fonts.caption, room),
        t.fonts.caption.clone(),
        t.text.secondary,
    );
    galley_on_baseline(p, card.left() + pad, baseline, label);
    galley_on_baseline(p, card.right() - pad - value.size().x, baseline, value);

    let plot = Rect::from_min_max(
        Pos2::new(
            card.left() + pad,
            card.top() + pad + LINE_TITLE_SMALL + space::S2,
        ),
        Pos2::new(
            card.right() - pad,
            card.bottom() - pad - LINE_CAPTION - space::S2,
        ),
    );
    if plot.width() > 8.0 && plot.height() > 8.0 {
        graph::sparkline(ui, t, plot, &m.hist, m.color, m.max.max(1.0), true);
    }

    let detail_base = row_baseline(
        p,
        &t.fonts.caption,
        card.bottom() - pad - LINE_CAPTION,
        LINE_CAPTION,
    );
    let detail = p.layout_no_wrap(
        fmt::elide_to_width(ui, &m.detail, &t.fonts.caption, card.width() - pad * 2.0),
        t.fonts.caption.clone(),
        t.text.tertiary,
    );
    galley_on_baseline(p, card.left() + pad, detail_base, detail);
}

// ---------------------------------------------------------------------------
// Rankings
// ---------------------------------------------------------------------------

/// Which resource a ranking is about.
#[derive(Clone, Copy)]
enum Rank {
    Cpu,
    Memory,
    Disk,
}

impl Rank {
    /// Left to right, in the order the Processes tab's own columns are in.
    const ALL: [Rank; 3] = [Self::Cpu, Self::Memory, Self::Disk];

    fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Disk => "Disk",
        }
    }

    /// The subtree total this ranks by — the figure a folded row shows on the
    /// Processes tab.
    fn value(self, p: &Proc) -> f32 {
        match self {
            Self::Cpu => p.sub_cpu,
            Self::Memory => p.sub_private_ws as f32,
            Self::Disk => p.sub_disk_bps,
        }
    }

    /// The resource's own series colour, so a ranking and the trace above it are
    /// drawn in one hue — the rule the tables' magnitude tint follows.
    fn color(self, t: &Theme) -> Color32 {
        match self {
            Self::Cpu => t.cpu,
            Self::Memory => t.mem,
            Self::Disk => t.disk,
        }
    }

    /// Below this the reading prints as zero, and a ranking of zeroes is a list of
    /// processes doing nothing.
    fn floor(self) -> f32 {
        match self {
            Self::Cpu => 0.05,
            Self::Memory => 1.0,
            // What `fmt::rate_mbs` shows as `0`: a twentieth of a megabyte a second.
            Self::Disk => 0.05 * 1024.0 * 1024.0,
        }
    }

    /// Full scale for the bars: the leader, but never less than what it takes for a
    /// reading to be worth noticing. Against the leader alone, an idle machine's
    /// busiest process — three tenths of a percent — would draw a full-width bar
    /// and read as a machine under load.
    fn scale(self, leader: f32, snap: &Snapshot) -> f32 {
        match self {
            Self::Cpu => leader.max(10.0),
            // An eighth of the machine's RAM: on 32 GB, a 4 GB process fills the
            // bar. Floored again for a machine whose total could not be read.
            Self::Memory => leader.max((snap.mem.total as f32 / 8.0).max(1e9)),
            Self::Disk => leader.max(5.0 * 1024.0 * 1024.0),
        }
    }

    fn text(self, buf: &mut String, v: f32) {
        match self {
            // The Processes tab's own CPU column, plus the sign its column header
            // would otherwise be carrying.
            Self::Cpu => {
                fmt::percent(buf, v);
                buf.push('%');
            }
            Self::Memory => fmt::bytes(buf, v as u64),
            Self::Disk => fmt::rate_mbs(buf, v),
        }
    }

    /// Severity is the number's job. A share of the whole processor is the one
    /// figure here with a ceiling to be measured against, so it is the one that
    /// takes the window's load colours.
    fn value_color(self, t: &Theme, v: f32) -> Color32 {
        match self {
            Self::Cpu => t.load_text(v),
            _ => t.text.primary,
        }
    }

    /// What the card says when nothing is happening — which is the ordinary state
    /// of the disk ranking on a machine at rest.
    fn idle(self) -> &'static str {
        match self {
            Self::Cpu => "Nothing using the processor",
            Self::Memory => "No processes",
            Self::Disk => "No disk activity",
        }
    }
}

/// What to call an application in a ranking: the version resource's
/// `FileDescription` — "Visual Studio Code", "Antimalware Service Executable" — and
/// the image name where the executable carries none.
///
/// Deliberately *not* what the Processes tab calls the same row, which is the
/// window title with the application promoted to the front. That column is 300px
/// wide and identifies one process; these cards are a third of that and rank
/// *applications*. A title also carries whatever is open in it, so
/// "Visual Studio Code - main.rs - myproject" spends two thirds of the row on
/// something the ranking is not about — and reads as a different kind of thing next
/// to the row below it, whose process owns no window and is called "MsMpEng.exe".
///
/// The description is the application's own name for itself, it is the same length
/// whatever is open, and it is what the Publisher column and the Details tab
/// already show.
fn app_label<'a>(p: &'a Proc, desc: Option<&'a str>) -> &'a str {
    match desc {
        Some(d) if !d.is_empty() => d,
        _ => &p.name,
    }
}

/// One ranked entry: where the process is in this snapshot, and what it scored.
#[derive(Clone, Copy, Default)]
struct Entry {
    /// Index into [`Snapshot::procs`]. Built from, and used against, the one
    /// snapshot this frame is painting.
    idx: u32,
    value: f32,
}

/// The largest few of a population, without sorting the rest of it.
///
/// A ranking shows six of six hundred processes; a sort would order the five
/// hundred and ninety-four nobody reads. This keeps twelve slots and touches them
/// only when a value beats the smallest one in them — which, on a machine where a
/// handful of processes do all the work, is almost never.
struct Top {
    rows: [Entry; TOP_MAX],
    len: usize,
}

impl Top {
    fn new() -> Self {
        Self {
            rows: [Entry::default(); TOP_MAX],
            len: 0,
        }
    }

    fn offer(&mut self, idx: u32, value: f32) {
        if self.len == TOP_MAX && value <= self.rows[TOP_MAX - 1].value {
            return;
        }
        let at = self.rows[..self.len]
            .iter()
            .position(|e| value > e.value)
            .unwrap_or(self.len);
        if self.len < TOP_MAX {
            self.len += 1;
        }
        // Shift the tail down over the slot, then fill it. What rotates to the front
        // is either the entry falling off the end or the slot just claimed; either
        // way the next line overwrites it.
        self.rows[at..self.len].rotate_right(1);
        self.rows[at] = Entry { idx, value };
    }

    fn rows(&self) -> &[Entry] {
        &self.rows[..self.len]
    }
}

/// Fill the three rankings in one pass over the process table.
///
/// One pass rather than three, and no cache behind it: the table is six hundred
/// rows on a busy machine and the loop body is three comparisons, which is less
/// than this page spends laying out a single line of text.
fn rank_all(snap: &Snapshot) -> [Top; 3] {
    let mut out = [Top::new(), Top::new(), Top::new()];
    for (i, p) in snap.procs.iter().enumerate() {
        // Only rows that head their own entry, so an application and its helpers
        // are one line — and every process is inside exactly one of them, counted
        // once. See `sampler::rollup`.
        if !p.group_head {
            continue;
        }
        for (k, r) in Rank::ALL.iter().enumerate() {
            let v = r.value(p);
            if v >= r.floor() {
                out[k].offer(i as u32, v);
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn paint_rank(
    ui: &Ui,
    t: &Theme,
    card: Rect,
    r: Rank,
    top: &Top,
    n: usize,
    snap: &Snapshot,
    meta: &MetaStore,
    icons: &mut IconCache,
    ctx: &egui::Context,
    action: &mut Option<Action>,
) {
    let pad = space::S3;
    {
        let p = ui.painter();
        p.rect_filled(card, CornerRadius::same(radius::MEDIUM), t.bg.card);

        let base = row_baseline(p, &t.fonts.body_strong, card.top() + pad, LINE_BODY);
        galley_on_baseline(
            p,
            card.left() + pad,
            base,
            p.layout_no_wrap(
                r.title().to_owned(),
                t.fonts.body_strong.clone(),
                t.text.primary,
            ),
        );
        // A rule rather than air: the rows below each carry a coloured bar, and
        // without a line the heading reads as the first of them.
        p.hline(
            egui::Rangef::new(card.left() + pad, card.right() - pad),
            card.top() + pad + LINE_BODY,
            Stroke::new(1.0, t.stroke.subtle),
        );
    }

    let mut y = card.top() + pad + LINE_BODY + space::S2;
    let rows = top.rows();
    if rows.is_empty() {
        ui.painter().text(
            Pos2::new(card.left() + pad, y + row::DENSE * 0.5),
            Align2::LEFT_CENTER,
            r.idle(),
            t.fonts.caption.clone(),
            t.text.tertiary,
        );
        return;
    }

    let scale = r.scale(rows[0].value, snap);
    let mut value_buf = String::new();
    for (i, e) in rows.iter().take(n).enumerate() {
        let p = &snap.procs[e.idx as usize];
        let key = p.key();
        let rect = Rect::from_min_size(
            Pos2::new(card.left() + pad, y),
            Vec2::new(card.width() - pad * 2.0, row::DENSE),
        );
        y += row::DENSE;

        // Slot rather than identity, because a ranking is a set of positions: the
        // process in the top row changes as the machine does, and a hover that
        // followed the process instead would jump between rows under a still
        // pointer.
        let resp = ui.interact(rect, Id::new(("summary-top", r.title(), i)), Sense::click());

        // The bar *is* the ranking: its width is the value against the leader, in
        // the resource's own series colour and the same wash the fill under a trace
        // uses, so the two halves of this page agree about what a resource looks
        // like. The figure beside it is the measurement; the bar is what lets the
        // eye rank six of them without reading any.
        let frac = (e.value / scale).clamp(0.0, 1.0);
        if frac > 0.0 {
            ui.painter().rect_filled(
                Rect::from_min_size(rect.min, Vec2::new(rect.width() * frac, rect.height())),
                CornerRadius::ZERO,
                graph::fill_of(r.color(t)),
            );
        }
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, desktop::hover_fill(t));
        }

        let m = meta.get(key);
        let path = m.as_ref().and_then(|x| x.path.clone());
        let tex = icons.get(ctx, path.as_ref());
        let paint = ui.painter().with_clip_rect(rect);
        let icon = Rect::from_min_size(
            Pos2::new(rect.left() + space::S2, rect.center().y - 8.0),
            Vec2::splat(16.0),
        );
        paint.add(egui::Shape::image(
            tex,
            icon,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        ));

        // A row with a glyph on it, so `ink_baseline`: a line box centred in the row
        // hangs its text below an icon centred in the same row.
        let base = ink_baseline(&paint, &t.fonts.body, rect.top(), row::DENSE);
        r.text(&mut value_buf, e.value);
        let value = paint.layout_no_wrap(
            value_buf.clone(),
            t.fonts.body.clone(),
            r.value_color(t, e.value),
        );
        let name_x = icon.right() + space::S3;
        let room = (rect.right() - name_x - value.size().x - space::S3).max(0.0);
        let desc = m.as_ref().and_then(|x| x.description.as_deref());
        let label = paint.layout_no_wrap(
            fmt::elide_to_width(ui, app_label(p, desc), &t.fonts.body, room),
            t.fonts.body.clone(),
            t.text.primary,
        );
        galley_on_baseline(&paint, name_x, base, label);
        galley_on_baseline(&paint, rect.right() - value.size().x, base, value);

        // Clicking goes to the process in the list it came out of, and the menu is
        // the one the Processes tab's own rows carry — so nothing on this page is a
        // second way of doing something.
        if resp.clicked() {
            *action = Some(Action::GoToProcess(key));
        }
        let pid = p.pid;
        let image = p.name.clone();
        ContextMenu::new(&resp).show(ctx, |ui| {
            super::process_context_menu(ui, t, pid, &image, key, path.as_deref(), action);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(top: &Top) -> Vec<f32> {
        top.rows().iter().map(|e| e.value).collect()
    }

    /// The ranking has to come out ordered whatever order it was offered in, and
    /// each value has to keep the process it came from.
    #[test]
    fn keeps_the_largest_offers_in_order() {
        let mut top = Top::new();
        for (i, v) in [3.0, 1.0, 5.0, 2.0, 4.0].into_iter().enumerate() {
            top.offer(i as u32, v);
        }
        assert_eq!(values(&top), vec![5.0, 4.0, 3.0, 2.0, 1.0]);
        assert_eq!(top.rows()[0].idx, 2);
        assert_eq!(top.rows()[4].idx, 1);
    }

    /// Ascending offers displace the tail every single time, which is the case that
    /// walks the whole array -- and the one that would overflow it.
    #[test]
    fn never_holds_more_than_its_slots() {
        let mut top = Top::new();
        let n = TOP_MAX as u32 * 3;
        for i in 0..n {
            top.offer(i, i as f32);
        }
        assert_eq!(top.rows().len(), TOP_MAX);
        let v = values(&top);
        assert_eq!(v[0], (n - 1) as f32);
        assert!(v.windows(2).all(|w| w[0] >= w[1]), "{v:?}");
    }

    /// A value that cannot make the cut must not disturb the ones that did.
    #[test]
    fn ignores_an_offer_that_cannot_place() {
        let mut top = Top::new();
        for i in 0..TOP_MAX as u32 {
            top.offer(i, 100.0 - i as f32);
        }
        let before = values(&top);
        top.offer(999, 0.5);
        assert_eq!(values(&top), before);
    }

    /// Sums are aligned on the newest sample, not the oldest: a device that
    /// appeared half a minute ago has fewer samples, not older ones.
    #[test]
    fn sums_series_aligned_to_the_newest_sample() {
        let mut long = History::default();
        for v in [1.0, 1.0, 1.0, 1.0] {
            long.push(v);
        }
        let mut short = History::default();
        short.push(10.0);
        short.push(10.0);

        let sum = sum_hist([&long, &short].into_iter());
        assert_eq!(sum.iter().collect::<Vec<_>>(), vec![1.0, 1.0, 11.0, 11.0]);
    }

    #[test]
    fn sums_nothing_into_an_empty_series() {
        assert_eq!(sum_hist(std::iter::empty()).len(), 0);
    }

    /// The first device wins a tie, so an idle machine's card points at Disk 0
    /// rather than at whichever disk happened to be enumerated last.
    #[test]
    fn picks_the_first_device_when_none_is_busy() {
        assert_eq!(busiest(&[0.0, 0.0, 0.0], |v| *v), 0);
        assert_eq!(busiest(&[0.0, 3.0, 3.0], |v| *v), 1);
        assert_eq!(busiest(&[1.0, 9.0, 2.0], |v| *v), 1);
        assert_eq!(busiest(&[0.0f32; 0], |v| *v), 0);
    }
}
