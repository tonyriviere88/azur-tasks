//! Application shell: navigation, toolbar, status bar, action dispatch.

use crate::sys::actions;
use crate::sys::sampler::{self, Monitor};
use crate::sys::snapshot::Snapshot;
use crate::ui::icons::{self, Glyph, IconCache};
use crate::ui::theme::{self, Theme};
use crate::ui::app_mark::AppMark;
use crate::ui::{
    details, fmt, inspector, performance, processes, sensors, services, startup, Action,
};

use azur_egui_theme as azur;
use azur::components::{
    Badge, BadgeVariant, Button, Dialog, DialogSize, IconButton,
    MessageBar, NavItem, NavigationRail, SearchBox, Select, Size, Text, TextColor, TextVariant,
    TitleBar, TitleBarAction, Toast, Toaster, Variant,
};
use azur::tokens::{row, space};
use azur::components::Intent;
use egui::{Align, Align2, Color32, Id, Layout, Pos2, Rect, Sense, Ui, Vec2};
use std::time::Instant;

/// Height of the command row under the title bar: the tallest control in it with
/// `space-1` of air above and below.
///
/// Everything in the row is a small control at the desktop density -- search box,
/// buttons, the cadence picker -- so the row is exactly `control::SMALL` plus its
/// padding. Anything taller added here (a `CommandBar` frames its buttons in
/// `space-1` and stands 4px higher) has to be counted in, or the row overflows its
/// own panel and paints over the seam under it.
const TOOLBAR_H: f32 = azur::tokens::control::SMALL + PANEL_PAD_Y * 2.0;

/// The air between a chrome strip's edge and the controls in it. `space-2`: at the
/// dense density the strip is only as tall as one control, and this is what keeps
/// the control off the seam -- padding inside a surface, not a margin around a
/// floating one.
const PANEL_PAD_Y: f32 = space::S2;

/// The status bar: `row::TIGHT`, which is the height the design system gives a
/// status line -- one caption line and a point of air. It carries small badges
/// (`space-5` tall) and nothing else.
const STATUS_H: f32 = row::TIGHT;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Processes,
    Performance,
    Sensors,
    Details,
    Services,
    Startup,
}

impl Tab {
    const ALL: [Tab; 6] = [
        Tab::Processes,
        Tab::Performance,
        Tab::Sensors,
        Tab::Details,
        Tab::Services,
        Tab::Startup,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Processes => "Processes",
            Self::Performance => "Performance",
            Self::Sensors => "Sensors",
            Self::Details => "Details",
            Self::Services => "Services",
            Self::Startup => "Startup apps",
        }
    }

    /// The rail glyph, as an icon a component can take.
    fn icon(self) -> azur::icons::Icon<'static> {
        match self {
            Self::Processes => &icons::icon::processes,
            Self::Performance => &icons::icon::performance,
            Self::Sensors => &icons::icon::gauge,
            Self::Details => &icons::icon::details,
            Self::Services => &icons::icon::services,
            Self::Startup => &icons::icon::startup,
        }
    }

    /// Tabs whose content is filtered by the search box.
    fn searchable(self) -> bool {
        !matches!(self, Self::Performance | Self::Sensors)
    }

    /// Whether this tab's content is a table that runs to the window's edges.
    ///
    /// A table paints its own panel and is welded to the strips above and below it
    /// by seams, so the central panel gives it no margin at all. The two card tabs
    /// want the opposite: cards float on the canvas, and the canvas has to be
    /// visible around them.
    ///
    /// The same tabs as [`Self::searchable`] today, and a different question -- one
    /// is about what a filter would narrow, the other about what the content is
    /// shaped like. A tab could answer them differently.
    fn tabular(self) -> bool {
        !matches!(self, Self::Performance | Self::Sensors)
    }
}

/// An action held back until the user confirms it.
///
/// The text is built when the action is raised, not when the dialog paints: by
/// then the snapshot has moved on, and a dialog that renamed the process it was
/// asking about would be worse than no dialog.
struct Confirm {
    action: Action,
    heading: String,
    /// What the action will do, in the dialog's body.
    detail: String,
    /// What it costs, in a warning `MessageBar` under it. Split from `detail` so
    /// the two do not say the same thing twice, which is what a body that ends in
    /// "unsaved work will be lost" above a bar that says the same reads as.
    risk: &'static str,
    /// Label for the button that goes through with it.
    verb: &'static str,
}

pub struct App {
    mon: Monitor,
    theme: Theme,
    icons: IconCache,
    /// The product mark in the title bar. Not part of `icons`, which caches one
    /// texture per executable path: this is a single image with no key.
    mark: AppMark,
    tab: Tab,
    filter: String,
    /// Whether the filter field had the keyboard focus when this frame's input
    /// arrived — which is not the same question as `has_focus()`, and the difference
    /// is the whole reason this exists. See where it is set, and the end of [`Self::ui`].
    filter_focused: bool,
    elevated: bool,

    processes: processes::ProcessesTab,
    details: details::DetailsTab,
    perf: performance::PerfTab,
    sensors: sensors::SensorsTab,
    services: services::ServicesTab,
    startup: startup::StartupTab,

    /// Action results, as Azur toasts in the corner of the window.
    toaster: Toaster,
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
    /// The modules / handles / threads panel under the process list.
    inspector: inspector::InspectorPanel,
    /// Whether that panel is open. Off at every launch, and never persisted: it is
    /// a look at one process, which is not a thing a window can still be doing
    /// tomorrow. It also costs a read per sample while it is up.
    inspector_open: bool,
    /// Mirrors the window's maximized state, so the title bar can offer restore
    /// instead of maximize. The platform owns the truth; this is what we last
    /// asked for or last saw.
    maximized: bool,
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
        let mut sensors = sensors::SensorsTab::new();
        settings.restore_sensors("sensors", &mut sensors.layout);
        // Collapsed by default: the rail's icons are captioned by the tab heading
        // beside them, and the width it gives back goes to the table, which is what
        // a reader came for. Expanding is one click and it is remembered.
        let nav_collapsed = settings.bool("nav.collapsed", true);
        let maximized = settings.bool("window.maximized", false);


        Self {
            mon,
            theme: Theme::dark(),
            icons: IconCache::new(),
            mark: AppMark::default(),
            tab: Tab::Processes,
            filter: String::new(),
            filter_focused: false,
            elevated: actions::is_elevated(),
            processes,
            details,
            perf: performance::PerfTab::new(),
            sensors,
            services,
            startup,
            toaster: Toaster::new(),
            frame_ms: 0.0,
            frame_ms_peak: 0.0,
            last_generation: u64::MAX,
            clipboard: None,
            pending_exit: false,
            order_frozen: false,
            nav_collapsed,
            inspector: inspector::InspectorPanel::new(&cc.egui_ctx),
            // Not restored, and not saved: the panel is a look at one process, and a
            // window that came back with it open would be answering a question from
            // the last session.
            inspector_open: false,
            maximized,
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
                format!("PID {pid} will be ended immediately."),
                "It gets no chance to shut down cleanly, and unsaved work in it is lost.",
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
                    format!("{} processes will be ended, deepest first.", victims.len()),
                    "None of them gets a chance to shut down cleanly.",
                    "End process tree",
                ))
            }
            _ => None,
        };
        match text {
            Some((heading, detail, risk, verb)) => {
                self.confirm = Some(Confirm {
                    action,
                    heading,
                    detail,
                    risk,
                    verb,
                });
            }
            None => self.do_action(action, snap),
        }
    }

    /// The confirmation dialog. Returns once the answer has been acted on.
    fn confirm_dialog(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let Some(c) = &self.confirm else { return };
        // Copied out so the dialog's closures do not borrow `self.confirm` while
        // the answer is being written back to it. Only while a dialog is open.
        let (heading, detail, risk, verb) = (c.heading.clone(), c.detail.clone(), c.risk, c.verb);

        let mut answer: Option<bool> = None;
        // `open` is what the dialog writes a dismissal into -- the scrim, the
        // close button and Escape all clear it.
        let mut open = true;
        Dialog::new(&heading, &mut open)
            .size(DialogSize::Small)
            .footer(|ui| {
                // Right-to-left inside the footer: the action added first lands
                // furthest right, where the primary button belongs.
                let go = ui.add(Button::new(verb).variant(Variant::Danger));
                if go.clicked() {
                    answer = Some(true);
                }
                if ui
                    .add(Button::new("Cancel").variant(Variant::Subtle))
                    .clicked()
                {
                    answer = Some(false);
                }
                // The dialog opens focused on the button that goes through with it,
                // so Enter confirms -- egui turns Enter or Space on a focused
                // clickable widget into a click, and nothing else here has to read
                // the key. Escape, the ✕ and the scrim are the way out.
                //
                // Only when nothing holds focus, which is the frame the dialog
                // opened: the modal surrenders the focus of whatever was behind it,
                // and re-requesting every frame would pin it here and stop Tab from
                // reaching Cancel.
                if !ui.ctx().memory(|m| m.focused().is_some()) {
                    go.request_focus();
                }
            })
            .show(ctx, |ui| {
                ui.add(Text::new(&detail).color(TextColor::Secondary));
                ui.add_space(space::S3);
                ui.add(MessageBar::new(risk).intent(Intent::Warning));
            });

        if !open {
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
        self.settings.sync_sensors("sensors", &self.sensors.layout);

        // Geometry: outer rect, so restoring puts the frame back where it was
        // rather than offsetting the window by the caption height each launch.
        ctx.input(|i| {
            let vp = i.viewport();
            // The title bar offers restore instead of maximize from this.
            if let Some(max) = vp.maximized {
                self.maximized = max;
            }
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

    /// The navigation rail: Azur's, with this app's live footer beneath it.
    fn nav_rail(&mut self, ui: &mut Ui, snap: &Snapshot) -> bool {
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
    fn toolbar(&mut self, ui: &mut Ui, snap: &Snapshot, action: &mut Option<Action>) {
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

    /// Drop the selection on whichever tab has one.
    ///
    /// Performance's `selected` is not one of these: it names the resource whose graph
    /// is on screen, which is a view and not a row, and a tab showing nothing would be
    /// a tab with a hole in it.
    fn clear_selection(&mut self) {
        match self.tab {
            Tab::Processes => self.processes.clear_selection(),
            Tab::Details => self.details.clear_selection(),
            Tab::Services => self.services.clear_selection(),
            Tab::Startup => self.startup.clear_selection(),
            Tab::Performance | Tab::Sensors => {}
        }
    }

    /// The process the inspector is about: whatever the active list has selected.
    ///
    /// The name comes along because the panel is docked *under* the list and has to
    /// say which row it belongs to -- with the keyboard moving the selection, the
    /// two can otherwise be read as unrelated.
    fn inspected(&self, snap: &Snapshot) -> (Option<u32>, Option<String>) {
        let key = match self.tab {
            Tab::Processes => self.processes.selected,
            Tab::Details => self.details.selected,
            _ => None,
        };
        match key.and_then(|k| snap.find_by_key(k)) {
            Some(i) => (
                Some(snap.procs[i].pid),
                Some(snap.procs[i].name.to_string()),
            ),
            None => (None, None),
        }
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
        if matches!(self.tab, Tab::Processes | Tab::Details) {
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
                let sel = self.processes.selected;
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
                let sel = self.details.selected;
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
            // Nothing to act on: both are read-outs.
            Tab::Performance | Tab::Sensors => {}
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
        if matches!(self.tab, Tab::Processes | Tab::Details) {
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

    fn status_bar(&mut self, ui: &mut Ui, snap: &Snapshot) {
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
            Action::NewTask { at } => actions::run_new_task(at),
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
            Action::SetStartupEnabled { entry, enable } => {
                let result = crate::sys::startup::set_enabled(&entry, enable);
                // Re-read the registry rather than assume the write took: if it
                // was refused, the row must keep saying what Windows says.
                self.startup.invalidate();
                result
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
        self.toaster.push(Toast::new(text).intent(if error {
            Intent::Danger
        } else {
            Intent::Success
        }));
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

    /// Kept short: these are the options of a fixed-width `Select`, and a label
    /// wider than the trigger truncates rather than widening it.
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

/// The point a native window should hang centred under `widget` from, in screen
/// pixels — the coordinates Win32 places a window in, and the ones egui never deals
/// in.
///
/// A *centre* and not a left edge, because centring needs the dialog's width and
/// nothing here knows it: only the code holding the window can subtract half of it.
///
/// One multiplication converts both terms because they are in the same unit: a
/// widget rectangle is in points from the top-left of the window's content, and the
/// viewport's `inner_rect` is that content's own position in points — egui-winit
/// divides the physical rectangle by exactly the `pixels_per_point` used here, so a
/// display scale or a zoom factor cannot make the two disagree.
///
/// `None` before the platform has reported a position, which leaves the placement to
/// Windows rather than guessing at the origin.
pub fn screen_anchor_below(ctx: &egui::Context, widget: Rect) -> Option<(i32, i32)> {
    let content = ctx.input(|i| i.viewport().inner_rect)?;
    let ppp = ctx.pixels_per_point();
    // `space-2` of air under the button, which is the gap a menu opens with: the
    // dialog reads as hanging off the control rather than covering its edge.
    let x = (content.min.x + widget.center().x) * ppp;
    let y = (content.min.y + widget.bottom() + space::S2) * ppp;
    Some((x.round() as i32, y.round() as i32))
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

impl eframe::App for App {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        self.theme.bg.canvas.to_normalized_gamma_f32()
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
        // A menu belongs to a moment: click into another window with one open and
        // egui would keep it there, pinned over content it no longer describes --
        // over a selection, in the case of a context menu, that the click may have
        // changed. Nothing here latches a popup's state itself, so the answer is
        // not needed.
        let _ = azur::desktop::close_popups_on_blur(&ctx);
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
            // Alt+D toggles the inspector, on the tabs that have one. Consumed
            // whatever the tab, so it never reaches a text field as a character on
            // the tabs where it would do nothing.
            if i.consume_key(egui::Modifiers::ALT, egui::Key::D)
                && matches!(self.tab, Tab::Processes | Tab::Details)
            {
                self.inspector_open = !self.inspector_open;
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

        // --- window chrome -------------------------------------------------
        //
        // The platform's caption is off (see `main`), so the title bar is Azur's:
        // 32px of `background-layer` with the window buttons at the end.
        //
        // The mark is resolved before the panel, not inside it: uploading a texture
        // needs the context, and the closure below already holds the borrow of `self`
        // the window buttons write through.
        let mark = self.mark.icon(&ctx);
        egui::Panel::top("title")
            .exact_size(row::TITLE_BAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                match TitleBar::new("Azur Task Manager")
                    // The product mark, not a nav glyph — and the same artwork the
                    // taskbar button and the `.exe` in Explorer wear, reduced to the
                    // size this box is in pixels rather than redrawn for it.
                    .icon(&mark)
                    .maximized(self.maximized)
                    .show(ui)
                {
                    Some(TitleBarAction::Close) => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Some(TitleBarAction::Minimize) => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    Some(TitleBarAction::Maximize) | Some(TitleBarAction::Restore) => {
                        self.maximized = !self.maximized;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(self.maximized));
                    }
                    Some(TitleBarAction::Drag) => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    None => {}
                }
            });

        // Paint-path bisect, present only in a memtrace build.
        #[cfg(feature = "memtrace")]
        let level = crate::memtrace::ui_level();
        #[cfg(feature = "memtrace")]
        if level == 0 {
            return;
        }

        // The rail owns its two widths (220px, or 48px collapsed); the panel asks
        // it rather than repeating them.
        let mut probe = 0usize;
        let rail_w = NavigationRail::new(&mut probe)
            .collapsed(self.nav_collapsed)
            .width();
        let mut elevate = false;
        egui::Panel::left("nav")
            .exact_size(rail_w)
            .resizable(false)
            .show_separator_line(false)
            // A structural panel: `background-layer`, square, unringed, and no
            // margin of its own. The rail paints the seam down its own right edge.
            .frame(azur::desktop::panel(&theme))
            .show(ui, |ui| {
                elevate = self.nav_rail(ui, snap);
            });
        if elevate {
            self.do_action(Action::Elevate, &Snapshot::default());
        }

        #[cfg(feature = "memtrace")]
        if level < 2 {
            return;
        }

        egui::Panel::bottom("status")
            .exact_size(STATUS_H)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                self.status_bar(ui, snap);
            });

        // The strip is a panel like the rail: its own fill, square, and divided
        // from the view below by a seam rather than by a gap. `space-2` at the
        // sides so the filter box starts where the table's first column does, and
        // `PANEL_PAD_Y` above and below to keep the controls off the seam.
        let bar = egui::Panel::top("toolbar")
            .exact_size(TOOLBAR_H)
            .resizable(false)
            .show_separator_line(false)
            // `space-3` at the sides, which is the same 8px the table insets its
            // cell text by, so the filter box starts where the first column does.
            .frame(azur::desktop::panel(&theme).inner_margin(egui::Margin::symmetric(
                space::S3 as i8,
                PANEL_PAD_Y as i8,
            )))
            .show(ui, |ui| {
                self.toolbar(ui, snap, &mut action);
            });
        // The panel's own rect, not the `Ui`'s: inside the closure the margin has
        // already been taken off, and the seam belongs to the panel's edge.
        azur::components::chrome_edge(ui, bar.response.rect);

        #[cfg(feature = "memtrace")]
        if level < 3 {
            return;
        }

        egui::CentralPanel::default()
            .frame(
                // A table is the window's structure and runs to its edges: no
                // margin, and the seams above and below it are the toolbar's and
                // the status bar's. The two card tabs are the other case -- a card
                // floats on the canvas, so the canvas has to show around it.
                egui::Frame::NONE.inner_margin(if self.tab.tabular() {
                    egui::Margin::ZERO
                } else {
                    // `space-3`, the same 8px the toolbar keeps at its sides and the
                    // table insets its first column by: one line down the window's
                    // left edge whatever the tab.
                    egui::Margin::same(space::S3 as i8)
                }),
            )
            .show(ui, |ui| {
                // The inspector takes the bottom of whichever list tab is up, before
                // the list claims the rest -- so the list shrinks and the panel is
                // docked to it rather than floating over it. Resizable, because how
                // much of a process table you are willing to give up to see its
                // threads is not something this app can decide for you.
                if self.inspector_open && matches!(self.tab, Tab::Processes | Tab::Details) {
                    let (pid, name) = self.inspected(snap);
                    let view = ui.available_height();
                    // `default_size` is consulted only when egui has no remembered
                    // size for this panel, which is the first time it is shown in a
                    // session and never again -- so half the view to begin with, and
                    // from then on the height the reader dragged it to, held in egui's
                    // own panel state for as long as the process lives. Unchanged by a
                    // tab change, a new sample, a reopen; gone at the next launch,
                    // because none of it is written to the settings file.
                    //
                    // `max_size` is the one thing that overrules it, and it is a clamp
                    // rather than a resize: the list keeps `LIST_MIN_H` when the window
                    // is too short for both.
                    let panel = egui::Panel::bottom("inspector")
                        .default_size(view * inspector::FIRST_SHARE)
                        .min_size(inspector::min_h())
                        .max_size((view - inspector::LIST_MIN_H).max(inspector::min_h()))
                        .resizable(true)
                        .show_separator_line(false)
                        .frame(azur::desktop::panel(&theme))
                        .show(ui, |ui| {
                            self.inspector.ui(
                                ui,
                                &theme,
                                pid,
                                name.as_deref(),
                                snap.generation,
                                &mut action,
                            )
                        });
                    if panel.inner {
                        self.inspector_open = false;
                    }
                    // The seam that welds it to the list above, and the height to
                    // remember for next time.
                    let rect = panel.response.rect;
                    ui.painter().hline(
                        rect.x_range(),
                        rect.top() + azur::desktop::SEAM * 0.5,
                        egui::Stroke::new(azur::desktop::SEAM, azur::desktop::seam(&theme)),
                    );
                }
                match self.tab {
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
                Tab::Sensors => self.sensors.ui(ui, &theme, snap),
                Tab::Services => {
                    self.services.ui(ui, &theme, snap, &self.filter, &mut action)
                }
                Tab::Startup => self.startup.ui(ui, &theme, &self.filter, &mut action),
                }
            });

        // The window's resize edges, after the panels that would otherwise cover
        // them.
        //
        // A borderless window has none of its own: `with_decorations(false)` is what
        // lets this app draw Azur's `TitleBar`, and the frame it gives up includes
        // the eight strips Windows resizes by. `WS_THICKFRAME` survives, so the
        // window looks resizable to the shell, but every pixel of it — measured with
        // `WM_NCHITTEST` at all four edges and the corners — answers `HTCLIENT`, and
        // Windows never begins a drag. These are `Order::Middle` areas that set the
        // right cursor and hand the drag back to the window manager.
        azur::components::resize_handles(&ctx);

        // Actions run after painting, so they never mutate state the frame is
        // still reading.
        if let Some(a) = action {
            self.dispatch(a, snap);
        }
        self.confirm_dialog(&ctx, snap);

        // **Everything Escape means, in one place.** Filter text first, then the
        // selection -- and the whole policy is here rather than at the two controls it
        // acts on, because "was the user in the search box?" cannot be answered from
        // inside the search box on this frame: egui has already dropped the focused
        // widget by the time any widget runs (see [`Self::filter_focused`]).
        //
        // Last in the frame, because two things have a better claim on the key and
        // both have had their turn by here:
        //
        // - a confirmation is a modal, and `egui::Modal` *consumes* Escape to close, so
        //   this sees nothing. Which is why it sits after `confirm_dialog` rather than
        //   with the global shortcuts at the top: cancelling a dialog about the selected
        //   process and deselecting that process are opposite ends of one gesture.
        // - a context menu closes on Escape but does **not** consume it -- egui's popup
        //   only tests the key -- so the popup state is what has to be asked instead.
        //   `any_popup_open` reads this pass, and a menu is drawn during the tab body
        //   above, so by now it is registered.
        if !ctx.any_popup_open()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            if self.filter_focused && !self.filter.is_empty() {
                self.filter.clear();
            } else {
                self.clear_selection();
            }
        }

        // Toasts paint over everything, so they go last.
        self.toaster.show(&ctx);

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
    fn on_exit(&mut self) {
        self.settings.flush();
    }
}

#[cfg(test)]
mod tests {
    /// A bottom panel is only as tall as its content claims, and the inspector's
    /// content has to claim it -- which is the bug behind "the panel does not open at
    /// half the view" and "I cannot resize it when nothing is selected", both at once.
    ///
    /// `default_size` sizes the box the content lays out in; the height egui keeps is
    /// the one the content's frame measured, floored at `min_size`. A panel that only
    /// paints therefore collapses to that floor and springs back from every drag,
    /// because the dragged size is stored and then overruled by the content again on
    /// the next frame. This measures both halves of it.
    #[test]
    fn a_panel_is_only_as_tall_as_its_content_claims() {
        fn height(claim: bool) -> (f32, f32) {
            let ctx = egui::Context::default();
            crate::ui::theme::install(&ctx);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
            let (mut got, mut asked) = (0.0, 0.0);
            // Three passes: the first stores the panel state, and a height that only
            // holds for one frame is not a height.
            for _ in 0..3 {
                let input = egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                };
                let _ = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let view = ui.available_height();
                        asked = view * 0.5;
                        got = egui::Panel::bottom("probe")
                            .default_size(asked)
                            .min_size(109.0)
                            .max_size(view - 150.0)
                            .resizable(true)
                            .show(ui, |ui| {
                                if claim {
                                    ui.set_min_height(ui.available_height());
                                }
                                // Painting only, as the inspector's note does when no
                                // process is selected.
                                ui.painter().text(
                                    ui.max_rect().center(),
                                    egui::Align2::CENTER_CENTER,
                                    "note",
                                    egui::FontId::proportional(14.0),
                                    egui::Color32::WHITE,
                                );
                            })
                            .response
                            .rect
                            .height();
                        ui.label("the list below it");
                    });
                });
            }
            (asked, got)
        }

        // Without the claim: the floor, whatever it was opened at.
        let (asked, got) = height(false);
        assert!(asked > 300.0, "the probe asked for {asked}, so it proves nothing");
        assert_eq!(got, 109.0);

        // With it: what it asked for, to within the pixel round.
        let (asked, got) = height(true);
        assert!(
            (got - asked).abs() <= 1.0,
            "asked for {asked}, got {got}"
        );
    }
}
