//! The Startup tab.

use super::list::{ListTab, NO_GENERATION};
use super::table::{Align, Column, Table};
use super::theme::Theme;
use super::Action;
use azur_egui_theme::components::{menu_divider, ContextMenu, MenuItem, Text, TextColor, TextVariant};
use crate::sys::startup::{self, StartupEntry};
use egui::Ui;
use std::time::{Duration, Instant};

const C_NAME: u16 = 0;
const C_STATE: u16 = 1;
const C_SOURCE: u16 = 2;
const C_CMD: u16 = 3;

pub struct StartupTab {
    /// Keyed by row index: a startup entry has no identity that survives being
    /// re-enumerated, so position is the honest answer here. `ListTab` drops the
    /// selection when the list comes back shorter, which is what makes the toolbar's
    /// Enable/Disable button grey out rather than act on nothing.
    pub list: ListTab<usize>,
    entries: Vec<StartupEntry>,
    order: Vec<u32>,
    last_refresh: Option<Instant>,
    /// The folder-opening target for the row the context menu is on, resolved when
    /// the menu opens rather than on every frame it stays open: working out which
    /// part of a command line is the program means looking on disk.
    menu_path: Option<String>,
}

impl StartupTab {
    pub fn new() -> Self {
        let cols = vec![
            Column::new(C_NAME, "Name", 240.0, Align::Left).min(120.0),
            Column::new(C_STATE, "Status", 100.0, Align::Left),
            Column::new(C_SOURCE, "Source", 220.0, Align::Left)
                .help("Where Windows reads this entry from"),
            Column::new(C_CMD, "Command", 520.0, Align::Left),
        ];
        Self {
            list: ListTab::new("startup", Table::new(cols, C_NAME, false)),
            entries: Vec::new(),
            order: Vec::new(),
            last_refresh: None,
            menu_path: None,
        }
    }

    fn maybe_refresh(&mut self) {
        // Startup entries change only when the user installs something.
        let stale = self
            .last_refresh
            .map(|t| t.elapsed() > Duration::from_secs(10))
            .unwrap_or(true);
        if stale {
            self.entries = startup::enumerate();
            self.last_refresh = Some(Instant::now());
            self.list.mark_dirty();
        }
    }

    fn rebuild(&mut self, filter: &str) {
        self.order.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if !filter.is_empty() {
                let hit = super::fmt::contains_ci(&e.name, filter)
                    || super::fmt::contains_ci(&e.command, filter);
                if !hit {
                    continue;
                }
            }
            self.order.push(i as u32);
        }

        let es = &self.entries;
        let v = &mut self.order;
        match self.list.table.sort_by {
            C_STATE => v.sort_unstable_by_key(|&i| !es[i as usize].enabled),
            C_SOURCE => v.sort_unstable_by_key(|&i| es[i as usize].source.label()),
            C_CMD => v.sort_unstable_by_key(|&i| es[i as usize].command.to_ascii_lowercase()),
            _ => v.sort_unstable_by_key(|&i| es[i as usize].name.to_ascii_lowercase()),
        }
        if self.list.table.sort_desc {
            v.reverse();
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, t: &Theme, filter: &str, action: &mut Option<Action>) {
        self.maybe_refresh();
        if self.list.stale(NO_GENERATION, filter) {
            self.rebuild(filter);
            self.list
                .built(NO_GENERATION, filter, self.order.len(), Some);
        }
        self.list.header(ui, t);

        // Destructured rather than moved out of `self`: the row closure is handed the
        // column ids and the buffer, so the only thing left for it to borrow is this
        // tab's own data. See `ui::list`.
        let Self {
            list,
            entries,
            order,
            ..
        } = self;
        let out = list.body(ui, t, order.len(), |rp, vis, _buf| {
                let e = &entries[order[rp.row] as usize];
                for (slot, &cid) in vis.iter().enumerate() {
                    match cid {
                        C_NAME => rp.text(slot, &e.name, t.text.primary, Align::Left),
                        C_STATE => {
                            let (fg, bg, label) = if e.enabled {
                                (t.status.success, t.status.success.gamma_multiply(0.18), "Enabled")
                            } else {
                                (
                                    t.text.tertiary,
                                    t.text.tertiary.gamma_multiply(0.14),
                                    "Disabled",
                                )
                            };
                            rp.pill(slot, label, fg, bg);
                        }
                        C_SOURCE => rp.text(slot, e.source.label(), t.text.secondary, Align::Left),
                        C_CMD => rp.text_font(
                            slot,
                            &e.command,
                            t.text.tertiary,
                            Align::Left,
                            &t.fonts.mono,
                        ),
                        _ => {}
                    }
                }
            });

        self.list.settle(ui, &out, self.order.len(), Some);

        // Double-click toggles, like flipping a switch in a list of switches.
        if let Some(r) = out.double_clicked_row {
            if let Some(e) = self.entry_at(r) {
                *action = Some(Action::SetStartupEnabled {
                    entry: Box::new(e.clone()),
                    enable: !e.enabled,
                });
            }
        }

        let target = self
            .list
            .selected_row
            .and_then(|r| self.entry_at(r))
            .cloned();

        // Cleared as well as set: a right-click on the empty space below the rows
        // opens the menu with no row under it, and last time's path must not
        // outlive the row it came from.
        if out.response.secondary_clicked() {
            self.menu_path = out
                .context_row
                .and_then(|r| self.entry_at(r))
                .and_then(|e| crate::sys::actions::executable_of(&e.command));
        }
        let menu_path = self.menu_path.clone();

        ContextMenu::new(&out.response).show(&ui.ctx().clone(), |ui| {
            if let Some(e) = &target {
                ui.add(
                    Text::new(super::fmt::elide(&e.name, 40).into_owned())
                        .variant(TextVariant::Caption)
                        .color(TextColor::Secondary),
                );
                menu_divider(ui);
                let label = if e.enabled { "Disable" } else { "Enable" };
                let item = ui.add(MenuItem::new(label));
                let item = if e.source.needs_admin() {
                    item.on_hover_text("Needs administrator rights: this entry applies to all users")
                } else {
                    item
                };
                if item.clicked() {
                    *action = Some(Action::SetStartupEnabled {
                        entry: Box::new(e.clone()),
                        enable: !e.enabled,
                    });
                    ui.close();
                }
                // A shortcut in the Startup folder *is* the item, so this lands on
                // the Startup folder with it selected; a `Run` value points at a
                // program, so it lands where the program lives.
                let item = ui.add_enabled(menu_path.is_some(), MenuItem::new("Open folder"));
                if item.clicked() {
                    if let Some(p) = &menu_path {
                        *action = Some(Action::OpenFileLocation { path: p.clone() });
                    }
                    ui.close();
                }
                if menu_path.is_none() {
                    item.on_hover_text(
                        "Nothing on disk matches this command, so there is no folder to open",
                    );
                }
                menu_divider(ui);
            }
            if ui.add(MenuItem::new("Refresh now")).clicked() {
                self.last_refresh = None;
                ui.close();
            }
        });
    }

    /// The entry a visible row stands for.
    fn entry_at(&self, row: usize) -> Option<&StartupEntry> {
        self.order
            .get(row)
            .and_then(|i| self.entries.get(*i as usize))
    }

    /// The selected entry, for the toolbar's Enable/Disable button.
    pub fn selected_entry(&self) -> Option<&StartupEntry> {
        self.list.selected_row.and_then(|r| self.entry_at(r))
    }

    /// Drop the selection, for Escape. This is also what disables the toolbar's
    /// Enable/Disable button, since it reads [`Self::selected_entry`].
    pub fn clear_selection(&mut self) {
        self.list.clear_selection();
    }

    /// Forget the cached enumeration, so the next frame reads the registry again.
    pub fn invalidate(&mut self) {
        self.last_refresh = None;
    }

    pub fn count(&self) -> (usize, usize) {
        let enabled = self.entries.iter().filter(|e| e.enabled).count();
        (self.entries.len(), enabled)
    }
}
