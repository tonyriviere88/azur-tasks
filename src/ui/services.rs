//! The Services tab.

use super::fmt;
use super::list::{ListTab, NO_GENERATION};
use super::table::{Align, Column, Table};
use super::theme::Theme;
use azur_egui_theme::components::{
    menu_divider, ContextMenu, MenuItem, Text, TextColor, TextVariant,
};
use crate::sys::services::{self, Service, ServiceState};
use crate::sys::snapshot::Snapshot;
use egui::Ui;
use std::time::{Duration, Instant};

const C_NAME: u16 = 0;
const C_PID: u16 = 1;
const C_DESC: u16 = 2;
const C_STATE: u16 = 3;
const C_GROUP: u16 = 4;

pub struct ServicesTab {
    /// Keyed by row index: a service is re-enumerated wholesale every couple of
    /// seconds, so position is what a selection can honestly be pinned to here.
    pub list: ListTab<usize>,
    services: Vec<Service>,
    order: Vec<u32>,
    /// Enumerating services walks the registry for every entry, so it is polled
    /// on its own slower cadence rather than with the process sampler.
    last_refresh: Option<Instant>,
    /// The folder-opening target for the row the context menu is on, resolved when
    /// the menu opens: a service's image path is a registry read and a walk down a
    /// command line, neither of which belongs in every frame.
    menu_path: Option<String>,
}

impl ServicesTab {
    pub fn new() -> Self {
        let cols = vec![
            Column::new(C_NAME, "Name", 220.0, Align::Left).min(120.0),
            Column::new(C_PID, "PID", 74.0, Align::Right),
            Column::new(C_DESC, "Description", 380.0, Align::Left),
            Column::new(C_STATE, "Status", 96.0, Align::Left),
            Column::new(C_GROUP, "Group", 160.0, Align::Left),
        ];
        Self {
            list: ListTab::new("services", Table::new(cols, C_NAME, false)),
            services: Vec::new(),
            order: Vec::new(),
            last_refresh: None,
            menu_path: None,
        }
    }

    /// Re-enumerate at most every two seconds.
    fn maybe_refresh(&mut self) {
        let stale = self
            .last_refresh
            .map(|t| t.elapsed() > Duration::from_secs(2))
            .unwrap_or(true);
        if stale {
            self.services = services::enumerate();
            self.last_refresh = Some(Instant::now());
            self.list.mark_dirty();
        }
    }

    fn rebuild(&mut self, filter: &str) {
        self.order.clear();
        let pid = filter.parse::<u32>().ok();
        for (i, s) in self.services.iter().enumerate() {
            if !filter.is_empty() {
                let hit = super::fmt::contains_ci(&s.name, filter)
                    || super::fmt::contains_ci(&s.display_name, filter)
                    || pid == Some(s.pid);
                if !hit {
                    continue;
                }
            }
            self.order.push(i as u32);
        }

        let svc = &self.services;
        let v = &mut self.order;
        match self.list.table.sort_by {
            C_PID => v.sort_unstable_by_key(|&i| {
                // Stopped services report PID 0; sort those last either way.
                let p = svc[i as usize].pid;
                if p == 0 {
                    u32::MAX
                } else {
                    p
                }
            }),
            C_DESC => {
                v.sort_unstable_by_key(|&i| svc[i as usize].display_name.to_ascii_lowercase())
            }
            C_STATE => v.sort_unstable_by_key(|&i| match svc[i as usize].state {
                ServiceState::Running => 0u8,
                ServiceState::Pending => 1,
                ServiceState::Stopped => 2,
            }),
            C_GROUP => v.sort_unstable_by_key(|&i| {
                svc[i as usize]
                    .group
                    .as_ref()
                    .map(|g| g.to_ascii_lowercase())
                    .unwrap_or_default()
            }),
            _ => v.sort_unstable_by_key(|&i| svc[i as usize].name.to_ascii_lowercase()),
        }
        if self.list.table.sort_desc {
            v.reverse();
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        snap: &Snapshot,
        filter: &str,
        action: &mut Option<super::Action>,
    ) {
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
            services,
            order,
            ..
        } = self;
        let out = list.body(ui, t, order.len(), |rp, vis, buf| {
                let s = &services[order[rp.row] as usize];
                for (slot, &cid) in vis.iter().enumerate() {
                    match cid {
                        C_NAME => rp.text(slot, &s.name, t.text.primary, Align::Left),
                        C_PID => {
                            if s.pid == 0 {
                                rp.text(slot, "—", t.text.tertiary, Align::Right);
                            } else {
                                fmt::integer(buf, s.pid as u64);
                                rp.text_font(slot, buf, t.text.secondary, Align::Right, &t.fonts.mono);
                            }
                        }
                        C_DESC => rp.text(slot, &s.display_name, t.text.secondary, Align::Left),
                        C_STATE => {
                            let (fg, bg) = match s.state {
                                ServiceState::Running => (t.status.success, t.status.success.gamma_multiply(0.18)),
                                ServiceState::Stopped => {
                                    (t.text.tertiary, t.text.tertiary.gamma_multiply(0.14))
                                }
                                ServiceState::Pending => (t.status.warning, t.status.warning.gamma_multiply(0.18)),
                            };
                            rp.pill(slot, s.state.label(), fg, bg);
                        }
                        C_GROUP => rp.text(
                            slot,
                            s.group.as_deref().unwrap_or("—"),
                            t.text.tertiary,
                            Align::Left,
                        ),
                        _ => {}
                    }
                }
            });

        self.list.settle(ui, &out, self.order.len(), Some);

        // A running service maps onto a process, so offer the same jump.
        let hosting_pid = out
            .context_row
            .or(self.list.selected_row)
            .and_then(|r| self.service_at(r))
            .map(|s| s.pid)
            .filter(|&p| p != 0);

        // Cleared as well as set: right-clicking below the last row opens the menu
        // with no service under it.
        if out.response.secondary_clicked() {
            self.menu_path = out
                .context_row
                .and_then(|r| self.service_at(r))
                .and_then(|s| services::image_path(&s.name))
                .and_then(|cmd| crate::sys::actions::executable_of(&cmd));
        }
        let menu_path = self.menu_path.clone();

        ContextMenu::new(&out.response).show(&ui.ctx().clone(), |ui| {
            match hosting_pid {
                Some(pid) => {
                    if ui.add(MenuItem::new("Go to process")).clicked() {
                        if let Some(p) = snap.procs.iter().find(|p| p.pid == pid) {
                            *action = Some(super::Action::GoToDetails(p.key()));
                        }
                        ui.close();
                    }
                }
                None => {
                    ui.add_enabled(false, MenuItem::new("Go to process"));
                }
            }
            // Where the service's own image lives -- svchost's folder for the many
            // that are hosted, which is the honest answer: that is the program
            // Windows starts.
            let item = ui.add_enabled(menu_path.is_some(), MenuItem::new("Open folder"));
            if item.clicked() {
                if let Some(p) = &menu_path {
                    *action = Some(super::Action::OpenFileLocation { path: p.clone() });
                }
                ui.close();
            }
            if menu_path.is_none() {
                item.on_hover_text("This service records no image path on disk");
            }
            menu_divider(ui);
            if ui.add(MenuItem::new("Refresh now")).clicked() {
                self.last_refresh = None;
                ui.close();
            }
            menu_divider(ui);
            ui.add(
                Text::new("Starting and stopping services requires elevation")
                    .variant(TextVariant::Caption)
                    .color(TextColor::Tertiary),
            );
        });

    }

    /// The service a visible row stands for.
    fn service_at(&self, row: usize) -> Option<&Service> {
        self.order
            .get(row)
            .and_then(|i| self.services.get(*i as usize))
    }

    /// Drop the selection, for Escape.
    pub fn clear_selection(&mut self) {
        self.list.clear_selection();
    }

    pub fn count(&self) -> (usize, usize) {
        let running = self
            .services
            .iter()
            .filter(|s| s.state == ServiceState::Running)
            .count();
        (self.services.len(), running)
    }
}
