//! Presentation layer.

pub mod app_mark;
pub mod details;
pub mod fmt;
pub mod graph;
pub mod icons;
pub mod inspector;

pub mod performance;
pub mod processes;
pub mod sensors;
pub mod services;
pub mod startup;
pub mod summary;
pub mod table;
pub mod theme;

use crate::sys::actions::Priority;
use crate::sys::snapshot::ProcKey;
use theme::Theme;

/// A request raised by the UI and executed by the app shell.
///
/// Menus and buttons never touch the system directly: they emit an `Action`,
/// which `App` performs once per frame. That keeps the borrow of the snapshot
/// (held for painting) separate from mutation, and gives every action a single
/// place to report success or failure.
#[derive(Clone)]
pub enum Action {
    EndTask { pid: u32, name: String },
    EndTree { key: ProcKey },
    SetPriority { pid: u32, name: String, prio: Priority },
    OpenFileLocation { path: String },
    OpenProperties { path: String },
    GoToDetails(ProcKey),
    /// Show a process on the Processes tab, folds opened and scrolled to.
    ///
    /// The Summary tab's counterpart to `GoToDetails`: its rankings are the
    /// Processes tab's own rows, aggregated the same way, so that is the list a
    /// reader is asking to be taken to -- not the flat grid of 33 columns.
    GoToProcess(ProcKey),
    /// Show one resource on the Performance tab.
    ///
    /// What makes the Summary tab's cards a way *into* the page they summarise
    /// rather than a copy of it.
    ShowResource(performance::Resource),
    CopyRow(String),
    Elevate,
    /// Open Windows' Run dialog. The counterpart to `EndTask`, and the one action
    /// here that starts a process rather than acting on one already listed.
    ///
    /// `at` is the point to centre it under, in *screen pixels*: a native dialog is
    /// placed in the desktop's coordinates, not this window's, and it is a centre
    /// because only the code holding the dialog knows how wide it is. `None` leaves
    /// the placement to Windows. See
    /// [`screen_anchor_below`](crate::app::screen_anchor_below).
    NewTask { at: Option<(i32, i32)> },
    /// Switch a startup entry on or off. Carries the entry rather than an index:
    /// actions run after the frame that raised them, by which time the list may
    /// have been re-enumerated under a different order.
    SetStartupEnabled {
        entry: Box<crate::sys::startup::StartupEntry>,
        enable: bool,
    },
}

/// Shared right-click menu for a process row, in Azur's menu items.
///
/// The priority choices are laid out in this menu rather than in a submenu: six
/// entries behind a chevron in a menu that already has eight is a worse trade
/// than a caption and a divider, and a submenu is one more thing to keep open.
pub fn process_context_menu(
    ui: &mut egui::Ui,
    _t: &Theme,
    pid: u32,
    name: &str,
    key: ProcKey,
    path: Option<&str>,
    action: &mut Option<Action>,
) {
    use azur_egui_theme::components::{
        menu_divider, submenu, MenuItem, Text, TextColor, TextVariant,
    };

    ui.add(
        Text::new(fmt::elide(name, 40).into_owned())
            .variant(TextVariant::Caption)
            .color(TextColor::Secondary),
    );
    menu_divider(ui);

    // Destructive first, and marked as such: `danger` paints the label and its
    // icon in `status-danger`, and tints the row on hover.
    if ui.add(MenuItem::new("End task").danger(true)).clicked() {
        *action = Some(Action::EndTask {
            pid,
            name: name.to_owned(),
        });
        ui.close();
    }
    if ui
        .add(MenuItem::new("End process tree").danger(true))
        .on_hover_text("Ends this process and every process it started")
        .clicked()
    {
        *action = Some(Action::EndTree { key });
        ui.close();
    }

    menu_divider(ui);

    if ui.add(MenuItem::new("Go to details")).clicked() {
        *action = Some(Action::GoToDetails(key));
        ui.close();
    }

    let has_path = path.is_some_and(|p| !p.is_empty());
    // "Open folder" rather than Task Manager's "Open file location": every table
    // that has a path for a row offers this, and one verb across all of them is
    // worth more than one table's familiar wording. It does the same thing either
    // way -- opens the containing folder with the file selected.
    if ui
        .add_enabled(has_path, MenuItem::new("Open folder"))
        .clicked()
    {
        if let Some(p) = path {
            *action = Some(Action::OpenFileLocation { path: p.to_owned() });
        }
        ui.close();
    }
    if ui
        .add_enabled(has_path, MenuItem::new("Properties"))
        .clicked()
    {
        if let Some(p) = path {
            *action = Some(Action::OpenProperties { path: p.to_owned() });
        }
        ui.close();
    }
    if ui
        .add_enabled(has_path, MenuItem::new("Copy image path"))
        .clicked()
    {
        if let Some(p) = path {
            *action = Some(Action::CopyRow(p.to_owned()));
        }
        ui.close();
    }
    if ui.add(MenuItem::new("Copy PID")).clicked() {
        *action = Some(Action::CopyRow(pid.to_string()));
        ui.close();
    }

    menu_divider(ui);
    submenu(ui, MenuItem::new("Set priority"), |ui| {
        for p in Priority::ALL {
            if ui.add(MenuItem::new(p.label())).clicked() {
                *action = Some(Action::SetPriority {
                    pid,
                    name: name.to_owned(),
                    prio: p,
                });
                // Closes the whole stack, parent included: the action is done, and
                // leaving the menu that raised it standing would be a loose end.
                ui.close();
            }
        }
    });
}

