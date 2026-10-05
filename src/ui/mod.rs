//! Presentation layer.

pub mod azur_theme;
pub mod details;
pub mod fmt;
pub mod graph;
pub mod icons;
pub mod performance;
pub mod processes;
pub mod services;
pub mod startup;
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
    CopyRow(String),
    Elevate,
}

/// Shared right-click menu for a process row.
pub fn process_context_menu(
    ui: &mut egui::Ui,
    t: &Theme,
    pid: u32,
    name: &str,
    key: ProcKey,
    path: Option<&str>,
    action: &mut Option<Action>,
) {
    ui.label(
        egui::RichText::new(fmt::elide(name, 40).as_ref())
            .small()
            .color(t.text_dim),
    );
    ui.separator();

    if ui.button("End task").clicked() {
        *action = Some(Action::EndTask {
            pid,
            name: name.to_owned(),
        });
        ui.close();
    }
    if ui
        .button("End process tree")
        .on_hover_text("Ends this process and every process it started")
        .clicked()
    {
        *action = Some(Action::EndTree { key });
        ui.close();
    }

    ui.separator();

    ui.menu_button("Set priority", |ui| {
        for p in Priority::ALL {
            if ui.button(p.label()).clicked() {
                *action = Some(Action::SetPriority {
                    pid,
                    name: name.to_owned(),
                    prio: p,
                });
                ui.close();
            }
        }
    });

    if ui.button("Go to details").clicked() {
        *action = Some(Action::GoToDetails(key));
        ui.close();
    }

    ui.separator();

    let has_path = path.is_some_and(|p| !p.is_empty());
    if ui
        .add_enabled(has_path, egui::Button::new("Open file location"))
        .clicked()
    {
        if let Some(p) = path {
            *action = Some(Action::OpenFileLocation { path: p.to_owned() });
        }
        ui.close();
    }
    if ui
        .add_enabled(has_path, egui::Button::new("Properties"))
        .clicked()
    {
        if let Some(p) = path {
            *action = Some(Action::OpenProperties { path: p.to_owned() });
        }
        ui.close();
    }
    if ui
        .add_enabled(has_path, egui::Button::new("Copy image path"))
        .clicked()
    {
        if let Some(p) = path {
            *action = Some(Action::CopyRow(p.to_owned()));
        }
        ui.close();
    }
    if ui.button("Copy PID").clicked() {
        *action = Some(Action::CopyRow(pid.to_string()));
        ui.close();
    }
}

