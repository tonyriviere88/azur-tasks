//! Carrying out what the UI asked for.
//!
//! Every control in this window raises an [`Action`] instead of touching the system,
//! and this is where they are performed — once per frame, after painting, so an
//! action never mutates state the frame is still reading. Destructive ones are held
//! back behind a confirmation first.

use super::{App, Confirm, Tab};
use crate::sys::actions;
use crate::sys::snapshot::Snapshot;
use crate::ui::Action;
use azur_egui_theme as azur;
use azur::components::{
    Button, Dialog, DialogSize, Intent, MessageBar, Text, TextColor, Toast, Variant,
};
use azur::tokens::space;

impl App {
    /// Route an action: destructive ones go through a confirmation first.
    pub(super) fn dispatch(&mut self, action: Action, snap: &Snapshot) {
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
    pub(super) fn confirm_dialog(&mut self, ctx: &egui::Context, snap: &Snapshot) {
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

    pub(super) fn do_action(&mut self, action: Action, snap: &Snapshot) {
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
            Action::GoToProcess(key) => {
                self.tab = Tab::Processes;
                self.processes.reveal(snap, key);
                self.filter.clear();
                Ok(String::new())
            }
            Action::ShowResource(res) => {
                self.tab = Tab::Performance;
                self.perf.selected = res;
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
