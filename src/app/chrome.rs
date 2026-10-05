//! The window's furniture: the navigation rail, the command row, the status bar.
//!
//! Everything here paints a strip of chrome and reads or writes one field of
//! [`App`]. None of it is about *what* a tab shows — that is the tab's own module —
//! and none of it performs an action: a control here raises an
//! [`Action`](crate::ui::Action) and [`super::actions`] carries it out.

use super::{screen_anchor_below, App, Tab};
use crate::sys::snapshot::Snapshot;
use crate::ui::icons::{self, Glyph};
use crate::ui::theme::Theme;
use crate::ui::{fmt, Action};
use azur_egui_theme as azur;
use azur::components::{
    Badge, BadgeVariant, Button, IconButton, NavItem, NavigationRail, SearchBox, Select, Size,
    Text, TextColor, TextVariant, Variant,
};
use azur::tokens::{row, space};
use egui::{Align, Align2, Color32, Id, Layout, Pos2, Rect, Sense, Ui, Vec2};

/// Height of the command row under the title bar: the tallest control in it with
/// `space-1` of air above and below.
///
/// Everything in the row is a small control at the desktop density -- search box,
/// buttons, the cadence picker -- so the row is exactly `control::SMALL` plus its
/// padding. Anything taller added here (a `CommandBar` frames its buttons in
/// `space-1` and stands 4px higher) has to be counted in, or the row overflows its
/// own panel and paints over the seam under it.
pub(super) const TOOLBAR_H: f32 = azur::tokens::control::SMALL + PANEL_PAD_Y * 2.0;

/// The air between a chrome strip's edge and the controls in it. `space-2`: at the
/// dense density the strip is only as tall as one control, and this is what keeps
/// the control off the seam -- padding inside a surface, not a margin around a
/// floating one.
pub(super) const PANEL_PAD_Y: f32 = space::S2;

/// The status bar: `row::TIGHT`, which is the height the design system gives a
/// status line -- one caption line and a point of air. It carries small badges
/// (`space-5` tall) and nothing else.
pub(super) const STATUS_H: f32 = row::TIGHT;

/// Sampling cadence presets, mirroring Task Manager's View > Update speed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Speed {
    High,
    Normal,
    Low,
    VeryLow,
    Paused,
}

impl Speed {
    const ALL: [Speed; 5] = [
        Speed::High,
        Speed::Normal,
        Speed::Low,
        Speed::VeryLow,
        Speed::Paused,
    ];

    /// Kept short: these are the options of a fixed-width `Select`, and a label
    /// wider than the trigger truncates rather than widening it.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::High => "High",
            Self::Normal => "Normal",
            Self::Low => "Low",
            Self::VeryLow => "Very low",
            Self::Paused => "Paused",
        }
    }

    pub(super) fn interval_ms(self) -> u32 {
        match self {
            Self::High => 500,
            Self::Normal => 1000,
            Self::Low => 2000,
            Self::VeryLow => 4000,
            // Pausing is a separate flag; keep a sane period for resume.
            Self::Paused => 1000,
        }
    }
}

impl App {
    /// The navigation rail: Azur's, with this app's live footer beneath it.
    pub(super) fn nav_rail(&mut self, ui: &mut Ui, snap: &Snapshot) -> bool {
        let t = &self.theme;
        let narrow = self.nav_collapsed;
        let rect = ui.available_rect_before_wrap();
        let mut elevate = false;

        // The rail lays its entries out from the top and the footer is anchored
        // to the bottom, so each gets its own child `Ui` and neither has to know
        // how tall the other is.
        let foot_h = 132.0;
        let mut nav = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    rect.min,
                    Pos2::new(rect.right(), rect.bottom() - foot_h),
                ))
                .layout(Layout::top_down(Align::Min)),
        );
        let mut index = Tab::ALL.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let mut toggled = false;
        nav.add(
            NavigationRail::new(&mut index)
                .collapsed(narrow)
                // The rail's own width is the rail's business, so its control
                // lives here rather than in the command row.
                .toggle(
                    &icons::icon::sidebar,
                    if narrow {
                        "Expand the sidebar"
                    } else {
                        "Collapse the sidebar"
                    },
                    &mut toggled,
                )
                .items(Tab::ALL.map(|tab| NavItem::new(tab.label()).icon(tab.icon()))),
        );
        self.tab = Tab::ALL[index];
        if toggled {
            self.nav_collapsed = !narrow;
        }

        // --- footer: live CPU and memory ----------------------------------
        //
        // Only the horizontal padding changes with the rail's width. The vertical
        // geometry is shared, so the gauges sit at the same height in both states
        // -- a shorter footer put them over the elevation button.
        let pad = if narrow { space::S2 } else { space::S3 };
        let foot = Rect::from_min_max(
            Pos2::new(rect.left() + pad, rect.bottom() - foot_h),
            Pos2::new(rect.right() - pad, rect.bottom() - space::S3),
        );

        // Collapsed there is no room for a label beside a number beside a graph,
        // so the label goes (the colour already says which is which) and the value
        // sits directly above a shorter sparkline.
        let gauge_h = 30.0;
        let gap = 14.0;
        for (i, (label, value, color, hist)) in [
            ("CPU", snap.cpu.total, t.cpu, &snap.hist.cpu),
            ("Memory", snap.mem.percent(), t.mem, &snap.hist.mem),
        ]
        .into_iter()
        .enumerate()
        {
            let r = Rect::from_min_size(
                Pos2::new(foot.left(), foot.top() + i as f32 * (gauge_h + gap)),
                Vec2::new(foot.width(), gauge_h),
            );
            let mut b = String::new();
            fmt::percent_sign(&mut b, value);

            let graph_top = if narrow {
                ui.painter().text(
                    Pos2::new(r.center().x, r.top()),
                    Align2::CENTER_TOP,
                    &b,
                    t.fonts.caption.clone(),
                    color,
                );
                r.top() + 14.0
            } else {
                ui.painter().text(
                    r.left_top(),
                    Align2::LEFT_TOP,
                    label,
                    t.fonts.caption.clone(),
                    t.text.secondary,
                );
                ui.painter().text(
                    r.right_top(),
                    Align2::RIGHT_TOP,
                    &b,
                    t.fonts.caption.clone(),
                    color,
                );
                r.top() + 16.0
            };
            let graph = Rect::from_min_max(
                Pos2::new(r.left(), graph_top),
                Pos2::new(r.right(), r.bottom()),
            );
            crate::ui::graph::sparkline(ui, t, graph, hist, color, 100.0, false);
            if narrow {
                ui.interact(r, Id::new("nav-gauge").with(i as u8), Sense::hover())
                    .on_hover_text(format!("{label} {b}"));
            }
        }

        // --- elevation state / elevate button ------------------------------
        //
        // Collapsed, this is one glyph with nothing beside it, so it is centred on
        // the rail the way the nav icons above it are — anything else reads as a
        // stray mark pushed against the left edge. Expanded it is an icon followed
        // by a label, which has to start where every other label in the rail does.
        //
        // The centring is a `top_down` layout rather than a horizontal one with
        // `main_align: Center`: a single item's *cross*-axis alignment is exact in
        // egui, where centring along the main axis of a horizontal run depends on
        // the content's width being known before it is added.
        let btn_h = azur::tokens::control::SMALL;
        let (btn, layout) = if narrow {
            // Only as tall as what goes in it: a `top_down` layout puts its item at
            // the *top* of the box, so a box taller than the content would centre
            // the glyph horizontally and lift it vertically in the same move. The
            // centre stays where the expanded state has it.
            let content_h = if self.elevated { 14.0 } else { btn_h };
            (
                Rect::from_center_size(
                    Pos2::new(foot.center().x, foot.bottom() - btn_h / 2.0),
                    Vec2::new(foot.width(), content_h),
                ),
                Layout::top_down(Align::Center),
            )
        } else {
            (
                Rect::from_min_size(
                    Pos2::new(foot.left(), foot.bottom() - btn_h),
                    Vec2::new(foot.width(), btn_h),
                ),
                Layout::left_to_right(Align::Center),
            )
        };
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(btn).layout(layout));
        if self.elevated {
            let (r, resp) = child.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            icons::draw(child.painter(), r, Glyph::Shield, t.status.success);
            if narrow {
                azur::components::tooltip(resp, "Running as administrator");
            } else {
                child.add(
                    Text::new("Administrator")
                        .variant(TextVariant::Caption)
                        .color(TextColor::Secondary),
                );
            }
        } else {
            // Subtle, not primary: elevating is a detour, and the accent belongs
            // to the action a reader came here for.
            let clicked = if narrow {
                child
                    .add(
                        IconButton::new(&icons::icon::shield)
                            .variant(Variant::Subtle)
                            .size(Size::Small)
                            .tooltip("Restart elevated to see all processes"),
                    )
                    .clicked()
            } else {
                child
                    .add(
                        Button::new("Run as administrator")
                            .variant(Variant::Subtle)
                            .size(Size::Small)
                            .icon(&icons::icon::shield)
                            .full_width(true),
                    )
                    .on_hover_text("Restart elevated to see all processes and to end protected ones")
                    .clicked()
            };
            elevate = clicked;
        }
        elevate
    }

    /// The command row: the filter, whatever the tab puts next to it, and the
    /// sampling cadence on the right.
    pub(super) fn toolbar(&mut self, ui: &mut Ui, snap: &Snapshot, action: &mut Option<Action>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S2;

            // Everything variable goes inside one scope, so that what follows it
            // cannot be renumbered.
            //
            // egui gives a widget an id from its position in the row, so the group
            // pinned to the right end — which keeps the same rect on every tab — used
            // to take a new id whenever the filter field or a tab's own buttons came
            // or went. A rect that stays put while its id changes is what
            // `warn_if_rect_changes_id` draws a red border around in a debug build,
            // and it means the control's state is dropped with it. A child scope
            // advances the parent's counter by exactly one however many widgets it
            // holds (`egui::Ui`, `IdSource::Child`), so the right-hand group is always
            // this row's second child and keeps its identity.
            ui.scope(|ui| {
                if self.tab.searchable() {
                    // Never let the field grow into the cadence group on the right,
                    // and never shrink it below something you can read a needle in.
                    let w = (ui.available_width() - 300.0).clamp(120.0, 260.0);
                    let field = ui.add(
                        SearchBox::new(&mut self.filter)
                            .placeholder("Filter by name, PID, publisher…")
                            .size(Size::Small)
                            .width(w),
                    );
                    // Ctrl+F or F3 focuses the box. Both, because both are what
                    // Windows taught: Ctrl+F in an application, F3 in Explorer and in
                    // the shell. Non-short-circuiting `|` so the second is consumed
                    // whether or not the first fired.
                    //
                    // Only reachable while the field is on screen, since this
                    // whole block is behind `Tab::searchable` -- a shortcut that
                    // focuses nothing would be a key that seems not to work.
                    let focus = ui.input_mut(|i| {
                        i.consume_key(egui::Modifiers::CTRL, egui::Key::F)
                            | i.consume_key(egui::Modifiers::NONE, egui::Key::F3)
                    });
                    if focus {
                        field.request_focus();
                    }
                    // `lost_focus` as well as `has_focus`, and that pair is what makes
                    // Escape work at all. egui drops the focused widget while it
                    // processes input at the *start* of a pass when one of the events
                    // is Escape (the `Key::Escape` arm of the focus handling in
                    // `memory/mod.rs`), so on the one frame the answer matters, every
                    // widget in the window reports `has_focus() == false` -- which is
                    // what had quietly stopped "Esc clears the filter" from working.
                    // `lost_focus` is true there instead, because `begin_pass` records
                    // the previous frame's focus before clearing it.
                    self.filter_focused = field.has_focus() || field.lost_focus();
                } else {
                    self.filter_focused = false;
                }

                self.toolbar_left(ui, snap, action);
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = space::S2;
                self.toolbar_right(ui);
            });
        });
    }

    /// The inspector's latch, on the cadence end of the toolbar.
    ///
    /// A latched `IconButton` rather than a labelled one: it is a view control, not
    /// a command, and the accent-subtle fill a pressed icon button takes is the
    /// design system's way of saying a mode is on. The panel it opens says what it
    /// is in its own tabs, so the button does not have to.
    fn inspector_toggle(&mut self, ui: &mut Ui) {
        if ui
            .add(
                IconButton::new(&icons::icon::bottom_panel)
                    .variant(Variant::Subtle)
                    .size(Size::Small)
                    .pressed(self.inspector_open)
                    .tooltip("Modules, handles and threads of the selected process (Alt+D)"),
            )
            .clicked()
        {
            self.inspector_open = !self.inspector_open;
        }
    }

    /// Per-tab controls, laid out left to right.
    fn toolbar_left(&mut self, ui: &mut Ui, snap: &Snapshot, action: &mut Option<Action>) {
        // Leftmost, and only on the two tabs that list processes: starting one is the
        // counterpart to ending one and belongs beside it, in the order Task Manager
        // puts them in. It acts on nothing selected, so unlike End task it is never
        // disabled.
        if self.tab.is_process_list() {
            // The label cannot say where the program is typed in, and the button next
            // to it is the one thing in the window that destroys something -- so this
            // one says plainly that it opens a dialog.
            let resp = azur::components::tooltip(
                ui.add(
                    Button::new("New task")
                        .variant(Variant::Secondary)
                        .size(Size::Small),
                ),
                "Start a program, through Windows' Run dialog",
            );
            if resp.clicked() {
                // Measured here rather than in the action: the button's rectangle is
                // a fact of this frame's layout, and by the time the dialog is being
                // opened there is nothing left to ask.
                *action = Some(Action::NewTask {
                    at: screen_anchor_below(ui.ctx(), resp.rect),
                });
            }
        }

        match self.tab {
            Tab::Processes => {
                let sel = self.processes.list.selected;
                let target = sel.and_then(|k| snap.find_by_key(k));
                // Secondary, not danger: it is the one button in the window that
                // destroys something, but it is also the one on this toolbar and it
                // sits there whether or not a row is selected. A red fill above a
                // list you are reading is an alarm about nothing -- the confirmation
                // is where the weight belongs.
                if ui
                    .add_enabled(
                        target.is_some(),
                        Button::new("End task")
                            .variant(Variant::Secondary)
                            .size(Size::Small),
                    )
                    .clicked()
                {
                    if let Some(i) = target {
                        *action = Some(Action::EndTask {
                            pid: snap.procs[i].pid,
                            name: snap.procs[i].name.to_string(),
                        });
                    }
                }
                // Grouping follows the sort (see `ProcessesTab::grouped`) rather
                // than a control, and the group headings in the list say so
                // themselves -- a badge repeating it was one more thing to read.
            }
            Tab::Details => {
                let sel = self.details.list.selected;
                let target = sel.and_then(|k| snap.find_by_key(k));
                if ui
                    .add_enabled(
                        target.is_some(),
                        Button::new("End task")
                            .variant(Variant::Secondary)
                            .size(Size::Small),
                    )
                    .clicked()
                {
                    if let Some(i) = target {
                        *action = Some(Action::EndTask {
                            pid: snap.procs[i].pid,
                            name: snap.procs[i].name.to_string(),
                        });
                    }
                }
            }
            Tab::Services => {
                let (total, running) = self.services.count();
                ui.add(
                    Badge::new(format!("{running} running"))
                        .small(true)
                        .variant(BadgeVariant::Success),
                );
                ui.add(Badge::new(format!("{total} total")).small(true));
            }
            Tab::Startup => {
                // The one control on this tab, so it reads as the primary action
                // rather than as a destructive one: nothing is lost either way,
                // and the entry can be switched straight back.
                let target = self.startup.selected_entry().cloned();
                let label = match &target {
                    Some(e) if e.enabled => "Disable",
                    _ => "Enable",
                };
                let resp = ui.add_enabled(
                    target.is_some(),
                    Button::new(label)
                        .variant(Variant::Primary)
                        .size(Size::Small),
                );
                let resp = match &target {
                    Some(e) if e.source.needs_admin() && !self.elevated => resp.on_hover_text(
                        "This entry applies to all users, so changing it needs administrator rights",
                    ),
                    _ => resp.on_hover_text("Takes effect at the next sign-in"),
                };
                if resp.clicked() {
                    if let Some(e) = target {
                        *action = Some(Action::SetStartupEnabled {
                            enable: !e.enabled,
                            entry: Box::new(e),
                        });
                    }
                }

                let (total, enabled) = self.startup.count();
                ui.add(
                    Badge::new(format!("{enabled} enabled"))
                        .small(true)
                        .variant(BadgeVariant::Success),
                );
                ui.add(Badge::new(format!("{total} total")).small(true));
            }
            // Nothing to act on: all three are read-outs, and the summary's own
            // rows carry the process menu themselves.
            Tab::Summary | Tab::Performance | Tab::Sensors => {}
        }
    }

    /// The sampling cadence, added right to left.
    fn toolbar_right(&mut self, ui: &mut Ui) {
        let mut picked = Speed::ALL
            .iter()
            .position(|s| *s == self.speed())
            .or(Some(1));
        let response = ui.add(
            Select::new(&mut picked)
                .options(Speed::ALL.map(|s| s.label()))
                // The chronometer carries what the caption used to say: the
                // field is about how often, not about what. 20px wider than the
                // bare trigger, which is the icon and its gap -- the value keeps
                // the room it had.
                .icon(&icons::icon::stopwatch)
                // Subtle: nothing on this end of the toolbar has a border, so a
                // filled field here would be the loudest thing on a strip whose
                // job is to stay out of the way. The chevron says it opens.
                .variant(Variant::Subtle)
                .size(Size::Small)
                .width(124.0),
        );
        if response.changed() {
            match picked.map(|i| Speed::ALL[i]) {
                Some(Speed::Paused) => self.mon.set_paused(true),
                Some(opt) => {
                    self.mon.set_paused(false);
                    self.mon.set_interval(opt.interval_ms());
                }
                None => {}
            }
        }
        // The icon says "time" but not which direction, so the tooltip carries
        // the sentence the caption used to.
        azur::components::tooltip(response, "Update speed: how often the machine is sampled");

        // Sampling on its own cadence makes this a no-op that happens to work, so
        // the button exists only while paused -- a control that does nothing is
        // worse than no control. F5 still samples whatever the cadence is.
        // Beside the cadence picker because that is what it overrides.
        if self.mon.is_paused()
            && ui
                .add(
                    IconButton::new(&icons::icon::refresh)
                        .variant(Variant::Subtle)
                        .size(Size::Small)
                        .tooltip("Sample now (F5)"),
                )
                .clicked()
        {
            self.mon.refresh_now();
        }

        // Last in a right-to-left run, so it sits to the left of the cadence group:
        // a view control belongs with the other view controls rather than beside End
        // task, which is a command and the one destructive thing in the window.
        if self.tab.is_process_list() {
            self.inspector_toggle(ui);
        }
    }

    fn speed(&self) -> Speed {
        if self.mon.is_paused() {
            return Speed::Paused;
        }
        match self.mon.interval() {
            ms if ms <= 500 => Speed::High,
            ms if ms <= 1000 => Speed::Normal,
            ms if ms <= 2000 => Speed::Low,
            _ => Speed::VeryLow,
        }
    }

    pub(super) fn status_bar(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let t = &self.theme;
        // `max_rect`, not `available_rect_before_wrap`: the strip has to be the
        // whole panel whatever the row inside it measures, or the table above would
        // take the difference and paint over the seam.
        let strip = ui.max_rect();
        ui.painter()
            .rect_filled(strip, azur::desktop::structural_radius(), t.bg.layer);
        // The seam that divides it from the view above. Drawn here rather than with
        // `chrome_edge`, which puts its line under a panel; this one goes on top.
        ui.painter().hline(
            strip.x_range(),
            strip.top() + azur::desktop::SEAM * 0.5,
            egui::Stroke::new(azur::desktop::SEAM, azur::desktop::seam(t)),
        );

        // Our own cost, on the right. Sub-millisecond frames are the whole point
        // of the direct-painting table, so the number is on permanent display --
        // and it is measured first so the counts on the left know how much room
        // they have to give up.
        let perf = format!(
            "frame {:.2} ms  (peak {:.2})   sample {} ms",
            self.frame_ms,
            self.frame_ms_peak,
            self.mon.interval()
        );
        let perf_w = ui
            .painter()
            .layout_no_wrap(perf.clone(), t.fonts.caption.clone(), t.text.tertiary)
            .size()
            .x;

        // `horizontal_centered`, not `horizontal`: it centres the caption in the
        // strip, and it makes the row claim the panel's full height. A `Frame`
        // reserves what its *content* measured, so a 16px row in a 24px panel
        // handed the 8px difference back -- and the table below took it and
        // painted over the seam.
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = space::S2;
            ui.add_space(space::S2);

            // Modes first, then counts. A frozen or paused list otherwise just
            // looks like the app has stopped, and those are exactly the labels a
            // narrow window would drop if they came last.
            if self.order_frozen {
                ui.add(
                    Badge::new("Order held (Ctrl)")
                        .small(true)
                        .variant(BadgeVariant::Accent),
                );
            }
            if self.mon.is_paused() {
                ui.add(
                    Badge::new("Paused")
                        .small(true)
                        .variant(BadgeVariant::Warning),
                );
            }

            let mut b = String::new();
            fmt::integer(&mut b, snap.cpu.processes as u64);
            status_pair(ui, t, perf_w, "Processes", &b, t.text.primary);
            fmt::integer(&mut b, snap.cpu.threads as u64);
            status_pair(ui, t, perf_w, "Threads", &b, t.text.secondary);
            fmt::integer(&mut b, snap.cpu.handles as u64);
            status_pair(ui, t, perf_w, "Handles", &b, t.text.secondary);

            status_pair(
                ui,
                t,
                perf_w,
                "CPU",
                &format!("{:.1}%", snap.cpu.total),
                t.load_text(snap.cpu.total),
            );
            let mut mem = String::new();
            fmt::bytes(&mut mem, snap.mem.in_use);
            let mut memt = String::new();
            fmt::bytes(&mut memt, snap.mem.total);
            status_pair(
                ui,
                t,
                perf_w,
                "Memory",
                &format!("{mem} / {memt}"),
                t.text.primary,
            );
        });

        // The readout is *painted*, not added as a widget.
        //
        // It never interacts, and as a widget it was the one thing in this row whose
        // rect never moved while its id did: egui numbers widgets by the order they
        // are added, `status_pair` drops pairs as the row narrows — which is what the
        // rail expanding does — and everything after a dropped pair is renumbered.
        // A rect that stays put while its id changes is what
        // `warn_if_rect_changes_id` paints a two-pixel red border around, and that
        // option is on in every debug build. Naming the group does not help: a child
        // `Ui` mixes the parent's running counter into its id by design, so it is
        // renumbered along with everything else (`egui::Ui`, `IdSource::Child`).
        //
        // Rounded, because a fractional edge is what the sibling `show_unaligned`
        // warning is for, and this is the one text in the strip placed by hand.
        let strip = ui.max_rect();
        ui.painter().text(
            Pos2::new(
                (strip.right() - space::S2).round(),
                strip.center().y.round(),
            ),
            Align2::RIGHT_CENTER,
            &perf,
            t.fonts.caption.clone(),
            t.text.tertiary,
        );
    }
}

/// Add a `label value` pair to the status bar, or nothing at all.
///
/// `reserve` is the width the frame-cost readout needs on the right. A pair that
/// would not fit before it is dropped whole: that reads as "there is no room for
/// this", where letting it clip reads as a rendering fault -- which is what a
/// 500px window used to look like.
fn status_pair(ui: &mut Ui, t: &Theme, reserve: f32, label: &str, value: &str, color: Color32) {
    let width = |s: &str| {
        ui.painter()
            .layout_no_wrap(s.to_owned(), t.fonts.caption.clone(), color)
            .size()
            .x
    };
    if ui.available_width() - reserve < width(label) + width(value) + space::S5 {
        return;
    }
    ui.add(
        Text::new(label)
            .variant(TextVariant::Caption)
            .color(TextColor::Tertiary),
    );
    ui.add(
        Text::new(value.to_owned())
            .variant(TextVariant::Caption)
            .explicit_color(color),
    );
    ui.add_space(space::S3);
}
