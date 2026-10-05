//! Application shell: navigation, toolbar, status bar, action dispatch.

use crate::sys::actions;
use crate::sys::sampler::{self, Monitor};
use crate::sys::snapshot::Snapshot;
use crate::ui::icons::{self, Glyph, IconCache};
use crate::ui::theme::{self, Theme};
use crate::ui::{details, fmt, performance, processes, services, startup, Action};

use egui::{Align2, Color32, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use std::time::{Duration, Instant};

/// Shared height for every interactive control in the toolbar.
const CONTROL_H: f32 = 28.0;

/// Navigation rail width, expanded and collapsed. Collapsed leaves room for a
/// 17px glyph plus the hover pill around it, and nothing else.
const NAV_W: f32 = 196.0;
const NAV_W_NARROW: f32 = 54.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Processes,
    Performance,
    Details,
    Services,
    Startup,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Tab::Processes,
        Tab::Performance,
        Tab::Details,
        Tab::Services,
        Tab::Startup,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Processes => "Processes",
            Self::Performance => "Performance",
            Self::Details => "Details",
            Self::Services => "Services",
            Self::Startup => "Startup apps",
        }
    }

    fn glyph(self) -> Glyph {
        match self {
            Self::Processes => Glyph::Processes,
            Self::Performance => Glyph::Performance,
            Self::Details => Glyph::Details,
            Self::Services => Glyph::Services,
            Self::Startup => Glyph::Startup,
        }
    }

    /// Tabs whose content is filtered by the search box.
    fn searchable(self) -> bool {
        !matches!(self, Self::Performance)
    }
}

/// A transient message shown in the status bar.
struct Toast {
    text: String,
    error: bool,
    at: Instant,
}

/// An action held back until the user confirms it.
///
/// The text is built when the action is raised, not when the dialog paints: by
/// then the snapshot has moved on, and a dialog that renamed the process it was
/// asking about would be worse than no dialog.
struct Confirm {
    action: Action,
    heading: String,
    detail: String,
    /// Label for the button that goes through with it.
    verb: &'static str,
}

pub struct App {
    mon: Monitor,
    theme: Theme,
    icons: IconCache,
    tab: Tab,
    filter: String,
    elevated: bool,

    processes: processes::ProcessesTab,
    details: details::DetailsTab,
    perf: performance::PerfTab,
    services: services::ServicesTab,
    startup: startup::StartupTab,

    toast: Option<Toast>,
    /// Wall-clock cost of the previous frame's `ui` call, shown in the status
    /// bar. This is a latency-focused app; the number should be visible.
    frame_ms: f32,
    frame_ms_peak: f32,
    last_generation: u64,
    /// Text to hand to the clipboard once painting is done.
    clipboard: Option<String>,
    /// Set after a successful elevated relaunch, so this instance closes.
    pending_exit: bool,
    /// Mirrors the active tab's Ctrl-freeze state, for the status bar.
    order_frozen: bool,
    /// Rail shows icons only.
    nav_collapsed: bool,
    /// Destructive action waiting on a yes/no.
    confirm: Option<Confirm>,
    /// Persisted geometry, column layouts and rail state.
    settings: crate::settings::Settings,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: crate::settings::Settings) -> Self {
        theme::install(&cc.egui_ctx);

        // The sampler pokes egui when a new snapshot lands. This is what makes
        // the app event-driven rather than polled: with no input and no new
        // data, `ui` is never called and the process uses no CPU at all.
        let ctx = cc.egui_ctx.clone();
        let mon = sampler::start_with_notify(1000, move || ctx.request_repaint());

        let mut processes = processes::ProcessesTab::new();
        let mut details = details::DetailsTab::new();
        let mut services = services::ServicesTab::new();
        let mut startup = startup::StartupTab::new();
        for (name, table) in [
            ("processes", &mut processes.table),
            ("details", &mut details.table),
            ("services", &mut services.table),
            ("startup", &mut startup.table),
        ] {
            settings.restore_table(name, table);
        }
        let nav_collapsed = settings.bool("nav.collapsed", false);

        Self {
            mon,
            theme: Theme::dark(),
            icons: IconCache::new(),
            tab: Tab::Processes,
            filter: String::new(),
            elevated: actions::is_elevated(),
            processes,
            details,
            perf: performance::PerfTab::new(),
            services,
            startup,
            toast: None,
            frame_ms: 0.0,
            frame_ms_peak: 0.0,
            last_generation: u64::MAX,
            clipboard: None,
            pending_exit: false,
            order_frozen: false,
            nav_collapsed,
            confirm: None,
            settings,
        }
    }

    /// Route an action: destructive ones go through a confirmation first.
    fn dispatch(&mut self, action: Action, snap: &Snapshot) {
        // Built while `action` is only borrowed, so it can be moved into the
        // `Confirm` afterwards without cloning it.
        let text = match &action {
            Action::EndTask { name, pid } => Some((
                format!("End {name}?"),
                format!(
                    "PID {pid}. Unsaved work will be lost, and a process ended this \
                     way gets no chance to shut down cleanly."
                ),
                "End process",
            )),
            Action::EndTree { key } => {
                let victims = self.processes.subtree_of(snap, *key);
                // `subtree_of` returns children first so a parent cannot respawn
                // them, which puts the process the user picked last.
                let name = victims
                    .last()
                    .map(|(_, n)| n.clone())
                    .unwrap_or_else(|| "this process".to_owned());
                Some((
                    format!("End {name} and its children?"),
                    format!(
                        "{} processes will be ended, deepest first. Unsaved work in \
                         any of them will be lost.",
                        victims.len()
                    ),
                    "End process tree",
                ))
            }
            _ => None,
        };
        match text {
            Some((heading, detail, verb)) => {
                self.confirm = Some(Confirm {
                    action,
                    heading,
                    detail,
                    verb,
                });
            }
            None => self.do_action(action, snap),
        }
    }

    /// The confirmation dialog. Returns once the answer has been acted on.
    fn confirm_dialog(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let Some(c) = &self.confirm else { return };
        let t = &self.theme;

        let mut answer: Option<bool> = None;
        egui::Modal::new(Id::new("confirm")).show(ctx, |ui| {
            ui.set_max_width(430.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                icons::glyph(ui.painter(), rect, Glyph::Shield, t.warn, t);
                ui.add_space(4.0);
                ui.label(egui::RichText::new(&c.heading).size(15.0).strong());
            });
            ui.add_space(6.0);
            ui.label(egui::RichText::new(&c.detail).color(t.text_dim));
            ui.add_space(14.0);

            // Cancel first and focused, so Enter and a stray double-click both
            // do the harmless thing.
            ui.horizontal(|ui| {
                let cancel = ui.button("Cancel");
                if cancel.clicked() {
                    answer = Some(false);
                }
                if !ctx.memory(|m| m.focused().is_some()) {
                    cancel.request_focus();
                }
                if ui
                    .add(egui::Button::new(egui::RichText::new(c.verb).color(t.crit)))
                    .clicked()
                {
                    answer = Some(true);
                }
            });
        });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(false);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            answer = Some(false);
        }

        match answer {
            Some(true) => {
                if let Some(c) = self.confirm.take() {
                    self.do_action(c.action, snap);
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    /// Push anything the UI may have changed into the settings store.
    ///
    /// Done in one place per frame rather than at each site that can change
    /// something: a column edge drag, a header click and a menu toggle all
    /// persist without any of them having to know persistence exists. `set`
    /// ignores writes that do not change the value, so this is free when idle.
    fn sync_settings(&mut self, ctx: &egui::Context) {
        self.settings.set_bool("nav.collapsed", self.nav_collapsed);
        // Borrowing both `self.settings` mutably and a tab immutably at once is
        // fine only because they are distinct fields; the loop is written out
        // for the same reason.
        let tables = [
            ("processes", &self.processes.table),
            ("details", &self.details.table),
            ("services", &self.services.table),
            ("startup", &self.startup.table),
        ];
        for (name, table) in tables {
            self.settings.sync_table(name, table);
        }

        // Geometry: outer rect, so restoring puts the frame back where it was
        // rather than offsetting the window by the caption height each launch.
        ctx.input(|i| {
            let vp = i.viewport();
            if vp.maximized == Some(true) || vp.minimized == Some(true) {
                // Saving a maximized frame would restore a window that fills the
                // screen but is not maximized, and could not be un-maximized to
                // anything sensible.
                self.settings.set_bool("window.maximized", vp.maximized == Some(true));
                return;
            }
            // Position from the outer rect and size from the inner one, to
            // match what the builder consumes on the way back in:
            // `with_position` places the frame, `with_inner_size` sizes the
            // client area. Saving the outer size and restoring it as the inner
            // size would grow the window by the caption height every launch.
            if let (Some(outer), Some(inner)) = (vp.outer_rect, vp.inner_rect) {
                self.settings.set_bool("window.maximized", false);
                self.settings.set("window.x", outer.min.x.round());
                self.settings.set("window.y", outer.min.y.round());
                self.settings.set("window.w", inner.width().round());
                self.settings.set("window.h", inner.height().round());
            }
        });

        self.settings.maybe_flush();
    }

    // -----------------------------------------------------------------
    // Chrome
    // -----------------------------------------------------------------

    fn nav_rail(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let t = &self.theme;
        let narrow = self.nav_collapsed;
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, CornerRadius::ZERO, t.nav);
        ui.painter().vline(
            rect.right() - 0.5,
            rect.y_range(),
            Stroke::new(1.0, t.border_soft),
        );

        // --- title / collapse toggle --------------------------------------
        //
        // The title is the control: there is nothing else for a 196px header to
        // do, and a separate chevron somewhere else would be one more thing to
        // find. Collapsed, only the glyph is left.
        let header_h = 36.0;
        let toggle = if narrow {
            Rect::from_center_size(
                Pos2::new(rect.center().x, rect.top() + header_h * 0.5),
                Vec2::splat(28.0),
            )
        } else {
            Rect::from_min_size(
                Pos2::new(rect.left() + 6.0, rect.top() + 4.0),
                Vec2::new(rect.width() - 12.0, header_h - 8.0),
            )
        };
        let tr = ui.interact(toggle, Id::new("nav-toggle"), Sense::click());
        if tr.hovered() {
            ui.painter().rect_filled(toggle, t.radius_sm, t.surface);
        }
        let glyph_c = if narrow {
            toggle.center()
        } else {
            Pos2::new(toggle.left() + 14.0, toggle.center().y)
        };
        icons::glyph(
            ui.painter(),
            Rect::from_center_size(glyph_c, Vec2::splat(16.0)),
            Glyph::Sidebar,
            if tr.hovered() { t.text } else { t.text_dim },
            t,
        );
        if !narrow {
            ui.painter().text(
                Pos2::new(toggle.left() + 32.0, toggle.center().y),
                Align2::LEFT_CENTER,
                "Task Manager",
                t.fonts.medium.clone(),
                t.text,
            );
        }
        let tr = tr.on_hover_text(if narrow {
            "Expand the sidebar"
        } else {
            "Collapse the sidebar"
        });
        if tr.clicked() {
            self.nav_collapsed = !narrow;
        }

        // --- tabs ----------------------------------------------------------
        let item_h = 38.0;
        let mut y = rect.top() + header_h + 8.0;
        let mut picked = None;

        for tab in Tab::ALL {
            let r = Rect::from_min_size(
                Pos2::new(rect.left() + 6.0, y),
                Vec2::new(rect.width() - 12.0, item_h),
            );
            let mut resp = ui.interact(r, Id::new("nav").with(tab as u8), Sense::click());
            let active = self.tab == tab;

            if active {
                ui.painter().rect_filled(r, t.radius_sm, t.surface_hi);
                // Fluent-style selection pip.
                ui.painter().rect_filled(
                    Rect::from_min_size(
                        Pos2::new(r.left() + 1.0, r.center().y - 9.0),
                        Vec2::new(3.0, 18.0),
                    ),
                    CornerRadius::same(2),
                    t.accent,
                );
            } else if resp.hovered() {
                ui.painter().rect_filled(r, t.radius_sm, t.surface);
            }

            let fg = if active { t.text } else { t.text_dim };
            let gx = if narrow { r.center().x } else { r.left() + 26.0 };
            icons::glyph(
                ui.painter(),
                Rect::from_center_size(Pos2::new(gx, r.center().y), Vec2::splat(17.0)),
                tab.glyph(),
                if active { t.accent } else { fg },
                t,
            );
            if narrow {
                // The label is the only thing identifying a tab now, so it has
                // to be reachable somehow.
                resp = resp.on_hover_text(tab.label());
            } else {
                ui.painter().text(
                    Pos2::new(r.left() + 45.0, r.center().y),
                    Align2::LEFT_CENTER,
                    tab.label(),
                    t.fonts.nav.clone(),
                    fg,
                );
            }

            if resp.clicked() {
                picked = Some(tab);
            }
            y += item_h + 2.0;
        }
        if let Some(tab) = picked {
            self.tab = tab;
        }

        // --- footer: live CPU and memory ----------------------------------
        // Only the horizontal padding changes with the rail's width. The
        // vertical geometry is shared, so the gauges sit at the same height in
        // both states -- a shorter footer put them over the elevation button.
        let pad = if narrow { 6.0 } else { 12.0 };
        let foot_h = 132.0;
        let foot = Rect::from_min_max(
            Pos2::new(rect.left() + pad, rect.bottom() - foot_h),
            Pos2::new(rect.right() - pad, rect.bottom() - 10.0),
        );

        // Collapsed there is no room for a label beside a number beside a
        // graph, so the label goes (the colour already says which is which)
        // and the value sits directly above a shorter sparkline.
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
                    t.fonts.small.clone(),
                    color,
                );
                r.top() + 14.0
            } else {
                ui.painter().text(
                    r.left_top(),
                    Align2::LEFT_TOP,
                    label,
                    t.fonts.small.clone(),
                    t.text_dim,
                );
                ui.painter().text(
                    r.right_top(),
                    Align2::RIGHT_TOP,
                    &b,
                    t.fonts.small.clone(),
                    color,
                );
                r.top() + 15.0
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
        let btn = if narrow {
            Rect::from_center_size(
                Pos2::new(rect.center().x, foot.bottom() - 13.0),
                Vec2::splat(26.0),
            )
        } else {
            Rect::from_min_size(
                Pos2::new(foot.left(), foot.bottom() - 26.0),
                Vec2::new(foot.width(), 26.0),
            )
        };
        if self.elevated {
            let gx = if narrow { btn.center().x } else { btn.left() + 9.0 };
            icons::glyph(
                ui.painter(),
                Rect::from_center_size(Pos2::new(gx, btn.center().y), Vec2::splat(13.0)),
                Glyph::Shield,
                t.ok,
                t,
            );
            if narrow {
                ui.interact(btn, Id::new("elev-state"), Sense::hover())
                    .on_hover_text("Running as administrator");
            } else {
                ui.painter().text(
                    Pos2::new(btn.left() + 21.0, btn.center().y),
                    Align2::LEFT_CENTER,
                    "Administrator",
                    t.fonts.small.clone(),
                    t.text_dim,
                );
            }
        } else {
            let resp = ui.interact(btn, Id::new("elevate"), Sense::click());
            let hov = resp.hovered();
            ui.painter().rect_filled(
                btn,
                t.radius_sm,
                if hov { t.surface_hi } else { t.surface },
            );
            let gx = if narrow { btn.center().x } else { btn.left() + 14.0 };
            icons::glyph(
                ui.painter(),
                Rect::from_center_size(Pos2::new(gx, btn.center().y), Vec2::splat(13.0)),
                Glyph::Shield,
                t.warn,
                t,
            );
            if !narrow {
                ui.painter().text(
                    Pos2::new(btn.left() + 26.0, btn.center().y),
                    Align2::LEFT_CENTER,
                    "Run as administrator",
                    t.fonts.small.clone(),
                    if hov { t.text } else { t.text_dim },
                );
            }
            if resp.clicked() {
                self.do_action(Action::Elevate, &Snapshot::default());
            }
            resp.on_hover_text(
                "Restart elevated to see all processes and to end protected ones",
            );
        }
    }

    /// The search field: a hand-drawn frame with a bare `TextEdit` inside.
    ///
    /// Placed at an explicit rect and deliberately *not* through
    /// `scope_builder`: pointing a child `Ui` at a rect that starts behind the
    /// parent's layout cursor rewinds that cursor, and the buttons after it then
    /// draw on top of the field.
    fn search_field(&mut self, ui: &mut Ui, rect: Rect) {
        let t = &self.theme;
        let focused = ui.memory(|m| m.has_focus(Id::new("filter-edit")));

        ui.painter().rect_filled(rect, t.radius_sm, t.surface);
        ui.painter().rect_stroke(
            rect,
            t.radius_sm,
            Stroke::new(1.0, if focused { t.accent } else { t.border }),
            egui::StrokeKind::Inside,
        );
        icons::glyph(
            ui.painter(),
            Rect::from_center_size(
                Pos2::new(rect.left() + 16.0, rect.center().y),
                Vec2::splat(13.0),
            ),
            Glyph::Search,
            t.text_faint,
            t,
        );

        // egui positions a galley from the top of the widget's rect, and a font's
        // row height is not symmetric around its ink (the line gap sits above
        // the ascender). Measuring the ink against the magnifier glyph -- drawn
        // exactly on the field's centre line -- put the text 1.5px high, but
        // correcting by the full amount reads high next to the placeholder's
        // descenders. The net is half a pixel *down*: negative, hence the sign.
        const TEXT_BASELINE_NUDGE: f32 = -0.5;
        let inner = Rect::from_min_max(
            Pos2::new(rect.left() + 28.0, rect.top() + 1.0 - TEXT_BASELINE_NUDGE),
            Pos2::new(rect.right() - 24.0, rect.bottom() - 1.0 - TEXT_BASELINE_NUDGE),
        );
        // Deliberately not `ui.put`: that justifies the widget to fill the rect,
        // and a stretched `TextEdit` draws its text (and hint) from the top, so
        // both sat visibly high. A `left_to_right(Align::Center)` child instead
        // gives the widget its natural height and centres it.
        let resp = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .max_rect(inner)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .id(Id::new("filter-edit"))
                            // The rounded frame above is ours, so the widget
                            // contributes no chrome of its own.
                            .frame(egui::Frame::NONE)
                            .hint_text("Filter by name, PID, publisher…")
                            .desired_width(inner.width()),
                    )
                },
            )
            .inner;

        // Ctrl+F focuses the box; Esc clears it.
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F)) {
            resp.request_focus();
        }
        if resp.has_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Escape))
            && !self.filter.is_empty()
        {
            self.filter.clear();
        }

        if !self.filter.is_empty() {
            let clear = Rect::from_center_size(
                Pos2::new(rect.right() - 14.0, rect.center().y),
                Vec2::splat(18.0),
            );
            let cr = ui.interact(clear, Id::new("clear-filter"), Sense::click());
            icons::glyph(
                ui.painter(),
                clear.shrink(4.0),
                Glyph::Close,
                if cr.hovered() { t.text } else { t.text_faint },
                t,
            );
            if cr.clicked() {
                self.filter.clear();
            }
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, snap: &Snapshot, action: &mut Option<Action>) {
        // Everything is positioned from explicit rects so the row sits at the
        // same height on every tab, including Performance, which has no search
        // field.
        let row0 = ui.available_rect_before_wrap();
        // Reserved on the right for the update-speed group. The full 300px pays
        // for the caption and a Refresh button; below ~560px of row there is not
        // room for both that and a usable filter box, so the group keeps only
        // what it needs and drops the caption (see `toolbar_right`).
        let right_w = if row0.width() < 560.0 { 116.0 } else { 300.0 };

        // One height for every control in the toolbar. egui sizes a button or a
        // combo as `max(text + 2 * button_padding.y, interact_size.y)`, which
        // lands on 27px with the default spacing -- one pixel shy of the
        // hand-drawn search field. Raising `interact_size.y` to the same value
        // makes the clamp the deciding term for all of them, so field, buttons
        // and combo end up pixel-identical instead of nearly identical.
        ui.spacing_mut().interact_size.y = CONTROL_H;

        let row = row0;
        let top = row.top() + ((row.height() - CONTROL_H) * 0.5).max(0.0);
        let mut x = row.left();

        if self.tab.searchable() {
            // Never let the field grow into the reserved right-hand group, and
            // never shrink it below something you can read a needle in.
            let avail = row.width() - right_w - 12.0;
            let w = 250.0f32.min(avail).max(90.0);
            let field = Rect::from_min_size(Pos2::new(x, top), Vec2::new(w, CONTROL_H));
            self.search_field(ui, field);
            x = field.right() + 8.0;
        }

        let left = Rect::from_min_max(
            Pos2::new(x, top),
            Pos2::new((row.right() - right_w).max(x), top + CONTROL_H),
        );
        if left.width() > 40.0 {
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(left)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| self.toolbar_left(ui, snap, action),
            );
        }

        let right = Rect::from_min_max(
            Pos2::new((row.right() - right_w).max(row.left()), top),
            Pos2::new(row.right(), top + CONTROL_H),
        );
        self.toolbar_right(ui, right);
    }

    /// Per-tab controls, laid out left to right.
    fn toolbar_left(&mut self, ui: &mut Ui, snap: &Snapshot, action: &mut Option<Action>) {
        let t = &self.theme;
        ui.spacing_mut().item_spacing.x = 6.0;
        {
            match self.tab {
                Tab::Processes => {
                    let sel = self.processes.selected;
                    let target = sel.and_then(|k| snap.find_by_key(k));
                    let enabled = target.is_some();
                    if ui
                        .add_enabled(enabled, egui::Button::new("End task"))
                        .clicked()
                    {
                        if let Some(i) = target {
                            *action = Some(Action::EndTask {
                                pid: snap.procs[i].pid,
                                name: snap.procs[i].name.to_string(),
                            });
                        }
                    }
                    // Grouping is no longer a button: it follows the sort (see
                    // `ProcessesTab::grouped`). Expand/collapse-all are gone
                    // too -- the tree is per-row and the keyboard covers it.
                    if self.processes.grouped() {
                        ui.label(
                            egui::RichText::new("Grouped by type")
                                .small()
                                .color(t.text_faint),
                        )
                        .on_hover_text(
                            "Sorting by Name groups into Apps and Background processes; \
                             sorting by any other column lists every process flat",
                        );
                    }
                }
                Tab::Details => {
                    let sel = self.details.selected;
                    let target = sel.and_then(|k| snap.find_by_key(k));
                    if ui
                        .add_enabled(target.is_some(), egui::Button::new("End task"))
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
                    ui.label(
                        egui::RichText::new(format!("{running} running / {total} total"))
                            .small()
                            .color(t.text_dim),
                    );
                }
                Tab::Startup => {
                    let (total, enabled) = self.startup.count();
                    ui.label(
                        egui::RichText::new(format!("{enabled} enabled / {total} total"))
                            .small()
                            .color(t.text_dim),
                    );
                }
                Tab::Performance => {}
            }
        }
    }

    /// Update-speed and manual refresh, positioned from explicit rects.
    ///
    /// Laid out by hand rather than with a `right_to_left` layout because
    /// egui's combo box is not vertically centred by its parent: `button_frame`
    /// anchors it to the *top* of the available rect and then grows it to at
    /// least `interact_size.y`. Next to a layout-centred `Button` that put the
    /// two controls a few pixels out of line. Sharing one explicit top edge
    /// makes them agree.
    fn toolbar_right(&mut self, ui: &mut Ui, area: Rect) {
        const COMBO_W: f32 = 96.0;
        const BUTTON_W: f32 = 72.0;

        let faint = self.theme.text_faint;
        let cy = area.center().y;
        let top = cy - CONTROL_H * 0.5;
        let mut right = area.right();

        // --- update-speed combo (rightmost) -------------------------------
        //
        // The popup entries must stay narrower than COMBO_W. egui's combo popup
        // runs `set_min_width(available_width())` with wrapping disabled, so an
        // entry wider than the button grows the popup, which grows
        // `available_width`, which grows the popup again: it never settles and a
        // scrollbar flickers in and out. Short labels keep the text inside;
        // `theme::list_items` keeps hover from adding width of its own.
        let combo_rect =
            Rect::from_min_size(Pos2::new(right - COMBO_W, top), Vec2::new(COMBO_W, CONTROL_H));
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(combo_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Min)),
            |ui| {
                egui::ComboBox::from_id_salt("rate")
                    .selected_text(self.speed().label())
                    .width(COMBO_W)
                    .show_ui(ui, |ui| {
                        crate::ui::theme::list_items(ui, &self.theme);
                        for opt in Speed::ALL {
                            if ui
                                .selectable_label(self.speed() == opt, opt.label())
                                .clicked()
                            {
                                match opt {
                                    Speed::Paused => self.mon.set_paused(true),
                                    _ => {
                                        self.mon.set_paused(false);
                                        self.mon.set_interval(opt.interval_ms());
                                    }
                                }
                            }
                        }
                    })
                    .response
                    .on_hover_text("How often the machine is sampled");
            },
        );
        right = combo_rect.left() - 8.0;

        // --- caption --------------------------------------------------------
        //
        // First thing to go in a narrow window: the combo below it already reads
        // "Normal" or "Paused", which is enough to know what it controls.
        let caption = "Update speed";
        let galley =
            ui.painter()
                .layout_no_wrap(caption.to_owned(), self.theme.fonts.small.clone(), faint);
        let cap_w = galley.size().x;
        if right - cap_w >= area.left() {
            ui.painter().galley(
                Pos2::new(right - cap_w, cy - galley.size().y * 0.5),
                galley,
                faint,
            );
            right -= cap_w + 10.0;
        }

        // --- manual refresh --------------------------------------------------
        //
        // Only while paused. Sampling on its own cadence makes the button a
        // no-op that happens to work, and a control that does nothing visible
        // is worse than no control; paused is exactly when it is the only way
        // to get a fresh reading. F5 still works either way.
        if self.mon.is_paused() && right - BUTTON_W >= area.left() {
            let btn_rect = Rect::from_min_size(
                Pos2::new(right - BUTTON_W, top),
                Vec2::new(BUTTON_W, CONTROL_H),
            );
            if ui
                .put(btn_rect, egui::Button::new("Refresh"))
                .on_hover_text("Sample immediately (F5)")
                .clicked()
            {
                self.mon.refresh_now();
            }
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

    fn status_bar(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let t = &self.theme;
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, CornerRadius::ZERO, t.nav);
        ui.painter()
            .hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, t.border_soft));

        let y = rect.center().y;
        let mut x = rect.left() + 12.0;

        // A toast takes over the left side while it is alive.
        let toast_alive = self
            .toast
            .as_ref()
            .is_some_and(|to| to.at.elapsed() < Duration::from_secs(6));
        if !toast_alive {
            self.toast = None;
        }

        if let Some(to) = &self.toast {
            let color = if to.error { t.crit } else { t.ok };
            let galley = ui.painter().layout_no_wrap(
                fmt::elide(&to.text, 120).into_owned(),
                t.fonts.small.clone(),
                color,
            );
            let w = galley.size().x;
            ui.painter()
                .galley(Pos2::new(x, y - galley.size().y * 0.5), galley, color);
            x += w;
        } else {
            let mut b = String::new();
            fmt::integer(&mut b, snap.cpu.processes as u64);
            let procs = b.clone();
            fmt::integer(&mut b, snap.cpu.threads as u64);
            let threads = b.clone();
            fmt::integer(&mut b, snap.cpu.handles as u64);
            let handles = b.clone();

            let mut mem = String::new();
            fmt::bytes(&mut mem, snap.mem.in_use);
            let mut memt = String::new();
            fmt::bytes(&mut memt, snap.mem.total);

            // Modes first, then counts. A frozen or paused list otherwise just
            // looks like the app has stopped, and those are exactly the labels a
            // narrow window would drop if they came last.
            let limit = rect.right() - 12.0;
            if self.order_frozen {
                x = status_pair(ui, t, x, y, limit, "", "Order held (Ctrl)", t.accent);
            }
            if self.mon.is_paused() {
                x = status_pair(ui, t, x, y, limit, "", "Paused", t.warn);
            }
            for (label, value, color) in [
                ("Processes", procs.as_str(), t.text),
                ("Threads", threads.as_str(), t.text_dim),
                ("Handles", handles.as_str(), t.text_dim),
            ] {
                x = status_pair(ui, t, x, y, limit, label, value, color);
            }
            x = status_pair(
                ui,
                t,
                x,
                y,
                limit,
                "CPU",
                &format!("{:.1}%", snap.cpu.total),
                t.load_text(snap.cpu.total),
            );
            x = status_pair(ui, t, x, y, limit, "Memory", &format!("{mem} / {memt}"), t.text);
        }

        // Right: our own cost. Sub-millisecond frames are the whole point of the
        // direct-painting table, so the number is on permanent display -- but not
        // at the price of overprinting the counts on the left, which is what
        // happened once the window could be narrowed to 500px. `x` is where the
        // left group ended, so the two can be checked against each other.
        let perf = format!(
            "frame {:.2} ms  (peak {:.2})   sample {} ms",
            self.frame_ms,
            self.frame_ms_peak,
            self.mon.interval()
        );
        let galley = ui
            .painter()
            .layout_no_wrap(perf, t.fonts.small.clone(), t.text_faint);
        if rect.right() - 12.0 - galley.size().x > x + 12.0 {
            ui.painter().galley(
                Pos2::new(
                    rect.right() - 12.0 - galley.size().x,
                    y - galley.size().y * 0.5,
                ),
                galley,
                t.text_faint,
            );
        }
    }

    // -----------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------

    fn do_action(&mut self, action: Action, snap: &Snapshot) {
        let result = match action {
            Action::EndTask { pid, name } => actions::end_task(pid, &name),
            Action::EndTree { key } => {
                let targets = self.processes.subtree_of(snap, key);
                if targets.is_empty() {
                    Err("That process is no longer running.".into())
                } else {
                    actions::end_tree(&targets)
                }
            }
            Action::SetPriority { pid, name, prio } => {
                actions::set_priority(pid, &name, prio)
            }
            Action::OpenFileLocation { path } => actions::open_file_location(&path),
            Action::OpenProperties { path } => actions::open_properties(&path),
            Action::GoToDetails(key) => {
                self.tab = Tab::Details;
                self.details.reveal(key);
                // Jumping to a filtered-out row would look like nothing happened.
                self.filter.clear();
                Ok(String::new())
            }
            Action::CopyRow(text) => {
                let n = text.len();
                self.clipboard = Some(text);
                Ok(format!("Copied {n} characters."))
            }
            Action::Elevate => match actions::relaunch_elevated() {
                Ok(_) => {
                    // The elevated instance is starting; this one should go.
                    self.pending_exit = true;
                    Ok(String::new())
                }
                Err(e) => Err(e),
            },
        };

        match result {
            Ok(msg) if !msg.is_empty() => self.toast(msg, false),
            Ok(_) => {}
            Err(e) => self.toast(e, true),
        }

        // Any action that changed the system should be reflected right away
        // instead of at the next scheduled sample.
        self.mon.refresh_now();
    }

    fn toast(&mut self, text: String, error: bool) {
        self.toast = Some(Toast {
            text,
            error,
            at: Instant::now(),
        });
    }
}

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

    /// Kept short: these strings must fit inside the combo button's width, or
    /// egui's popup sizing oscillates (see `toolbar_right`).
    fn label(self) -> &'static str {
        match self {
            Self::High => "High",
            Self::Normal => "Normal",
            Self::Low => "Low",
            Self::VeryLow => "Very low",
            Self::Paused => "Paused",
        }
    }

    fn interval_ms(self) -> u32 {
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

/// Draw a `Label  value` pair and return the x to continue at.
/// Draw a `label value` pair and return where the next one starts.
///
/// Skips the pair entirely if it would not fit before `limit`, and returns `x`
/// unchanged. Dropping a whole pair reads as "there is no room for this"; letting
/// it clip reads as a rendering fault, which is what a 500px window used to look
/// like.
#[allow(clippy::too_many_arguments)]
fn status_pair(
    ui: &Ui,
    t: &Theme,
    x: f32,
    y: f32,
    limit: f32,
    label: &str,
    value: &str,
    color: Color32,
) -> f32 {
    let p = ui.painter();
    let lg = p.layout_no_wrap(label.to_owned(), t.fonts.small.clone(), t.text_faint);
    let vg = p.layout_no_wrap(value.to_owned(), t.fonts.small.clone(), color);
    let lw = lg.size().x;
    let vw = vg.size().x;
    let gap = if label.is_empty() { 0.0 } else { 6.0 };
    if x + lw + gap + vw > limit {
        return x;
    }

    p.galley(Pos2::new(x, y - lg.size().y * 0.5), lg, t.text_faint);
    p.galley(Pos2::new(x + lw + gap, y - vg.size().y * 0.5), vg, color);

    x + lw + gap + vw + 24.0
}

impl eframe::App for App {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        self.theme.bg.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        #[cfg(feature = "memtrace")]
        {
            crate::memtrace::note_frame();
            // The app is meant to paint only when a sample lands or input
            // arrives. Anything else asking for a repaint multiplies whatever
            // each frame costs, so name it.
            for c in ui.ctx().repaint_causes() {
                eprintln!("[repaint] {}:{} {}", c.file, c.line, c.reason);
            }
        }

        let t0 = Instant::now();
        let ctx = ui.ctx().clone();

        // One atomic load; the snapshot is then stable for the whole frame.
        let guard = self.mon.snapshot();
        let snap: &Snapshot = &guard;

        if snap.generation != self.last_generation {
            self.last_generation = snap.generation;
            self.processes.mark_dirty();
            self.details.mark_dirty();
        }

        self.icons.pump(&ctx);
        let mut action: Option<Action> = None;
        // Ctrl freezes the row order on the table tabs; mirrored here so the
        // status bar can say so.
        self.order_frozen =
            ctx.input(|i| i.modifiers.ctrl) && matches!(self.tab, Tab::Processes | Tab::Details);

        // Global shortcuts.
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::F5) {
                self.mon.refresh_now();
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) {
                let key = match self.tab {
                    Tab::Processes => self.processes.selected,
                    Tab::Details => self.details.selected,
                    _ => None,
                };
                if let Some(k) = key.and_then(|k| snap.find_by_key(k)) {
                    action = Some(Action::EndTask {
                        pid: snap.procs[k].pid,
                        name: snap.procs[k].name.to_string(),
                    });
                }
            }
        });

        let theme = Theme::dark();

        // Paint-path bisect, present only in a memtrace build.
        #[cfg(feature = "memtrace")]
        let level = crate::memtrace::ui_level();
        #[cfg(feature = "memtrace")]
        if level == 0 {
            return;
        }

        egui::Panel::left("nav")
            .exact_size(if self.nav_collapsed { NAV_W_NARROW } else { NAV_W })
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                self.nav_rail(ui, snap);
            });

        #[cfg(feature = "memtrace")]
        if level < 2 {
            return;
        }

        egui::Panel::bottom("status")
            .exact_size(28.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                self.status_bar(ui, snap);
            });

        egui::Panel::top("toolbar")
            .exact_size(46.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme.bg)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                self.toolbar(ui, snap, &mut action);
            });

        #[cfg(feature = "memtrace")]
        if level < 3 {
            return;
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme.bg)
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 12,
                        top: 0,
                        bottom: 8,
                    }),
            )
            .show(ui, |ui| match self.tab {
                Tab::Processes => self.processes.ui(
                    ui,
                    &theme,
                    snap,
                    &self.mon.meta,
                    &mut self.icons,
                    &self.filter,
                    &mut action,
                ),
                Tab::Details => self.details.ui(
                    ui,
                    &theme,
                    snap,
                    &self.mon.meta,
                    &self.filter,
                    &mut action,
                ),
                Tab::Performance => self.perf.ui(ui, &theme, snap),
                Tab::Services => {
                    self.services.ui(ui, &theme, snap, &self.filter, &mut action)
                }
                Tab::Startup => self.startup.ui(ui, &theme, &self.filter),
            });

        // Actions run after painting, so they never mutate state the frame is
        // still reading.
        if let Some(a) = action {
            self.dispatch(a, snap);
        }
        self.confirm_dialog(&ctx, snap);

        if let Some(text) = self.clipboard.take() {
            ctx.copy_text(text);
        }

        // Keep VRAM bounded: forget icons for executables no longer listed.
        if snap.generation % 32 == 0 {
            let meta = &self.mon.meta;
            let live: rustc_hash::FxHashSet<std::sync::Arc<str>> = snap
                .procs
                .iter()
                .filter_map(|p| meta.get(p.key()).and_then(|m| m.path.clone()))
                .collect();
            self.icons.trim(|k| live.contains(k));
        }

        if self.pending_exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        self.sync_settings(&ctx);

        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        // Smooth the readout, but keep a true peak.
        self.frame_ms = self.frame_ms * 0.85 + ms * 0.15;
        self.frame_ms_peak = self.frame_ms_peak.max(ms);
    }

    /// Last chance to write settings.
    ///
    /// `App::save` is only called when eframe has a storage backend, which needs
    /// its `persistence` feature; this build does its own persistence, so the
    /// write has to hang off shutdown instead. `sync_settings` also flushes on a
    /// two-second delay while running, so a kill still loses at most that.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.flush();
    }
}
