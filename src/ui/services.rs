//! The Services tab.

use super::fmt;
use super::table::{Align, Column, Table};
use super::theme::Theme;
use crate::sys::services::{self, Service, ServiceState};
use crate::sys::snapshot::Snapshot;
use egui::{Id, Ui};
use std::time::{Duration, Instant};

const C_NAME: u16 = 0;
const C_PID: u16 = 1;
const C_DESC: u16 = 2;
const C_STATE: u16 = 3;
const C_GROUP: u16 = 4;

pub struct ServicesTab {
    pub table: Table,
    services: Vec<Service>,
    order: Vec<u32>,
    selected: Option<usize>,
    /// Enumerating services walks the registry for every entry, so it is polled
    /// on its own slower cadence rather than with the process sampler.
    last_refresh: Option<Instant>,
    built_filter: String,
    dirty: bool,
    scratch: String,
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
        let mut table = Table::new(cols, C_NAME, false);
        table.row_h = 24.0;
        Self {
            table,
            services: Vec::new(),
            order: Vec::new(),
            selected: None,
            last_refresh: None,
            built_filter: String::new(),
            dirty: true,
            scratch: String::new(),
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
            self.dirty = true;
        }
    }

    fn rebuild(&mut self, filter: &str) {
        self.order.clear();
        let needle = filter.to_ascii_lowercase();
        for (i, s) in self.services.iter().enumerate() {
            if !needle.is_empty() {
                let hit = s.name.to_ascii_lowercase().contains(&needle)
                    || s.display_name.to_ascii_lowercase().contains(&needle)
                    || s.pid.to_string() == needle;
                if !hit {
                    continue;
                }
            }
            self.order.push(i as u32);
        }

        let svc = &self.services;
        let v = &mut self.order;
        match self.table.sort_by {
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
        if self.table.sort_desc {
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
        if self.dirty || self.built_filter != filter {
            self.rebuild(filter);
            self.built_filter = filter.to_owned();
            self.dirty = false;
        }

        let id = Id::new("services");
        if self.table.header(ui, t, id) {
            self.dirty = true;
        }

        let order = std::mem::take(&mut self.order);
        let mut buf = std::mem::take(&mut self.scratch);
        // Hoisted so the row callback does not need `self.table`, which
        // `Table::body` holds mutably.
        let vis: Vec<u16> = self
            .table
            .cols
            .iter()
            .filter(|c| c.visible)
            .map(|c| c.id)
            .collect();
        let services = std::mem::take(&mut self.services);

        let out = self
            .table
            .body(ui, t, id, order.len(), self.selected, |rp| {
                let s = &services[order[rp.row] as usize];
                for (slot, &cid) in vis.iter().enumerate() {
                    match cid {
                        C_NAME => rp.text(slot, &s.name, t.text, Align::Left),
                        C_PID => {
                            if s.pid == 0 {
                                rp.text(slot, "—", t.text_faint, Align::Right);
                            } else {
                                fmt::integer(&mut buf, s.pid as u64);
                                rp.text_font(slot, &buf, t.text_dim, Align::Right, &t.fonts.mono);
                            }
                        }
                        C_DESC => rp.text(slot, &s.display_name, t.text_dim, Align::Left),
                        C_STATE => {
                            let (fg, bg) = match s.state {
                                ServiceState::Running => (t.ok, t.ok.gamma_multiply(0.18)),
                                ServiceState::Stopped => {
                                    (t.text_faint, t.text_faint.gamma_multiply(0.14))
                                }
                                ServiceState::Pending => (t.warn, t.warn.gamma_multiply(0.18)),
                            };
                            rp.pill(slot, s.state.label(), fg, bg);
                        }
                        C_GROUP => rp.text(
                            slot,
                            s.group.as_deref().unwrap_or("—"),
                            t.text_faint,
                            Align::Left,
                        ),
                        _ => {}
                    }
                }
            });

        self.scratch = buf;
        self.services = services;

        if let Some(r) = out.clicked_row.or(out.context_row) {
            self.selected = Some(r);
        }

        // A running service maps onto a process, so offer the same jump.
        let hosting_pid = out
            .context_row
            .or(self.selected)
            .and_then(|r| order.get(r))
            .map(|&i| self.services[i as usize].pid)
            .filter(|&p| p != 0);

        out.response.context_menu(|ui| {
            match hosting_pid {
                Some(pid) => {
                    if ui.button("Go to process").clicked() {
                        if let Some(p) = snap.procs.iter().find(|p| p.pid == pid) {
                            *action = Some(super::Action::GoToDetails(p.key()));
                        }
                        ui.close();
                    }
                }
                None => {
                    ui.add_enabled(false, egui::Button::new("Go to process"));
                }
            }
            ui.separator();
            if ui.button("Refresh now").clicked() {
                self.last_refresh = None;
                ui.close();
            }
            ui.separator();
            ui.label(
                egui::RichText::new("Starting and stopping services requires elevation")
                    .small()
                    .color(t.text_faint),
            );
        });

        self.order = order;
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
