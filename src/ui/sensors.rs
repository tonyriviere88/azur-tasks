//! The Sensors tab: what the machine reports about itself, over the last minute.
//!
//! A section per part of the machine, a card per reading, and each card is a trace
//! rather than a dial. A sensor's value only means something next to where it has
//! just been: a package at 60 °C reads one way after a minute of idling and
//! another thirty seconds into a build, and a needle cannot say which this is.
//!
//! Nothing is drawn for a reading the hardware does not have. The card says "—"
//! and leaves its trace empty, because a line along the bottom is a measurement of
//! zero and an absent sensor is not that.
//!
//! ## The reader's arrangement
//!
//! Which sections, in which order, with which cards, is the reader's to decide:
//! drag a heading to move a section, drag a card to move it within its section,
//! right-click for the list of what is switched on. This page is unlike the tables
//! in that respect, and for a reason — a table's rows are all the same kind of
//! thing and sorting them says everything, while a sensor page is a wall of
//! unrelated numbers of which any given reader cares about four. Whose four is not
//! something the app can know.
//!
//! Everything is keyed by *what it is* rather than by where it sits
//! ([`Sect::key`]), so the arrangement survives a driver update, a second GPU, or a
//! firmware that reorders its thermal zones. Anything the saved arrangement has
//! never seen appears at the end of its group rather than not at all.

use super::fmt;
use super::graph;
use super::theme::Theme;
use crate::sys::snapshot::{History, Snapshot};
use azur_egui_theme::components::{
    galley_on_baseline, menu_divider, row_baseline, submenu, Checkbox, ContextMenu, MenuItem, Text,
    TextColor, TextVariant,
};
use azur_egui_theme::tokens::space;
use azur_egui_theme::tokens::typography::LINE_BODY;
use egui::{Align2, Color32, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use std::collections::{BTreeMap, BTreeSet};

/// One reading: its name, its current value, and 60 seconds of it.
///
/// Wider than tall, because that is the shape of a minute.
const CARD: Vec2 = Vec2::new(208.0, 96.0);
/// What a card reads when the machine has no such sensor. An em dash, not a zero:
/// the difference between "nothing to report" and "reported nothing".
const MISSING: &str = "—";
/// The gap between cards, and between a section and the next.
const GAP: f32 = space::S3;
/// The heading row, which is also the handle a section is dragged by.
const HEAD_H: f32 = 24.0;

pub struct SensorsTab {
    /// Scroll offset, kept so switching tabs does not jump back to the top.
    scroll: f32,
    pub layout: Layout,
    /// What is under the pointer mid-drag, if anything.
    drag: Option<Drag>,
}

/// The reader's arrangement, as it is persisted.
///
/// Orders are *partial*: a key the file has never seen sorts after everything it
/// has. That is what lets a machine grow a second graphics card without the saved
/// layout hiding it, and it is why these are lists of keys rather than indices.
#[derive(Default)]
pub struct Layout {
    pub sections: Vec<String>,
    pub cards: BTreeMap<String, Vec<String>>,
    /// Section *and* card keys that are switched off — one set, because a reader
    /// switching things off does not think of them as two kinds of thing.
    pub hidden: BTreeSet<String>,
}

#[derive(Clone, PartialEq, Eq)]
enum Drag {
    Section(String),
    Card { section: String, key: String },
}

impl SensorsTab {
    pub fn new() -> Self {
        Self {
            scroll: 0.0,
            layout: Layout::default(),
            drag: None,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot) {
        let out = egui::ScrollArea::vertical()
            .id_salt("sensors")
            .auto_shrink([false, false])
            // A drag inside the page is a card being moved, never the page being
            // panned: on a touch screen the two gestures are the same one, and the
            // cards get it. The wheel and the bar still scroll.
            .scroll_source(egui::containers::scroll_area::ScrollSource {
                drag: egui::containers::scroll_area::DragScroll::Never,
                ..Default::default()
            })
            .vertical_scroll_offset(self.scroll)
            .show(ui, |ui| {
                azur_egui_theme::scrollbar(ui, t);
                self.page(ui, t, snap);
            });
        self.scroll = out.state.offset.y;
    }

    fn page(&mut self, ui: &mut Ui, t: &Theme, snap: &Snapshot) {
        let mut sects = build(snap, t);
        self.layout.arrange(&mut sects);

        // Nothing here mutates the layout in place. A menu entry or a released
        // drag records what it wants, and it is applied once the frame's borrows
        // are done with -- the same split the rest of the app uses for actions.
        let drag = self.drag.clone();
        let mut edit = Edit::default();

        // Where the things being dragged past currently are, for working out where
        // a release would put the thing being dragged.
        let mut section_rects: Vec<Rect> = Vec::new();
        let mut card_rects: Vec<Rect> = Vec::new();

        for sect in sects.iter().filter(|s| s.shown) {
            let top = ui.cursor().top();

            // --- heading, which is also the section's drag handle -------------
            ui.add_space(GAP);
            let (head, resp) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), HEAD_H),
                Sense::click_and_drag(),
            );
            let held = drag == Some(Drag::Section(sect.key.clone()));
            if resp.drag_started() {
                edit.start = Some(Drag::Section(sect.key.clone()));
            }
            if resp.drag_stopped() {
                edit.drop = true;
            }
            if resp.hovered() || held {
                // A place the pointer lands to pick this section up, so it takes the
                // window's one hover fill rather than a surface of its own.
                ui.painter().rect_filled(
                    head.expand2(Vec2::new(space::S2, 1.0)),
                    CornerRadius::same(4),
                    azur_egui_theme::desktop::hover_fill(t),
                );
            }
            ui.painter().text(
                Pos2::new(head.left(), head.center().y),
                Align2::LEFT_CENTER,
                &sect.title,
                t.fonts.subtitle.clone(),
                if held {
                    t.accent.default
                } else {
                    t.text.primary
                },
            );
            resp.clone().on_hover_cursor(if held {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
            ContextMenu::new(&resp)
                .sticky(true)
                .show(&ui.ctx().clone(), |ui| {
                    section_menu(ui, sect, &sects, &sect.key, &mut edit);
                });
            ui.add_space(space::S1);

            if let Some(note) = &sect.note {
                note_text(ui, note);
            }

            // --- the cards ---------------------------------------------------
            let avail = ui.available_width();
            let per_row = ((avail + GAP) / (CARD.x + GAP)).floor().max(1.0) as usize;
            let shown: Vec<&Card<'_>> = sect.cards.iter().filter(|c| c.shown).collect();
            let mine = matches!(&drag, Some(Drag::Card { section, .. }) if *section == sect.key);
            for chunk in shown.chunks(per_row) {
                let (row, _) = ui.allocate_exact_size(Vec2::new(avail, CARD.y), Sense::hover());
                for (i, card) in chunk.iter().enumerate() {
                    let rect = Rect::from_min_size(
                        Pos2::new(row.left() + i as f32 * (CARD.x + GAP), row.top()),
                        CARD,
                    );
                    if mine {
                        card_rects.push(rect);
                    }
                    // Two responses over one card, which egui hit-tests separately
                    // for clicks and for drags. The whole card takes the click, so
                    // the menu comes up wherever it is asked for; only the title
                    // strip takes the drag.
                    let resp =
                        ui.interact(rect, Id::new(("sensor-card", &card.key)), Sense::click());
                    let grip = title_bar(rect);
                    let grip_resp =
                        ui.interact(grip, Id::new(("sensor-grip", &card.key)), Sense::drag());
                    if grip_resp.drag_started() {
                        edit.start = Some(Drag::Card {
                            section: sect.key.clone(),
                            key: card.key.clone(),
                        });
                    }
                    if grip_resp.drag_stopped() {
                        edit.drop = true;
                    }
                    let held = matches!(&drag, Some(Drag::Card { key, .. }) if *key == card.key);
                    paint_card(
                        ui,
                        t,
                        rect,
                        &card.metric,
                        held,
                        resp.hovered(),
                        grip_resp.hovered() || held,
                    );
                    grip_resp.on_hover_cursor(if held {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    });
                    ContextMenu::new(&resp)
                        .sticky(true)
                        .show(&ui.ctx().clone(), |ui| {
                            section_menu(ui, sect, &sects, &card.key, &mut edit);
                        });
                }
                ui.add_space(GAP);
            }

            section_rects.push(Rect::from_min_max(
                Pos2::new(ui.max_rect().left(), top),
                Pos2::new(ui.max_rect().right(), ui.cursor().top()),
            ));
        }

        // --- the page's own menu, on whatever is left of it -------------------
        let rest = ui.available_rect_before_wrap();
        if rest.height() > 1.0 {
            let resp = ui.interact(rest, Id::new("sensors-page"), Sense::click());
            ContextMenu::new(&resp)
                .sticky(true)
                .show(&ui.ctx().clone(), |ui| {
                    ui.add(
                        Text::new("Sections")
                            .variant(TextVariant::Caption)
                            .color(TextColor::Secondary),
                    );
                    menu_divider(ui);
                    // No anchor: this menu hangs off the page itself, which cannot
                    // be switched off from inside it.
                    section_list(ui, &sects, "", &mut edit);
                    menu_divider(ui);
                    reset_item(ui, &mut edit);
                });
        }

        // --- the insertion marker, and the drop ------------------------------
        if let Some(d) = &drag {
            let pointer = ui.ctx().pointer_interact_pos();
            let slot = pointer.map(|p| match d {
                Drag::Section(_) => slot_vertical(&section_rects, p),
                Drag::Card { .. } => slot_reading_order(&card_rects, p),
            });
            if let Some(slot) = slot {
                let rects = match d {
                    Drag::Section(_) => &section_rects,
                    Drag::Card { .. } => &card_rects,
                };
                paint_marker(ui, t, rects, slot, matches!(d, Drag::Section(_)));
                if edit.drop {
                    edit.moved = Some((d.clone(), slot));
                }
            }
            // A drag has to keep the page repainting: the marker follows a pointer
            // that may be moving over nothing that would otherwise ask for a frame.
            ui.ctx().request_repaint();
        }

        ui.add_space(GAP);
        self.apply(edit, &sects);
    }

    /// Fold a frame's worth of recorded intent into the layout.
    fn apply(&mut self, edit: Edit, sects: &[Sect<'_>]) {
        if edit.reset {
            self.layout = Layout::default();
            self.drag = None;
            return;
        }
        for key in edit.toggle {
            if !self.layout.hidden.remove(&key) {
                self.layout.hidden.insert(key);
            }
        }
        if let Some(start) = edit.start {
            self.drag = Some(start);
        }
        if let Some((what, slot)) = edit.moved {
            match what {
                Drag::Section(key) => {
                    let all: Vec<String> = sects.iter().map(|s| s.key.clone()).collect();
                    let visible: Vec<String> = sects
                        .iter()
                        .filter(|s| s.shown)
                        .map(|s| s.key.clone())
                        .collect();
                    self.layout.sections = reordered(&all, &visible, &key, slot);
                }
                Drag::Card { section, key } => {
                    if let Some(s) = sects.iter().find(|s| s.key == section) {
                        let all: Vec<String> = s.cards.iter().map(|c| c.key.clone()).collect();
                        let visible: Vec<String> = s
                            .cards
                            .iter()
                            .filter(|c| c.shown)
                            .map(|c| c.key.clone())
                            .collect();
                        self.layout
                            .cards
                            .insert(section, reordered(&all, &visible, &key, slot));
                    }
                }
            }
        }
        if edit.drop {
            self.drag = None;
        }
    }
}

/// What a frame decided to change, applied once its borrows are released.
#[derive(Default)]
struct Edit {
    /// Keys whose visibility was flipped.
    toggle: Vec<String>,
    start: Option<Drag>,
    /// The pointer was released this frame.
    drop: bool,
    moved: Option<(Drag, usize)>,
    reset: bool,
}

impl Layout {
    /// Put the built sections and cards in the reader's order, and mark what is
    /// switched off.
    fn arrange(&self, sects: &mut [Sect<'_>]) {
        order_by(&self.sections, sects, |s| &s.key);
        for s in sects.iter_mut() {
            s.shown = !self.hidden.contains(&s.key);
            if let Some(order) = self.cards.get(&s.key) {
                order_by(order, &mut s.cards, |c| &c.key);
            }
            for c in s.cards.iter_mut() {
                c.shown = !self.hidden.contains(&c.key);
            }
        }
    }
}

/// Sort by position in `order`. Anything `order` does not mention keeps its
/// natural place, after everything it does — `sort_by_key` is stable, which is the
/// whole mechanism.
fn order_by<T>(order: &[String], items: &mut [T], key: impl Fn(&T) -> &String) {
    items.sort_by_key(|it| {
        order
            .iter()
            .position(|k| k == key(it))
            .unwrap_or(usize::MAX)
    });
}

/// The full key list with `key` lifted out and put back at the `slot`th *visible*
/// position.
///
/// Two lists, because a hidden card still has a place in the order and must keep
/// it: the reader dropped the card between two things they can see, and where that
/// lands among the things they cannot is decided here rather than left to chance.
fn reordered(all: &[String], visible: &[String], key: &str, slot: usize) -> Vec<String> {
    // The visible slot, as an index into the full list: before whatever visible
    // key currently occupies it, or at the very end.
    let at = match visible.get(slot) {
        Some(k) => all.iter().position(|f| f == k).unwrap_or(all.len()),
        None => all.len(),
    };
    let from = all.iter().position(|k| k == key);
    let mut out: Vec<String> = all.iter().filter(|k| *k != key).cloned().collect();
    // Removing the dragged key shifts everything after it down one, so a target
    // past its old position is one too far.
    let at = match from {
        Some(f) if at > f => at - 1,
        _ => at,
    };
    out.insert(at.min(out.len()), key.to_owned());
    out
}

/// Which gap in a vertical stack the pointer is in.
fn slot_vertical(rects: &[Rect], p: Pos2) -> usize {
    rects.iter().filter(|r| p.y > r.center().y).count()
}

/// Which gap in a wrapped row of equal cards the pointer is in.
///
/// Reading order: everything on an earlier line comes first, then everything to
/// the left on this one. A pointer below the last line lands at the end, which is
/// what dropping into empty space should mean.
fn slot_reading_order(rects: &[Rect], p: Pos2) -> usize {
    rects
        .iter()
        .filter(|r| {
            if p.y < r.top() {
                false
            } else if p.y > r.bottom() {
                true
            } else {
                p.x > r.center().x
            }
        })
        .count()
}

/// The line showing where a release would put the dragged thing.
///
/// Drawn *on* the leading edge of whatever would be pushed along, rather than in the
/// gap before it. The gap before the first card is outside the run of cards, and
/// outside the scroll area's clip rectangle — a marker there is correct arithmetic
/// and an invisible line.
fn paint_marker(ui: &Ui, t: &Theme, rects: &[Rect], slot: usize, vertical: bool) {
    const W: f32 = 2.0;
    let Some(last) = rects.last() else { return };
    let line = match (vertical, rects.get(slot)) {
        // Above the section that would move down, or under the last one.
        (true, Some(r)) => Rect::from_min_max(
            Pos2::new(r.left(), r.top()),
            Pos2::new(r.right(), r.top() + W),
        ),
        (true, None) => Rect::from_min_max(
            Pos2::new(last.left(), last.bottom() - W),
            Pos2::new(last.right(), last.bottom()),
        ),
        // Before a card, or after the last one — which is how the marker follows a
        // wrapped row without having to know where the rows break.
        (false, Some(r)) => Rect::from_min_max(
            Pos2::new(r.left(), r.top()),
            Pos2::new(r.left() + W, r.bottom()),
        ),
        (false, None) => Rect::from_min_max(
            Pos2::new(last.right(), last.top()),
            Pos2::new(last.right() + W, last.bottom()),
        ),
    };
    ui.painter()
        .rect_filled(line, CornerRadius::same(1), t.accent.default);
}

// ---------------------------------------------------------------------------
// Menus
// ---------------------------------------------------------------------------

/// The menu behind a section: what it can show, and where its section sits.
///
/// `anchor` is the key of the thing the menu hangs off — the card that was
/// right-clicked, or the section's own key for a menu opened from its heading. It
/// matters because this menu can switch off its own anchor: see [`closing`].
fn section_menu(ui: &mut Ui, sect: &Sect<'_>, all: &[Sect<'_>], anchor: &str, edit: &mut Edit) {
    ui.add(
        Text::new(sect.title.clone())
            .variant(TextVariant::Caption)
            .color(TextColor::Secondary),
    );
    menu_divider(ui);
    for card in &sect.cards {
        let mut on = card.shown;
        if ui.add(Checkbox::new(&mut on, card.metric.label)).changed() {
            edit.toggle.push(card.key.clone());
            closing(ui, &card.key, anchor);
        }
    }
    menu_divider(ui);
    submenu(ui, MenuItem::new("Sections"), |ui| {
        section_list(ui, all, anchor, edit);
    });
    reset_item(ui, edit);
}

/// A tick per section, for switching whole blocks off.
fn section_list(ui: &mut Ui, all: &[Sect<'_>], anchor: &str, edit: &mut Edit) {
    for s in all {
        let mut on = s.shown;
        if ui.add(Checkbox::new(&mut on, s.title.as_str())).changed() {
            edit.toggle.push(s.key.clone());
            closing(ui, &s.key, anchor);
        }
    }
}

/// Close the menu when the entry just ticked is the one it hangs from.
///
/// Switching off a menu's own anchor takes the menu with it: the card or heading
/// stops being drawn next frame, so the popup is never shown again — and because it
/// was never shown, egui never gets to notice it closed, and keeps it flagged open
/// for whenever that card comes back. Closing deliberately fixes the flag, and is
/// what the reader asked for anyway.
fn closing(ui: &Ui, toggled: &str, anchor: &str) {
    if toggled == anchor {
        ui.close();
    }
}

fn reset_item(ui: &mut Ui, edit: &mut Edit) {
    if ui
        .add(MenuItem::new("Reset layout"))
        .on_hover_text("Back to every sensor, in the order the machine reports them")
        .clicked()
    {
        edit.reset = true;
        ui.close();
    }
}

// ---------------------------------------------------------------------------
// The page's content
// ---------------------------------------------------------------------------

/// One reading and its recent history.
pub struct Metric<'h> {
    label: &'h str,
    /// The current reading. `None` is "this machine has no such sensor", which is
    /// not the same as zero and is not drawn as zero.
    value: Option<f32>,
    /// The reading as the reader should see it, units and all.
    text: String,
    /// Full scale for the trace.
    max: f32,
    color: Color32,
    hist: &'h History,
}

/// One reading, with the key it is remembered by.
struct Card<'h> {
    key: String,
    shown: bool,
    metric: Metric<'h>,
}

/// One block of the page.
struct Sect<'h> {
    /// What this section is *about*, not where it sits: `cpu`, `memory`, `gpu.0`,
    /// `zones`. Cards below it are keyed `<section>.<reading>`.
    key: String,
    title: String,
    note: Option<String>,
    shown: bool,
    cards: Vec<Card<'h>>,
}

/// Collects a section's cards, so a section reads as a list rather than as a
/// sequence of pushes.
struct Builder<'h> {
    key: String,
    title: String,
    note: Option<String>,
    cards: Vec<Card<'h>>,
}

impl<'h> Builder<'h> {
    fn new(key: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            note: None,
            cards: Vec::new(),
        }
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    fn card(&mut self, id: &str, m: Metric<'h>) {
        self.cards.push(Card {
            // Commas separate keys in the settings file, and a thermal zone is
            // named by its firmware.
            key: format!("{}.{}", self.key, id.replace(',', " ")),
            shown: true,
            metric: m,
        });
    }

    /// A 0..100 reading whose text is its own percentage.
    fn percent(&mut self, id: &str, label: &'h str, value: f32, color: Color32, hist: &'h History) {
        self.card(
            id,
            Metric {
                label,
                value: Some(value),
                text: fmt::percent_sign_of(value),
                max: 100.0,
                color,
                hist,
            },
        );
    }

    /// A temperature, on the fixed 0..100 °C scale every temperature here uses so
    /// that two of them can be compared by eye, and coloured by how hot it is.
    fn temperature(
        &mut self,
        t: &Theme,
        id: &str,
        label: &'h str,
        celsius: Option<f32>,
        base: Color32,
        hist: &'h History,
    ) {
        self.card(
            id,
            Metric {
                label,
                value: celsius,
                text: celsius.map_or_else(|| MISSING.to_owned(), |c| format!("{c:.0} °C")),
                max: 100.0,
                color: temp_color(t, celsius.unwrap_or(0.0), base),
                hist,
            },
        );
    }

    fn done(self) -> Sect<'h> {
        Sect {
            key: self.key,
            title: self.title,
            note: self.note,
            shown: true,
            cards: self.cards,
        }
    }
}

/// Every section this machine has, in the order the machine reports them.
fn build<'a>(snap: &'a Snapshot, t: &Theme) -> Vec<Sect<'a>> {
    let s = &snap.sensors;
    let h = &snap.hist;
    let mut out = Vec::new();

    // --- CPU --------------------------------------------------------------
    let mut cpu = Builder::new("cpu", snap.cpu_name.to_string());
    cpu.percent("usage", "Usage", snap.cpu.total, t.cpu, &h.cpu);
    // The clock scale is the nominal maximum, or the highest reading if that is
    // higher: every mobile part boosts past its nominal figure, and a trace pinned
    // to the ceiling while the number keeps climbing is worse than no scale at all.
    cpu.card(
        "clock",
        Metric {
            label: "Clock",
            value: (s.cpu_mhz > 0).then_some(s.cpu_mhz as f32),
            text: format!("{:.2} GHz", s.cpu_mhz as f32 / 1000.0),
            max: graph::nice_ceil(h.cpu_clock.max().max(s.cpu_max_mhz as f32)),
            color: t.cpu,
            hist: &h.cpu_clock,
        },
    );
    cpu.temperature(t, "temp", "Temperature", s.cpu_temp_c, t.cpu, &h.cpu_temp);
    cpu.card(
        "power",
        Metric {
            label: "Power consumption",
            value: s.cpu_power_w,
            text: s
                .cpu_power_w
                .map_or_else(|| MISSING.to_owned(), |w| format!("{w:.1} W")),
            max: graph::nice_ceil(h.cpu_power.max()),
            color: t.cpu,
            hist: &h.cpu_power,
        },
    );
    out.push(cpu.done());

    // --- memory -----------------------------------------------------------
    let mut total = String::new();
    fmt::bytes(&mut total, snap.mem.total);
    let mut used = String::new();
    fmt::bytes(&mut used, snap.mem.in_use);
    let mut committed = String::new();
    fmt::bytes(&mut committed, snap.mem.committed);
    let mut mem = Builder::new("memory", format!("Memory — {total}"));
    mem.card(
        "inuse",
        Metric {
            label: "In use",
            value: Some(snap.mem.percent()),
            text: used,
            max: 100.0,
            color: t.mem,
            hist: &h.mem,
        },
    );
    mem.card(
        "commit",
        Metric {
            label: "Committed",
            value: Some(h.mem_commit.last()),
            text: committed,
            max: 100.0,
            color: t.mem,
            hist: &h.mem_commit,
        },
    );
    out.push(mem.done());

    // --- GPUs -------------------------------------------------------------
    static EMPTY: History = History::EMPTY;
    for (i, g) in snap.gpus.iter().enumerate() {
        // Paired by the sampler, so this tab and the history buffers agree about
        // which device a temperature belongs to.
        let sensors = s.gpus.get(g.sensors as usize);
        // An adapter that appeared this tick has no history slot yet; one empty
        // series stands in for all of them until the next sample.
        let hist_of = |v: &'a Vec<History>| -> &'a History { v.get(i).unwrap_or(&EMPTY) };

        // An adapter whose use is mostly *shared* memory is an integrated one, and
        // what it uses is system RAM rather than a video memory pool -- Windows'
        // own per-adapter counters decide this, not a guess from the name.
        let integrated = g.shared_used > g.dedicated_used;
        let (mem_used, mem_total, mem_label) = if integrated {
            (g.shared_used, 0, "Shared memory")
        } else if let Some(d) = sensors.filter(|d| d.vram_total > 0) {
            // The vendor counts every process on the board; the adapter counter
            // counts this machine's own use of it.
            (d.vram_used, d.vram_total, "VRAM (device)")
        } else {
            (g.dedicated_used, g.dedicated_total, "VRAM")
        };
        let mut mem_text = String::new();
        fmt::bytes(&mut mem_text, mem_used);

        let mut gpu = Builder::new(format!("gpu.{i}"), g.name.to_string());
        // The adapter's busiest engine, from the performance counters, unless the
        // vendor reports its own overall figure.
        let usage = sensors.and_then(|d| d.util_pct).unwrap_or(g.utilization);
        gpu.percent("usage", "Usage", usage, t.gpu, hist_of(&h.gpu));
        gpu.card(
            "mem",
            Metric {
                label: mem_label,
                value: Some(mem_used as f32),
                text: mem_text,
                // A pool with a known size is a fraction of it; system memory the
                // driver borrowed is scaled to the highest it has reached.
                max: if mem_total > 0 {
                    mem_total as f32
                } else {
                    graph::nice_ceil(hist_of(&h.gpu_mem).max())
                },
                color: t.gpu,
                hist: hist_of(&h.gpu_mem),
            },
        );
        gpu.temperature(
            t,
            "temp",
            "Temperature",
            sensors.and_then(|d| d.temp_c),
            t.gpu,
            hist_of(&h.gpu_temp),
        );
        // Only where the device publishes one. Unlike temperature or fan, which
        // every discrete board has and an integrated one honestly lacks, a junction
        // reading is an AMD feature -- so an empty card here would say something
        // false about the hardware rather than about the sensor.
        if let Some(hotspot) = sensors.and_then(|d| d.hotspot_c) {
            gpu.temperature(
                t,
                "hotspot",
                "Hotspot",
                Some(hotspot),
                t.gpu,
                hist_of(&h.gpu_hotspot),
            );
        }
        // A percentage where the device gives one, revolutions where it gives those
        // instead; the trace is the percentage either way, since revolutions have
        // no ceiling to draw against.
        let fan_pct = sensors.and_then(|d| d.fan_pct);
        let fan_rpm = sensors.and_then(|d| d.fan_rpm);
        gpu.card(
            "fan",
            Metric {
                label: "Fan",
                value: fan_pct.or(fan_rpm.map(|r| r as f32)),
                text: match (fan_pct, fan_rpm) {
                    (_, Some(rpm)) => format!("{rpm} rpm"),
                    (Some(pct), None) => format!("{pct:.0} %"),
                    (None, None) => MISSING.to_owned(),
                },
                max: 100.0,
                color: t.status.info,
                hist: hist_of(&h.gpu_fan),
            },
        );
        let core = sensors.and_then(|d| d.core_mhz);
        gpu.card(
            "core",
            Metric {
                label: "Core clock",
                value: core.map(|m| m as f32),
                text: core.map_or_else(|| MISSING.to_owned(), |m| format!("{m} MHz")),
                max: graph::nice_ceil(hist_of(&h.gpu_clock).max()),
                color: t.gpu,
                hist: hist_of(&h.gpu_clock),
            },
        );
        let power = sensors.and_then(|d| d.power_w);
        gpu.card(
            "power",
            Metric {
                label: "Power consumption",
                value: power,
                text: power.map_or_else(|| MISSING.to_owned(), |w| format!("{w:.0} W")),
                max: graph::nice_ceil(hist_of(&h.gpu_power).max()),
                color: t.gpu,
                hist: hist_of(&h.gpu_power),
            },
        );

        // Where the vendor-specific half of this section came from. Worth a line: a
        // reader comparing these numbers against another tool needs to know which
        // interface produced them.
        let gpu = match sensors {
            // Level Zero reports no device name of its own, so there is nothing to
            // add beyond which interface answered.
            Some(d) if d.name.is_empty() => gpu.note(format!("Sensors from {}", d.source)),
            Some(d) => gpu.note(format!("Sensors from {} — {}", d.source, d.name)),
            None => {
                let mut why = String::from(
                    "No sensor interface for this adapter. Temperature, fan and clocks come \
                     from the vendor's own library — NVML for NVIDIA, Level Zero for Intel, \
                     ADL for AMD — which ships with its display driver.",
                );
                // And, when there is one, the specific reason this adapter's library
                // had nothing to say. Only notes for this vendor: a mute ADL says
                // nothing about an NVIDIA card.
                for (_, note) in s.notes.iter().filter(|(v, _)| *v == g.vendor) {
                    why.push(' ');
                    why.push_str(note);
                }
                gpu.note(why)
            }
        };
        out.push(gpu.done());
    }

    // --- the foreground application's frame pacing ------------------------
    //
    // Last of the named sections, because it is about one application rather than
    // about the machine, and because on an unelevated run it is a heading and an
    // explanation rather than any numbers.
    let f = &snap.frames;
    let title = if f.name.is_empty() {
        "Foreground application".to_owned()
    } else {
        format!("Foreground — {}", f.name)
    };
    let mut fg = Builder::new("frames", title);
    let stats = f.stats;
    fg.card(
        "fps",
        Metric {
            label: "FPS",
            value: stats.map(|s| s.fps),
            text: stats.map_or_else(|| MISSING.to_owned(), |s| format!("{:.0}", s.fps)),
            max: graph::nice_ceil(h.fps.max()),
            color: t.gpu,
            hist: &h.fps,
        },
    );
    fg.card(
        "frametime",
        Metric {
            label: "Frame time",
            value: stats.map(|s| s.avg_ms),
            text: stats.map_or_else(|| MISSING.to_owned(), |s| format!("{:.1} ms", s.avg_ms)),
            max: graph::nice_ceil(h.frametime.max()),
            color: t.gpu,
            hist: &h.frametime,
        },
    );
    // No history of its own: it is a *statistic* of the same window the FPS trace is
    // drawn from, so the trace under it is that one. The number is what matters.
    fg.card(
        "low1",
        Metric {
            label: "99th %ile FPS",
            value: stats.map(|s| s.low1_fps),
            text: stats.map_or_else(|| MISSING.to_owned(), |s| format!("{:.0}", s.low1_fps)),
            max: graph::nice_ceil(h.fps.max()),
            color: t.gpu,
            hist: &h.fps,
        },
    );
    fg.card(
        "stutter",
        Metric {
            label: "Micro-stutter",
            value: stats.map(|s| s.stutter_pct),
            text: stats.map_or_else(|| MISSING.to_owned(), |s| format!("{:.1} %", s.stutter_pct)),
            max: 100.0,
            color: t.status.warning,
            hist: &h.stutter,
        },
    );
    let mut note = f.status.note();
    if let Some(s) = stats {
        note.push_str(&format!(" Frames counted by {}.", s.source.label()));
    }
    out.push(fg.note(note).done());

    // --- thermal zones ----------------------------------------------------
    let mut zones = Builder::new("zones", "Thermal zones");
    let zones = if s.zones.is_empty() {
        zones.note(
            "This machine publishes no ACPI thermal zones, so it reports no temperatures \
             to an unprivileged process.",
        )
    } else {
        for (i, z) in s.zones.iter().enumerate() {
            zones.temperature(
                t,
                &z.label,
                &z.label,
                Some(z.celsius),
                t.cpu,
                h.zone_temp.get(i).unwrap_or(&EMPTY),
            );
        }
        zones.note(
            "Firmware sensors, in the order the firmware lists them. The names are the \
             machine's own; the untranslated ones are tags only its vendor documents.",
        )
    };
    out.push(zones.done());

    out
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// A temperature's colour: the section's own while it is unremarkable, then amber,
/// then red.
///
/// Colour is the page's alarm, so it has to mean something. A trace already green
/// at 40 °C has nothing left to say at 90. 70 is where a laptop part starts
/// throttling and 85 is where it is in trouble.
fn temp_color(t: &Theme, celsius: f32, base: Color32) -> Color32 {
    if celsius >= 85.0 {
        t.status.danger
    } else if celsius >= 70.0 {
        t.status.warning
    } else {
        base
    }
}

fn note_text(ui: &mut Ui, text: &str) {
    ui.add(
        Text::new(text.to_owned())
            .variant(TextVariant::Caption)
            .color(TextColor::Tertiary),
    );
    ui.add_space(space::S1);
}

/// The strip a card is dragged by: its name and its reading, and nothing else.
///
/// The trace below is not a handle. It is the part of the card a reader puts the
/// pointer on to look at, and a drag beginning there would be indistinguishable from
/// reading it — the same reason a window is moved by its caption and not by its
/// contents.
fn title_bar(card: Rect) -> Rect {
    Rect::from_min_max(
        card.min,
        Pos2::new(card.right(), card.top() + space::S3 * 2.0 + LINE_BODY),
    )
}

fn paint_card(
    ui: &Ui,
    t: &Theme,
    card: Rect,
    m: &Metric<'_>,
    held: bool,
    hovered: bool,
    grip: bool,
) {
    let p = ui.painter();
    let corner = CornerRadius::same(6);
    p.rect_filled(card, corner, t.bg.card);
    // The card being dragged stays where it was, greyed, so the row it came out of
    // does not close up and move everything else while the pointer is still down.
    if held {
        p.rect_filled(card, corner, t.bg.canvas.gamma_multiply(0.7));
        p.rect_stroke(
            card,
            corner,
            Stroke::new(1.0, t.accent.default),
            egui::StrokeKind::Inside,
        );
        return;
    }
    if hovered {
        p.rect_stroke(
            card,
            corner,
            Stroke::new(1.0, t.stroke.subtle),
            egui::StrokeKind::Inside,
        );
    }

    // The handle says so when the pointer is on it, so a reader finds out that the
    // card moves without having to try it on the trace first.
    if grip {
        p.rect_filled(
            title_bar(card),
            CornerRadius {
                nw: 6,
                ne: 6,
                sw: 0,
                se: 0,
            },
            t.bg.card_hover,
        );
    }

    let pad = space::S3;
    // Name on the left, reading on the right, the way a legend is read — and both
    // on one baseline. The reading is `body_strong` and the name is `caption`, so
    // topping the two boxes leaves the name floating above the number it belongs to.
    // The reading is the principal text here, so it is the one that sets the line.
    let head = LINE_BODY;
    let baseline = row_baseline(p, &t.fonts.body_strong, card.top() + pad, head);
    let name = p.layout_no_wrap(
        m.label.to_owned(),
        t.fonts.caption.clone(),
        t.text.secondary,
    );
    galley_on_baseline(p, card.left() + pad, baseline, name);
    let reading = p.layout_no_wrap(
        m.text.clone(),
        t.fonts.body_strong.clone(),
        if m.value.is_some() {
            t.text.primary
        } else {
            t.text.tertiary
        },
    );
    galley_on_baseline(p, card.right() - pad - reading.size().x, baseline, reading);

    let plot = Rect::from_min_max(
        Pos2::new(card.left() + pad, card.top() + pad + head + space::S2),
        Pos2::new(card.right() - pad, card.bottom() - pad),
    );
    if plot.width() < 8.0 || plot.height() < 8.0 {
        return;
    }
    if m.value.is_some() {
        graph::sparkline(ui, t, plot, m.hist, m.color, m.max.max(1.0), true);
    } else {
        // The recess, with nothing in it. A trace along the bottom would be a
        // reading of zero, and this machine did not report one.
        p.rect_filled(plot, CornerRadius::ZERO, t.bg.canvas);
        p.rect_stroke(
            plot,
            CornerRadius::ZERO,
            Stroke::new(1.0, t.stroke.subtle),
            egui::StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Moving one thing past another, in both directions, and off each end.
    #[test]
    fn moves_a_key_to_the_slot_it_was_dropped_in() {
        let all = keys(&["a", "b", "c", "d"]);
        // Dropped at slot 0: before everything.
        assert_eq!(reordered(&all, &all, "c", 0), keys(&["c", "a", "b", "d"]));
        // Slot 4 is past the last item: the end.
        assert_eq!(reordered(&all, &all, "a", 4), keys(&["b", "c", "d", "a"]));
        // Forwards by one. Slot 2 is "before c", and with `b` lifted out that is
        // still before `c` -- the off-by-one this exists to get right.
        assert_eq!(reordered(&all, &all, "b", 2), keys(&["a", "b", "c", "d"]));
        assert_eq!(reordered(&all, &all, "b", 3), keys(&["a", "c", "b", "d"]));
        // Backwards.
        assert_eq!(reordered(&all, &all, "d", 1), keys(&["a", "d", "b", "c"]));
        // A key that is not there at all leaves the list alone but for the append.
        assert_eq!(
            reordered(&all, &all, "z", 1),
            keys(&["a", "z", "b", "c", "d"])
        );
    }

    /// A hidden key keeps its place. The reader dropped something between two
    /// cards they can see; what happens to the one they cannot is decided here.
    #[test]
    fn keeps_a_hidden_key_where_it_was() {
        let all = keys(&["a", "hidden", "b", "c"]);
        let visible = keys(&["a", "b", "c"]);
        // Dropped before `b`, i.e. visible slot 1. `hidden` sits between `a` and
        // `b`, so landing "before b" means after it -- and it stays put either way.
        assert_eq!(
            reordered(&all, &visible, "c", 1),
            keys(&["a", "hidden", "c", "b"])
        );
        // Dropped at the end.
        assert_eq!(
            reordered(&all, &visible, "a", 3),
            keys(&["hidden", "b", "c", "a"])
        );
    }

    /// The saved order is partial on purpose: a card the file has never seen has
    /// to appear, and it appears last rather than first.
    #[test]
    fn puts_what_the_saved_order_never_saw_at_the_end() {
        let mut items = keys(&["usage", "clock", "temp", "hotspot"]);
        order_by(&keys(&["temp", "usage"]), &mut items, |k| k);
        assert_eq!(items, keys(&["temp", "usage", "clock", "hotspot"]));
        // And unlisted ones keep their own relative order.
        let mut items = keys(&["a", "b", "c"]);
        order_by(&[], &mut items, |k| k);
        assert_eq!(items, keys(&["a", "b", "c"]));
    }

    /// Reading order, so a drop into a wrapped grid lands where the pointer looks.
    #[test]
    fn finds_the_gap_the_pointer_is_in() {
        let row = |y: f32, n: usize| -> Vec<Rect> {
            (0..n)
                .map(|i| Rect::from_min_size(Pos2::new(i as f32 * 100.0, y), Vec2::new(90.0, 50.0)))
                .collect()
        };
        let mut rects = row(0.0, 3);
        rects.extend(row(60.0, 2)); // a second line of two

        // Left of the first card on the first line.
        assert_eq!(slot_reading_order(&rects, Pos2::new(5.0, 20.0)), 0);
        // Right half of the first card: after it.
        assert_eq!(slot_reading_order(&rects, Pos2::new(80.0, 20.0)), 1);
        // Second line, left of its first card: after all three of the first line.
        assert_eq!(slot_reading_order(&rects, Pos2::new(5.0, 80.0)), 3);
        // Past the last card, in the empty space after it: the very end.
        assert_eq!(slot_reading_order(&rects, Pos2::new(400.0, 80.0)), 5);
        // Below everything.
        assert_eq!(slot_reading_order(&rects, Pos2::new(5.0, 500.0)), 5);

        // Vertical stacks are simpler: the halfway line of each.
        let stack = vec![
            Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 40.0)),
            Rect::from_min_size(Pos2::new(0.0, 40.0), Vec2::new(100.0, 40.0)),
        ];
        assert_eq!(slot_vertical(&stack, Pos2::new(10.0, 5.0)), 0);
        assert_eq!(slot_vertical(&stack, Pos2::new(10.0, 30.0)), 1);
        assert_eq!(slot_vertical(&stack, Pos2::new(10.0, 70.0)), 2);
    }
}
