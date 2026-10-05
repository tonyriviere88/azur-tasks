//! Application shell: navigation, toolbar, status bar, action dispatch.

use crate::sys::sampler::{self, Monitor};
use crate::sys::snapshot::Snapshot;
use crate::ui::app_mark::AppMark;
use crate::ui::icons::IconCache;
use crate::ui::theme::{self, Palette, Theme};
use crate::ui::{
    details, inspector, performance, processes, sensors, services, startup, summary, Action,
};

use azur_egui_theme as azur;
use azur::components::{NavigationRail, TitleBar, TitleBarAction, Toaster, ToasterPosition};
use azur::tokens::{row, space};
use egui::{Rect, Ui};
use std::time::Instant;

mod actions;
mod chrome;
mod tabs;

pub use tabs::Tab;
use chrome::{PANEL_PAD_Y, STATUS_H, TOOLBAR_H};
use tabs::persisted_tables;

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

    summary: summary::SummaryTab,
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

/// The key the Sensors tab's arrangement is stored under.
const SENSORS_KEY: &str = "sensors";
/// And the one the Summary tab's chosen graphics adapter is stored under.
const SUMMARY_GPU_KEY: &str = "summary.gpu";
/// The palette, by [`Palette::key`] — the same word the file manager next door writes.
const THEME_KEY: &str = "theme";

/// What the menu under the title bar's mark asked for.
#[derive(Clone, Copy)]
enum AppMenu {
    ResetSize,
    SetTheme(Palette),
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: crate::settings::Settings) -> Self {
        // An unreadable name falls back to the default rather than refusing to open.
        let palette = settings
            .text(THEME_KEY)
            .and_then(Palette::parse)
            .unwrap_or_default();
        let theme = Theme::of(palette);
        theme::install(&cc.egui_ctx, &theme);

        // The sampler pokes egui when a new snapshot lands. This is what makes
        // the app event-driven rather than polled: with no input and no new
        // data, `ui` is never called and the process uses no CPU at all.
        let ctx = cc.egui_ctx.clone();
        let mon = sampler::start_with_notify(1000, move || ctx.request_repaint());

        let mut processes = processes::ProcessesTab::new();
        let mut details = details::DetailsTab::new();
        let mut services = services::ServicesTab::new();
        let mut startup = startup::StartupTab::new();
        for (key, table) in
            persisted_tables(&mut processes, &mut details, &mut services, &mut startup)
        {
            settings.restore_table(key, table);
        }
        let mut sensors = sensors::SensorsTab::new();
        settings.restore_sensors(SENSORS_KEY, &mut sensors.layout);
        let mut summary = summary::SummaryTab::new();
        // Absent means the reader has never chosen, which is not the same as having
        // chosen nothing: the default has to keep up with what is fitted, so it is
        // worked out from the snapshot every frame. See `SummaryTab::gpu`.
        if let Some(name) = settings.text(SUMMARY_GPU_KEY) {
            summary.gpu = name.to_owned();
        }
        // Collapsed by default: the rail's icons are captioned by the tab heading
        // beside them, and the width it gives back goes to the table, which is what
        // a reader came for. Expanding is one click and it is remembered.
        let nav_collapsed = settings.bool("nav.collapsed", true);
        let maximized = settings.bool("window.maximized", false);


        Self {
            mon,
            theme,
            icons: IconCache::new(),
            mark: AppMark::default(),
            // The window opens on the summary: it is the page that says whether
            // anything needs looking at, and every part of it is a way into the tab
            // that would answer the next question.
            tab: Tab::Summary,
            filter: String::new(),
            filter_focused: false,
            elevated: crate::sys::actions::is_elevated(),
            summary,
            processes,
            details,
            perf: performance::PerfTab::new(),
            sensors,
            services,
            startup,
            // Bottom right, where a toast reports on what has just happened without
            // covering what is about to. The top-right corner of this window holds the
            // sample-rate control and the title bar's own buttons, and a toast landing
            // there hides them at exactly the moment a reader may want to undo
            // something; the bottom of the window is the status bar, which is a
            // read-out and can be covered for four seconds.
            toaster: Toaster::new().position(ToasterPosition::BottomEnd),
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

    /// Push anything the UI may have changed into the settings store.
    ///
    /// Done in one place per frame rather than at each site that can change
    /// something: a column edge drag, a header click and a menu toggle all
    /// persist without any of them having to know persistence exists. `set`
    /// ignores writes that do not change the value, so this is free when idle.
    fn sync_settings(&mut self, ctx: &egui::Context) {
        self.settings.set_bool("nav.collapsed", self.nav_collapsed);
        self.settings.set(THEME_KEY, self.theme.palette.key());
        // Destructured rather than reached through `self`, because the loop needs
        // `settings` mutably and the tabs at the same time: naming the fields is what
        // tells the borrow checker they are distinct.
        let Self {
            settings,
            processes,
            details,
            services,
            startup,
            sensors,
            summary,
            ..
        } = self;
        for (key, table) in persisted_tables(processes, details, services, startup) {
            settings.sync_table(key, table);
        }
        settings.sync_sensors(SENSORS_KEY, &sensors.layout);
        // Only once there is a choice to keep. Writing the empty string would freeze
        // "no choice" into the file as if it were one, and the key is read back as a
        // name the machine no longer has.
        if !summary.gpu.is_empty() {
            settings.set(SUMMARY_GPU_KEY, &summary.gpu);
        }

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
        // Ctrl holds the row order on the tabs that have one to hold; mirrored
        // here so the status bar can say so.
        self.order_frozen =
            ctx.input(|i| i.modifiers.ctrl) && self.tab.holds_order_with_ctrl();

        // Global shortcuts.
        let mut reset_size_key = false;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::F5) {
                self.mon.refresh_now();
            }
            // Ctrl+Win+Down, the file manager's key for the same entry in the same
            // menu. The Windows key is not among egui's modifiers, so it is asked of
            // the platform -- and only once Ctrl and the arrow are both there, or
            // every frame of a keyboard scroll would be a syscall for nothing.
            // Consumed as Ctrl+Down, so a list does not also move its cursor.
            if i.modifiers.command
                && i.key_pressed(egui::Key::ArrowDown)
                && crate::sys::util::super_down()
            {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowDown);
                reset_size_key = true;
            }
            // Alt+D toggles the inspector, on the tabs that have one. Consumed
            // whatever the tab, so it never reaches a text field as a character on
            // the tabs where it would do nothing.
            if i.consume_key(egui::Modifiers::ALT, egui::Key::D)
                && self.tab.is_process_list()
            {
                self.inspector_open = !self.inspector_open;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) {
                let key = match self.tab {
                    Tab::Processes => self.processes.list.selected,
                    Tab::Details => self.details.list.selected,
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

        let theme = Theme::of(self.theme.palette);

        // --- window chrome -------------------------------------------------
        //
        // The platform's caption is off (see `main`), so the title bar is Azur's:
        // 32px of `background-layer` with the window buttons at the end.
        //
        // The mark is resolved before the panel, not inside it: uploading a texture
        // needs the context, and the closure below already holds the borrow of `self`
        // the window buttons write through.
        let mark = self.mark.icon(&ctx);
        let mut menu: Option<AppMenu> = None;
        egui::Panel::top("title")
            .exact_size(row::TITLE_BAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                match TitleBar::new(crate::PRODUCT_NAME)
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
                menu = app_menu(ui, &mark, &theme);
            });
        if reset_size_key {
            menu = Some(AppMenu::ResetSize);
        }
        match menu {
            Some(AppMenu::ResetSize) => {
                // Un-maximised first, and said out loud rather than relied on:
                // `SetWindowPos` on a maximised window happens to restore it on
                // the way, but that is winit's platform behaviour, not a promise.
                if self.maximized {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                    self.maximized = false;
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                    crate::WINDOW_SIZE.into(),
                ));
            }
            Some(AppMenu::SetTheme(palette)) if palette != self.theme.palette => {
                self.theme = Theme::of(palette);
                theme::restyle(&ctx, &self.theme);
                // This frame has painted its title bar in the old palette already;
                // the next one is all in the new.
                ctx.request_repaint();
            }
            _ => {}
        }

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
                if self.inspector_open && self.tab.is_process_list() {
                    let proc = self.inspected(snap);
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
                            self.inspector.ui(ui, &theme, proc, snap.generation, &mut action)
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
                Tab::Summary => self.summary.ui(
                    ui,
                    &theme,
                    snap,
                    &self.mon.meta,
                    &mut self.icons,
                    &mut action,
                ),
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
pub fn screen_anchor_below(ctx: &egui::Context, widget: Rect) -> Option<(i32, i32)> {
    let content = ctx.input(|i| i.viewport().inner_rect)?;
    let ppp = ctx.pixels_per_point();
    // `space-2` of air under the button, which is the gap a menu opens with: the
    // dialog reads as hanging off the control rather than covering its edge.
    let x = (content.min.x + widget.center().x) * ppp;
    let y = (content.min.y + widget.bottom() + space::S2) * ppp;
    Some((x.round() as i32, y.round() as i32))
}

/// The title bar's mark as a button, and the menu it opens: the settings that belong
/// to the window rather than to a tab. The same entries, in the same order and
/// words, as the file manager next door has under its own mark.
///
/// Called right after `TitleBar::show`, in the same `ui`, so that it is registered
/// *after* the bar's caption and wins the pointer over it — within a layer egui gives
/// a click to the last widget that asked for it.
fn app_menu(
    ui: &mut Ui,
    mark: azur::icons::Icon<'_>,
    t: &Theme,
) -> Option<AppMenu> {
    use azur::components::{menu_divider, Menu, MenuItem};
    use azur::icons::window_restore;
    use azur::tokens::radius;

    // Where `TitleBar` put its 16-point icon: `space-3` in, centred in the bar.
    let bar = ui.min_rect();
    let glyph = Rect::from_min_size(
        egui::pos2(bar.left() + space::S3, bar.center().y - 8.0),
        egui::Vec2::splat(16.0),
    );
    // The size of a tool button, which is what the file manager's mark is.
    let button = Rect::from_center_size(glyph.center(), egui::Vec2::splat(row::DENSE));
    let response = ui.interact(button, ui.id().with("app-menu"), egui::Sense::click());
    if response.hovered() {
        // The bar has already painted the mark, so the hover goes over it and the
        // mark is painted again on top: the one way to get a fill *under* it
        // without the bar knowing its icon is a button.
        ui.painter()
            .rect_filled(button, egui::CornerRadius::same(radius::SMALL), t.bg.control_hover);
        mark(ui.painter(), glyph, t.text.secondary);
    }

    let mut chosen = None;
    Menu::new(&response).min_width(220.0).show(ui.ctx(), |ui| {
        // The key is read with the other global shortcuts in `App::ui`.
        if ui
            .add(
                MenuItem::new("Reset window size")
                    .shortcut("Ctrl+Win+↓")
                    .icon(&window_restore),
            )
            .clicked()
        {
            chosen = Some(AppMenu::ResetSize);
        }
        menu_divider(ui);
        // The design system's own entry, so every Azur application offers the
        // palettes the same way: grouped under `Dark themes` and `Light themes`,
        // the current one ticked. See `azur::palettes::submenu`.
        if let Some(palette) = azur::palettes::submenu(ui, t.palette) {
            chosen = Some(AppMenu::SetTheme(palette));
        }
    });
    chosen
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
            crate::ui::theme::install(&ctx, &crate::ui::theme::Theme::dark());
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
