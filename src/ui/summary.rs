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
//! as a bug.
//!
//! The detail line under the disk card is the one place saturation *is* added up,
//! because a reader asking "how hard is the storage working" wants one figure and
//! the busiest disk is not it. That total can pass 100%, so the line prints the
//! number of disks beside it: over "2 disks", 160% is arithmetic rather than an
//! impossibility.
//!
//! ## One device, chosen, where several cannot be compared
//!
//! Graphics is the case that neither rule fits. Two adapters cannot be added up — a
//! discrete and an integrated GPU at 50% each are not one GPU at 100% — and they are
//! not interchangeable, so no one of them is "the" reading. Nor is a card each the
//! answer: on any machine that has both, the integrated one is idle, and it would
//! spend a sixth of the band saying so. So the GPU card is about *one* adapter, the
//! dedicated one until the reader says otherwise, and carries the selector for
//! saying it. That choice is the only thing on this page that is remembered.
//!
//! ## Held under Ctrl
//!
//! The rankings are ordered by a reading that moves every second, and their rows are
//! clickable — which is the same problem the two table tabs have, and it takes the
//! same answer: while Ctrl is held, each ranking keeps the rows it had in the places
//! they were in, so the row under the pointer is still there when the button goes
//! down. The figures keep updating; only the order is held, and the status bar says
//! so. See [`Hold`].
//!
//! ## The one card that is not about the machine
//!
//! Frame pacing is about whatever is in the foreground rather than about a device,
//! and it is the question the GPU card beside it cannot answer: an adapter at 60%
//! while frames arrive late is a different fault from one pinned at 100%. It is also
//! the one card whose figures the Performance tab has no page for, so it is the one
//! card that opens the Sensors tab instead — and the one card that is in the band
//! whether or not it has anything to show, because "Needs administrator" is an
//! answer and a card that quietly is not there is not.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::Arc;

use super::fmt;
use super::graph;
use super::icons::IconCache;
use super::performance::Resource;
use super::theme::Theme;
use super::Action;
use crate::sys::meta::MetaStore;
use crate::sys::snapshot::{
    GpuStats, History, Proc, ProcKey, Snapshot, HISTORY_LEN, NO_SENSORS,
};
use rustc_hash::FxHashMap;
use azur_egui_theme::components::{
    galley_on_baseline, ink_baseline, row_baseline, ContextMenu, Menu, MenuItem,
};
use azur_egui_theme::desktop;
use azur_egui_theme::icons::chevron_down;
use azur_egui_theme::tokens::typography::{LINE_BODY, LINE_CAPTION, LINE_TITLE_SMALL};
use azur_egui_theme::tokens::{radius, row, space};
use egui::{
    Align2, Color32, CornerRadius, Galley, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};

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

/// The chevron on the GPU card's device selector: `.chevron { width: 12px }`, the
/// box a `Select` trigger gives it -- a selector inside a card and one in a form are
/// the same affordance, so they are the same size.
const CARET: f32 = 12.0;
/// And the pill it shares with the label: the caption's line box with `space-1` of
/// air above and below, which is a comfortable target without being a second row.
const PILL_H: f32 = LINE_CAPTION + space::S1 * 2.0;

/// What a card shows in place of a figure nothing is measuring — the em dash the
/// Sensors tab shows for the same thing, so a reading that is absent looks absent on
/// both pages rather than looking like a zero on one of them.
const MISSING: &str = "—";

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
    /// Which graphics adapter the GPU card is about, by the name the snapshot gives
    /// it. Empty until the reader picks one from the card itself, and then the
    /// default is [`dedicated_gpu`] rather than anything stored.
    ///
    /// The *name*, not an index, for the reason the Sensors tab's arrangement is
    /// keyed by name as well: a driver update renumbers the adapter list and
    /// unplugging an external GPU shortens it, and a stored index would then point
    /// the card at a different device without saying so. Public because `App` owns
    /// the settings file, as it does that arrangement.
    pub gpu: String,
    /// The Ctrl hold over the rankings.
    hold: Hold,
}

/// What Ctrl holds: the order each ranking's rows were in before it came on.
///
/// The Summary tab's counterpart to
/// [`ListTab::freeze_with_ctrl`](super::list::ListTab::freeze_with_ctrl), and the
/// same bargain — the readings go on moving, the order does not.
#[derive(Default)]
struct Hold {
    /// Whether Ctrl is down.
    on: bool,
    /// Display position of each ranked process, one map per ranking, from the last
    /// frame the hold was off.
    ///
    /// Keyed by [`ProcKey`] rather than by index into `Snapshot::procs`, for the
    /// reason the Processes tab gives for its own copy of this map: those indices
    /// shift as processes come and go, so an index-based hold would quietly start
    /// showing a different process in the place the reader was aiming at.
    rank: [FxHashMap<ProcKey, u32>; Rank::ALL.len()],
}

impl SummaryTab {
    pub fn new() -> Self {
        Self {
            scroll: 0.0,
            gpu: String::new(),
            hold: Hold::default(),
        }
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
        // Read out here rather than down in the rankings, because it is the page that
        // has to be told and because this is the one place that knows the frame's
        // input. No text field to compete with: the search box is not on this tab.
        self.hold.on = ui.input(|i| i.modifiers.ctrl);
        // Recorded rather than written straight back: the page holds a borrow of
        // `gpu` for the whole frame -- the card's own label came out of it -- and a
        // menu entry leaves a choice behind rather than something to act on while
        // the band is still being painted. The same split as `action`.
        let mut chosen = None;
        let out = egui::ScrollArea::vertical()
            .id_salt("summary")
            .auto_shrink([false, false])
            .vertical_scroll_offset(self.scroll)
            .show(ui, |ui| {
                azur_egui_theme::scrollbar(ui, t);
                page(
                    ui,
                    t,
                    snap,
                    meta,
                    icons,
                    view,
                    &self.gpu,
                    &mut chosen,
                    &mut self.hold,
                    action,
                );
            });
        self.scroll = out.state.offset.y;
        if let Some(name) = chosen {
            self.gpu = name;
        }
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
    gpu: &str,
    chosen: &mut Option<String>,
    hold: &mut Hold,
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
    let cards = metrics(snap, t, gpu);
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
            // Laid out before anything is painted, because the selector's rect is
            // what the card's own click has to give way to.
            let head = head(ui, t, rect, m);
            // Registered *after* the card, which is what puts it on top: egui breaks
            // a tie between two widgets under the pointer in favour of the last one,
            // so the pointer on the selector is not on the card at all -- the card
            // cannot be clicked through it and does not offer its tooltip over it.
            //
            // A fixed id, not one built from the label: the label reads "GPU 1" one
            // frame and "GPU 0" the next, and the menu's open state hangs off this.
            let picker = head
                .pick
                .map(|(pill, _)| ui.interact(pill, Id::new("summary-gpu-pick"), Sense::click()));
            let hover = Hover {
                card: resp.hovered(),
                pick: picker.as_ref().is_some_and(egui::Response::hovered),
            };
            paint_metric(ui, t, rect, m, &head, hover);
            if resp.clicked() {
                *action = Some(m.dest.action());
            }
            resp.on_hover_text(format!(
                "{}\nShow it on the {} tab",
                m.subject,
                m.dest.tab()
            ));
            // What the selector does, in full: the menu names the adapters and a
            // click on one leaves the name in `chosen`, which the tab applies once
            // the frame is over.
            if let (Some(trigger), Some(pick)) = (&picker, &m.pick) {
                Menu::new(trigger).show(&ctx, |ui| {
                    for (i, name) in pick.names.iter().enumerate() {
                        // The full name each -- "GPU 1 (NVIDIA GeForce RTX 4070)" --
                        // where the card's label has room for "GPU 1" and no more.
                        // This list is where a reader finds out which is which.
                        if ui
                            .add(MenuItem::new(*name).selected(i == pick.current))
                            .clicked()
                        {
                            *chosen = Some((*name).to_owned());
                        }
                    }
                });
            }
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

    let tops = rank_all(snap, hold);
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
    /// The card's name. Borrowed for the fixed ones; owned where the card has to
    /// say *which* — an adapter's index ("GPU 1"), the application a frame rate
    /// belongs to ("FPS — chrome.exe"). Long ones are elided to fit; the whole of it
    /// is in `subject`, which the tooltip shows.
    label: Cow<'a, str>,
    /// The headline, in the units the reader should see.
    value: String,
    value_color: Color32,
    /// The line under the trace.
    detail: String,
    /// Borrowed for a resource the snapshot keeps a ring for; owned for the two
    /// that are sums across devices.
    hist: Cow<'a, History>,
    /// A second trace over the first, where one figure is not the whole story:
    /// kernel time inside the CPU total, upload beside download. `None` for the
    /// resources that are one quantity.
    overlay: Option<Trace<'a>>,
    max: f32,
    color: Color32,
    /// The page this card is a way into — where a click goes.
    dest: Dest,
    /// What that click will open, named. The aggregate cards land on the busiest
    /// device, and saying which one is what makes that honest.
    ///
    /// The name only: every card's tooltip ends with the same sentence, so that
    /// half is written where the tooltip is built rather than at all five of the
    /// places a card is built.
    subject: Cow<'a, str>,
    /// The devices this card could be about instead, on the one card that shows a
    /// single device out of several. `None` everywhere else — every other card is
    /// either the only thing of its kind or an honest sum across all of them.
    pick: Option<Pick<'a>>,
}

/// Where a card's click goes, and the page it names in the tooltip.
#[derive(Clone, Copy)]
enum Dest {
    /// The Performance tab's view of the same resource. Every card that is about a
    /// device.
    Perf(Resource),
    /// The Sensors tab's foreground section. The frame figures are about one
    /// application rather than about a device, and the Performance tab has no page
    /// for them.
    Frames,
}

impl Dest {
    /// The tab a click opens, for the second line of every card's tooltip.
    fn tab(self) -> &'static str {
        match self {
            Self::Perf(_) => "Performance",
            Self::Frames => "Sensors",
        }
    }

    fn action(self) -> Action {
        match self {
            Self::Perf(r) => Action::ShowResource(r),
            Self::Frames => Action::ShowFrames,
        }
    }
}

/// The GPU card's device selector: which adapter is on show, and what else there is.
struct Pick<'a> {
    /// Which of `names` the card is about.
    current: usize,
    /// Every adapter, in the snapshot's own order — one menu entry each.
    names: Vec<&'a str>,
}

/// A card's second trace, on the same vertical scale as the first.
struct Trace<'a> {
    hist: Cow<'a, History>,
    color: Color32,
    /// Filled under the line, like the principal trace, when this trace is a
    /// *share of* it — the shaded area then reads as "this much of that". A trace
    /// that stands beside the first instead is a line alone, so the two stay
    /// legible where they cross.
    fill: bool,
}

/// The band, in the order it is read: **CPU, memory, disk, GPU, FPS, network**.
///
/// Pushed in that order and laid out in it, so this function is the page's argument
/// about what to look at first rather than a walk over whatever the snapshot happens
/// to hold. What the order says: the two resources every process spends, then
/// storage, then the graphics pair — an adapter and the frame pacing that is the
/// point of having one, which belong side by side — and the network last, the one
/// resource here that measures traffic with something outside this machine rather
/// than a part of it.
///
/// A card whose device the machine does not have is absent. The FPS card is the one
/// exception, and says why it is empty instead.
fn metrics<'a>(snap: &'a Snapshot, t: &Theme, gpu: &str) -> Vec<Metric<'a>> {
    let s = &snap.sensors;
    let mut out: Vec<Metric<'a>> = Vec::with_capacity(6);

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
        // The share of the load that was spent in the kernel — drivers, page
        // faults, the network stack — which is the Performance tab's "Kernel
        // time" statistic and the band it draws inside the same trace. Part of
        // the total rather than beside it, so it is filled.
        overlay: Some(Trace {
            hist: Cow::Borrowed(&snap.hist.cpu_kernel),
            color: t.cpu_kernel(),
            fill: true,
        }),
        max: 100.0,
        color: t.cpu,
        dest: Dest::Perf(Resource::Cpu),
        subject: Cow::Borrowed(&snap.cpu_name),
        pick: None,
    });

    // --- memory ------------------------------------------------------------
    let mut in_use = String::new();
    fmt::bytes(&mut in_use, snap.mem.in_use);
    let mut committed = String::new();
    fmt::bytes(&mut committed, snap.mem.committed);
    let mut total = String::new();
    fmt::bytes(&mut total, snap.mem.total);
    let pct = snap.mem.percent();
    out.push(Metric {
        label: Cow::Borrowed("Memory"),
        value: fmt::percent_sign_of(pct),
        value_color: t.load_text(pct),
        // What is in RAM and what has been promised, in the Performance tab's own two
        // words for them. Not the total: it is what the headline percentage is a
        // percentage *of*, and the card already says it where it names the resource.
        // Not what is free either -- that is the same number as the headline, read
        // from the other end.
        detail: format!("{in_use} in use · {committed} committed"),
        hist: Cow::Borrowed(&snap.hist.mem),
        overlay: None,
        max: 100.0,
        color: t.mem,
        dest: Dest::Perf(Resource::Memory),
        subject: Cow::Owned(format!("{total} of physical memory")),
        pick: None,
    });

    // --- disks -------------------------------------------------------------
    if !snap.disks.is_empty() {
        let rate: f32 = snap.disks.iter().map(|d| d.read_bps + d.write_bps).sum();
        let hist = sum_hist(snap.hist.disk_rate.iter());
        let busiest = busiest(&snap.disks, |d| d.active_pct);
        // Added up over every disk, not read off the busiest one: what a reader wants
        // from one line is how hard this machine's storage is working, and on a
        // machine where two disks are both half busy the busiest of them reports half
        // of that. It follows that the figure can pass 100% -- two disks pinned are
        // 200% -- which is why the count is printed beside it: "2 disks · 160%" is
        // arithmetic a reader can do, where a bare 160% would read as a bug.
        let active: f32 = snap.disks.iter().map(|d| d.active_pct).sum();
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
                format!("{} disks · {active:.0}% active", snap.disks.len())
            },
            // 1 MB/s floor, as on the Performance tab: without it an idle disk's
            // trace magnifies noise into a mountain range.
            max: graph::nice_ceil(hist.max().max(1024.0 * 1024.0)),
            hist: Cow::Owned(hist),
            overlay: None,
            color: t.disk,
            dest: Dest::Perf(Resource::Disk(busiest)),
            subject: Cow::Borrowed(&snap.disks[busiest].name),
            pick: None,
        });
    }

    // --- graphics ----------------------------------------------------------
    // One card, about one adapter — not one per adapter and not a sum. A machine
    // with a graphics card in it also has the integrated GPU on the processor die,
    // and a second card spends a sixth of this band saying 0% about a device nothing
    // is using; a sum would say that a discrete and an integrated GPU at 50% each
    // are one GPU at 100%, which they are not, and they are not interchangeable
    // either. So: the one that is doing the work worth watching, which by default is
    // the dedicated one, and the card carries a selector for saying otherwise.
    if !snap.gpus.is_empty() {
        let i = chosen_gpu(&snap.gpus, gpu);
        let g = &snap.gpus[i];
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
            overlay: None,
            max: 100.0,
            color: t.gpu,
            dest: Dest::Perf(Resource::Gpu(i)),
            subject: Cow::Borrowed(&g.name),
            // Nothing to choose between on a machine with one adapter, so no
            // control: a menu of one entry is a chevron that does nothing.
            pick: (snap.gpus.len() > 1).then(|| Pick {
                current: i,
                names: snap.gpus.iter().map(|g| &*g.name).collect(),
            }),
        });
    }

    // --- frames ------------------------------------------------------------
    // The one card here about a program rather than about the machine, and what
    // earns it the place beside the GPU: whether what is rendering is *smooth* is
    // the question a utilization figure cannot answer, and a GPU at 60% while frames
    // arrive late is a different fault from one pinned at 100%.
    //
    // Always, and unconditionally — the only card here not contingent on the machine
    // having the thing it is about. Frame timing comes from an ETW session and
    // Windows grants those to elevated processes only, so on an ordinary run there
    // are no figures at all; but a card missing from the band says nothing about why,
    // where a card reading "Needs administrator" says both that this window can
    // measure frame pacing and what it would take. The Sensors tab explains it at
    // length, and is where a click on this card goes.
    {
        let f = snap.frames.stats;
        out.push(Metric {
            // Named, in the slot where every other card names the thing it is about.
            // A frame rate is only ever a fact about one program, and which program
            // is not guessable from the summary: the foreground is whatever the
            // reader last clicked on, and while they are reading this page it is
            // often this window — 60 fps of task manager, sitting where a game's
            // frame rate would be. The image name, as the Sensors tab's own
            // "Foreground — chrome.exe" heading gives it.
            label: if snap.frames.name.is_empty() {
                Cow::Borrowed("FPS")
            } else {
                Cow::Owned(format!("FPS — {}", snap.frames.name))
            },
            value: f.map_or_else(|| MISSING.to_owned(), |s| format!("{:.0}", s.fps)),
            // No threshold colour: what counts as a good frame rate is the display's
            // refresh rate, which nothing here can read, so 45 is neither good news
            // nor bad. Same rule as the disk and network cards, whose rates have no
            // ceiling to be a fraction of either.
            value_color: t.text.primary,
            detail: match f {
                // The rate the slowest one frame in a hundred arrives at, and the
                // mean interval between frames — the Sensors tab's "99th %ile FPS"
                // and "Frame time", in that order and measured over the same window.
                // The pair is the point: an average of 90 with a 99th percentile of
                // 30 is a stutter a reader can feel and the headline cannot show.
                Some(s) => format!("99%ile {:.0} · {:.1} ms", s.low1_fps, s.avg_ms),
                // Or why there are none, which is a different sentence for each way
                // there can be none. See `frames::Status::brief`.
                None => snap.frames.status.brief().to_owned(),
            },
            hist: Cow::Borrowed(&snap.hist.fps),
            overlay: None,
            // 60 fps floor, for the same reason the disk card floors its rate: with
            // the scale left to the data, a title holding a steady 58 draws a flat
            // line across the top of the card and a desktop at 3 fps draws a
            // mountain range.
            max: graph::nice_ceil(snap.hist.fps.max().max(60.0)),
            color: t.gpu,
            dest: Dest::Frames,
            subject: if snap.frames.name.is_empty() {
                Cow::Borrowed("Frame pacing in the foreground application")
            } else {
                Cow::Owned(format!("Frame pacing in {}", snap.frames.name))
            },
            pick: None,
        });
    }

    // --- network -----------------------------------------------------------
    if !snap.nets.is_empty() {
        let recv: f32 = snap.nets.iter().map(|n| n.recv_bps).sum();
        let send: f32 = snap.nets.iter().map(|n| n.send_bps).sum();
        // One trace per direction, as on the Performance tab and in the same two
        // colours: added together they would say a machine pulling 8 Mbps and one
        // pushing 8 Mbps are the same event, and the shape of a download is not the
        // shape of an upload. Summed across adapters, though — that is the
        // aggregation this card does, and the one it can do honestly.
        let recv_hist = sum_hist(snap.hist.net_recv.iter());
        let send_hist = sum_hist(snap.hist.net_send.iter());
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
            // 1 Mbps floor, matching the Performance tab's network graph. Against
            // the taller of the two directions, not their sum: they share the
            // scale, and each is measured against the same ceiling.
            max: graph::nice_ceil(recv_hist.max().max(send_hist.max()).max(125_000.0)),
            hist: Cow::Owned(recv_hist),
            // Line-only, and the accent the Performance tab gives the same series:
            // send stands beside receive rather than inside it, and a second fill
            // would hide whichever direction was quieter.
            overlay: Some(Trace {
                hist: Cow::Owned(send_hist),
                color: t.accent.default,
                fill: false,
            }),
            color: t.net,
            dest: Dest::Perf(Resource::Net(busiest)),
            subject: Cow::Borrowed(&snap.nets[busiest].name),
            pick: None,
        });
    }

    out
}

/// The adapter the GPU card is about: the reader's choice while the machine still
/// has an adapter of that name, and the dedicated one when it does not.
fn chosen_gpu(gpus: &[GpuStats], want: &str) -> usize {
    gpus.iter()
        .position(|g| &*g.name == want)
        .unwrap_or_else(|| dedicated_gpu(gpus))
}

/// Which of a machine's adapters is its *graphics card*: the one with the most
/// dedicated video memory.
///
/// Nothing reports "discrete" as such — DXGI describes an adapter's memory, not what
/// kind of device it is — and dedicated memory is what the distinction amounts to in
/// practice: a card carries gigabytes of its own, and an integrated GPU takes its
/// working set out of system RAM and reports none.
fn dedicated_gpu(gpus: &[GpuStats]) -> usize {
    // `busiest` is the same question asked of a different reading: the largest, and
    // the first of them on a tie — which on a machine where no adapter has memory of
    // its own is DXGI's enumeration order, and so the primary one.
    busiest(gpus, |g| g.dedicated_total as f32)
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

/// What the pointer is on, for a card that has two things it could be on.
#[derive(Clone, Copy)]
struct Hover {
    /// The card itself, whose click opens the page it summarises.
    card: bool,
    /// The device selector on it. This takes the pointer *off* the card — see the
    /// band in [`page`] — so the card is told about it: the surface stays lit, or
    /// the pointer would appear to leave the card by moving onto something inside
    /// it, and the outline goes, because the card's own click is not what a click
    /// here would do.
    pick: bool,
}

/// A card's headline row, laid out once and used twice.
///
/// Neither the interaction nor the paint can own this. The paint has to know whether
/// the selector is under the pointer, which is only settled once the interaction has
/// run; the interaction needs the selector's rect, which is only known once the row
/// has been laid out. So the layout is a third thing, and both read it.
struct Head {
    /// The card's name, elided to whatever the figure beside it left.
    label: Arc<Galley>,
    value: Arc<Galley>,
    /// The one baseline both sit on, set by the figure: it is the principal text on
    /// the line, and topping the two boxes instead leaves a 12px label floating
    /// above an 18px number.
    baseline: f32,
    /// On a card with a device selector: the pill, which is the whole hit target,
    /// and the box the chevron is drawn in at the end of it.
    pick: Option<(Rect, Rect)>,
}

fn head(ui: &Ui, t: &Theme, card: Rect, m: &Metric<'_>) -> Head {
    let p = ui.painter();
    let pad = space::S3;
    let baseline = row_baseline(p, &t.fonts.title_small, card.top() + pad, LINE_TITLE_SMALL);
    let value = p.layout_no_wrap(m.value.clone(), t.fonts.title_small.clone(), m.value_color);
    // The chevron comes out of the label's room and not out of the figure's: a card
    // too narrow for both would otherwise elide the label away and then draw a
    // chevron over the number.
    let caret = if m.pick.is_some() {
        CARET + space::S2
    } else {
        0.0
    };
    // The label is what gives way when the card is too narrow for both. A shortened
    // figure reads as a different figure; a shortened label is still recognisable.
    let room = (card.width() - pad * 2.0 - value.size().x - space::S3 - caret).max(0.0);
    let label = p.layout_no_wrap(
        fmt::elide_to_width(ui, &m.label, &t.fonts.caption, room),
        t.fonts.caption.clone(),
        t.text.secondary,
    );
    let pick = m.pick.as_ref().map(|_| {
        // A glyph on a line of text is centred on the text's *ink*, not on its line
        // box: `ink_baseline` over a line of no height is exactly the distance from
        // the middle of the ink up to the baseline.
        let mid = baseline - ink_baseline(p, &t.fonts.caption, 0.0, 0.0);
        let caret = Rect::from_min_size(
            Pos2::new(
                card.left() + pad + label.size().x + space::S2,
                mid - CARET * 0.5,
            ),
            Vec2::splat(CARET),
        );
        // The label keeps the card's own left gutter, level with the detail line and
        // the graph below it, so the pill grows *outwards* into the padding rather
        // than pushing the text off the column every other card starts at.
        let pill = Rect::from_min_max(
            Pos2::new(card.left() + pad - space::S2, mid - PILL_H * 0.5),
            Pos2::new(caret.right() + space::S2, mid + PILL_H * 0.5),
        );
        (pill, caret)
    });
    Head {
        label,
        value,
        baseline,
        pick,
    }
}

fn paint_metric(ui: &Ui, t: &Theme, card: Rect, m: &Metric<'_>, head: &Head, hover: Hover) {
    let p = ui.painter();
    let corner = CornerRadius::same(radius::MEDIUM);
    // This card is a control as well as a surface — it opens the page it summarises —
    // so it takes the window's card-hover fill *and* an outline under the pointer,
    // where the Sensors tab's cards, which only move, take the outline alone.
    p.rect_filled(
        card,
        corner,
        if hover.card || hover.pick {
            t.bg.card_hover
        } else {
            t.bg.card
        },
    );
    if hover.card {
        p.rect_stroke(
            card,
            corner,
            Stroke::new(1.0, t.stroke.strong),
            StrokeKind::Inside,
        );
    }

    let pad = space::S3;
    if let Some((pill, caret)) = head.pick {
        // The window's one hover grey at `radius-small`, which is what everything
        // else the pointer can land on inside a surface takes: this is a control in
        // a card, not a second surface in one.
        if hover.pick {
            p.rect_filled(pill, t.radius_sm, desktop::hover_fill(t));
        }
        chevron_down(
            p,
            caret,
            if hover.pick {
                t.text.primary
            } else {
                t.text.secondary
            },
        );
    }
    galley_on_baseline(p, card.left() + pad, head.baseline, head.label.clone());
    galley_on_baseline(
        p,
        card.right() - pad - head.value.size().x,
        head.baseline,
        head.value.clone(),
    );

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
        // Slot 1 is seeded with slot 0 so the array is always whole; the slice
        // handed over is one long unless this card has a second trace.
        let mut series = [
            graph::Series {
                hist: &m.hist,
                color: m.color,
                fill: true,
            },
            graph::Series {
                hist: &m.hist,
                color: m.color,
                fill: false,
            },
        ];
        let n = match &m.overlay {
            Some(o) => {
                series[1] = graph::Series {
                    hist: &o.hist,
                    color: o.color,
                    fill: o.fill,
                };
                2
            }
            None => 1,
        };
        graph::sparklines(ui, t, plot, &series[..n], m.max.max(1.0), true);
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
    /// What the ranking is ordered by: the reading itself, or the row's place from
    /// before a Ctrl hold. Kept beside the value rather than in place of it, because
    /// a held row still shows what its process is doing now.
    order: f32,
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

    fn offer(&mut self, idx: u32, value: f32, order: f32) {
        if self.len == TOP_MAX && order <= self.rows[TOP_MAX - 1].order {
            return;
        }
        let at = self.rows[..self.len]
            .iter()
            .position(|e| order > e.order)
            .unwrap_or(self.len);
        if self.len < TOP_MAX {
            self.len += 1;
        }
        // Shift the tail down over the slot, then fill it. What rotates to the front
        // is either the entry falling off the end or the slot just claimed; either
        // way the next line overwrites it.
        self.rows[at..self.len].rotate_right(1);
        self.rows[at] = Entry { idx, value, order };
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
fn rank_all(snap: &Snapshot, hold: &mut Hold) -> [Top; 3] {
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
            if hold.on {
                // Held: the rows that were on show, in the places they were in,
                // whatever their readings have done since — including falling under
                // the floor, because a row disappearing from under the pointer is
                // the one thing the hold is there to stop. Negated, since `Top`
                // keeps the largest and position zero is the top row.
                //
                // A process that has *exited* is simply not in this pass, and the
                // rows under it close up. Nothing can be done about that, and
                // showing a dead process would be worse.
                if let Some(&pos) = hold.rank[k].get(&p.key()) {
                    out[k].offer(i as u32, v, -(pos as f32));
                }
            } else if v >= r.floor() {
                out[k].offer(i as u32, v, v);
            }
        }
    }
    // Recorded whenever the hold is off, so that switching it on has something to
    // hold on to — and never while it is on, or the held order would drift as the
    // readings moved under it. The same rule, and the same reason, as the Processes
    // tab's `rank`.
    if !hold.on {
        for (rank, top) in hold.rank.iter_mut().zip(&out) {
            rank.clear();
            for (pos, e) in top.rows().iter().enumerate() {
                rank.insert(snap.procs[e.idx as usize].key(), pos as u32);
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

    // The largest reading among the rows on show. That is the first row's while the
    // ranking is ordered by value, and need not be while a Ctrl hold has it in the
    // order it had — where a full-width bar three rows down would be the giveaway.
    let scale = r.scale(rows.iter().map(|e| e.value).fold(0.0, f32::max), snap);
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
    // Only the tests name the tracer's states; the page reads them through
    // `Status::brief`.
    use crate::sys::frames;

    fn values(top: &Top) -> Vec<f32> {
        top.rows().iter().map(|e| e.value).collect()
    }

    /// The ranking has to come out ordered whatever order it was offered in, and
    /// each value has to keep the process it came from.
    #[test]
    fn keeps_the_largest_offers_in_order() {
        let mut top = Top::new();
        for (i, v) in [3.0, 1.0, 5.0, 2.0, 4.0].into_iter().enumerate() {
            top.offer(i as u32, v, v);
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
            top.offer(i, i as f32, i as f32);
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
            top.offer(i, 100.0 - i as f32, 100.0 - i as f32);
        }
        let before = values(&top);
        top.offer(999, 0.5, 0.5);
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

    /// One process, enough of one to be ranked by CPU.
    fn proc(pid: u32, cpu: f32) -> Proc {
        Proc {
            pid,
            // Distinct, because `ProcKey` is the pid *and* this, and the hold is
            // keyed by that pair.
            create_time: pid as i64,
            group_head: true,
            sub_cpu: cpu,
            ..Default::default()
        }
    }

    /// A snapshot holding nothing but a process table, which is all a ranking reads.
    fn table(procs: Vec<Proc>) -> Snapshot {
        Snapshot {
            procs,
            ..Default::default()
        }
    }

    fn pids(snap: &Snapshot, top: &Top) -> Vec<u32> {
        top.rows()
            .iter()
            .map(|e| snap.procs[e.idx as usize].pid)
            .collect()
    }

    /// Ctrl holds a ranking in the order it already had, so the row under the
    /// pointer is still there when the button goes down -- and the figures on those
    /// rows go on updating, which is the whole bargain.
    #[test]
    fn holds_the_ranking_in_the_order_it_had() {
        let before = table(vec![proc(1, 9.0), proc(2, 5.0), proc(3, 1.0)]);
        // The order turns over completely, which is what a browser starting a tab
        // does to this list.
        let after = table(vec![proc(1, 1.0), proc(2, 5.0), proc(3, 9.0)]);

        // Nothing held: the ranking follows the readings.
        let mut live = Hold::default();
        assert_eq!(pids(&before, &rank_all(&before, &mut live)[0]), vec![1, 2, 3]);
        assert_eq!(pids(&after, &rank_all(&after, &mut live)[0]), vec![3, 2, 1]);

        // Held: it does not, and the values are this snapshot's.
        let mut held = Hold::default();
        rank_all(&before, &mut held);
        held.on = true;
        let tops = rank_all(&after, &mut held);
        assert_eq!(pids(&after, &tops[0]), vec![1, 2, 3]);
        assert_eq!(values(&tops[0]), vec![1.0, 5.0, 9.0]);

        // A reading that has fallen away entirely keeps its row: a row vanishing
        // from under the pointer is what the hold exists to stop. And nothing new
        // gets in while it is on, however busy it is.
        let later = table(vec![proc(1, 0.0), proc(2, 5.0), proc(3, 9.0), proc(4, 90.0)]);
        let tops = rank_all(&later, &mut held);
        assert_eq!(pids(&later, &tops[0]), vec![1, 2, 3]);
        assert_eq!(values(&tops[0]), vec![0.0, 5.0, 9.0]);

        // Released, it catches up in one frame.
        held.on = false;
        let tops = rank_all(&later, &mut held);
        assert_eq!(pids(&later, &tops[0]), vec![4, 3, 2]);
    }

    /// The band's order is the page's own, not the order the snapshot holds things
    /// in — and the last three of it are the graphics pair and then the network.
    #[test]
    fn ends_the_band_with_graphics_frames_and_the_network() {
        let t = Theme::dark();
        let mut snap = Snapshot::default();
        snap.disks.push(Default::default());
        snap.nets.push(Default::default());
        snap.gpus.push(Default::default());
        let cards = metrics(&snap, &t, "");
        let labels: Vec<&str> = cards.iter().map(|m| &*m.label).collect();
        assert_eq!(labels, ["CPU", "Memory", "Disk", "GPU", "FPS", "Network"]);
    }

    /// The FPS card is in the band whether or not anything can be measured, and its
    /// detail line is either the two figures a headline frame rate cannot say on its
    /// own or the reason there are none.
    #[test]
    fn always_shows_a_frame_pacing_card() {
        let t = Theme::dark();
        let mut snap = Snapshot::default();
        // An unelevated run: the trace session never started. The card stays and says
        // so -- dropped, it would leave a hole in the band that nothing explains.
        snap.frames.status = frames::Status::NeedsElevation;
        snap.frames.name = "game.exe".into();
        let cards = metrics(&snap, &t, "");
        let fps = fps_card(&cards);
        assert_eq!(fps.label, "FPS — game.exe");
        assert_eq!(fps.value, MISSING);
        assert_eq!(fps.detail, "Needs administrator");

        snap.frames.status = frames::Status::Tracing;
        snap.frames.stats = Some(frames::FrameStats {
            source: frames::Source::Dxgi,
            fps: 143.6,
            avg_ms: 6.96,
            low1_fps: 47.2,
            stutter_pct: 3.0,
        });
        let cards = metrics(&snap, &t, "");
        let fps = fps_card(&cards);
        // The card says which program it is about, and the tooltip says it again in
        // full, for the window too narrow to keep the label whole.
        assert_eq!(fps.label, "FPS — game.exe");
        assert_eq!(fps.subject, "Frame pacing in game.exe");
        assert_eq!(fps.value, "144");
        assert_eq!(fps.detail, "99%ile 47 · 7.0 ms");

        // Tracing, but nothing in the foreground is presenting -- which is not a
        // failure and does not read as one.
        snap.frames.stats = None;
        let cards = metrics(&snap, &t, "");
        let fps = fps_card(&cards);
        assert_eq!(fps.label, "FPS — game.exe");
        assert_eq!(fps.value, MISSING);
        assert_eq!(fps.detail, "Nothing rendering");

        // Nothing in the foreground this tick — a process that has just exited, or
        // one this snapshot's table does not hold. The card is still the card.
        snap.frames.name = "".into();
        assert_eq!(fps_card(&metrics(&snap, &t, "")).label, "FPS");
    }

    /// The frame-pacing card, whose label is no longer a fixed string.
    fn fps_card<'a>(cards: &'a [Metric<'a>]) -> &'a Metric<'a> {
        cards
            .iter()
            .find(|m| m.label.starts_with("FPS"))
            .expect("no FPS card")
    }

    /// The GPU card opens on the machine's graphics card, not on whichever adapter
    /// Windows enumerated first -- and a choice is kept by name, so an adapter
    /// arriving or going away cannot silently repoint the card at a different device.
    #[test]
    fn opens_on_the_dedicated_adapter() {
        let gpu = |name: &str, vram: u64| GpuStats {
            name: name.into(),
            dedicated_total: vram,
            ..Default::default()
        };
        // The integrated one first, which is the order a laptop enumerates them in.
        let gpus = [
            gpu("GPU 0 (Intel UHD Graphics)", 0),
            gpu("GPU 1 (NVIDIA GeForce RTX 4070)", 8 << 30),
        ];
        assert_eq!(chosen_gpu(&gpus, ""), 1);
        assert_eq!(chosen_gpu(&gpus, "GPU 0 (Intel UHD Graphics)"), 0);
        // A choice this machine no longer has falls back rather than showing nothing.
        assert_eq!(chosen_gpu(&gpus, "GPU 2 (an eGPU that was unplugged)"), 1);
        // And where no adapter has memory of its own, the first -- DXGI's order,
        // which puts the primary adapter there.
        assert_eq!(chosen_gpu(&[gpu("a", 0), gpu("b", 0)], ""), 0);
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
