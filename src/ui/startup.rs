//! The Startup tab.

use super::table::{Align, Column, Table};
use super::theme::Theme;
use crate::sys::startup::{self, StartupEntry};
use egui::{Id, Ui};
use std::time::{Duration, Instant};

const C_NAME: u16 = 0;
const C_STATE: u16 = 1;
const C_SOURCE: u16 = 2;
const C_CMD: u16 = 3;

pub struct StartupTab {
    pub table: Table,
    entries: Vec<StartupEntry>,
    order: Vec<u32>,
    selected: Option<usize>,
    last_refresh: Option<Instant>,
    built_filter: String,
    dirty: bool,
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
        let mut table = Table::new(cols, C_NAME, false);
        table.row_h = 24.0;
        Self {
            table,
            entries: Vec::new(),
            order: Vec::new(),
            selected: None,
            last_refresh: None,
            built_filter: String::new(),
            dirty: true,
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
            self.dirty = true;
        }
    }

    fn rebuild(&mut self, filter: &str) {
        self.order.clear();
        let needle = filter.to_ascii_lowercase();
        for (i, e) in self.entries.iter().enumerate() {
            if !needle.is_empty() {
                let hit = e.name.to_ascii_lowercase().contains(&needle)
                    || e.command.to_ascii_lowercase().contains(&needle);
                if !hit {
                    continue;
                }
            }
            self.order.push(i as u32);
        }

        let es = &self.entries;
        let v = &mut self.order;
        match self.table.sort_by {
            C_STATE => v.sort_unstable_by_key(|&i| !es[i as usize].enabled),
            C_SOURCE => v.sort_unstable_by_key(|&i| es[i as usize].source.label()),
            C_CMD => v.sort_unstable_by_key(|&i| es[i as usize].command.to_ascii_lowercase()),
            _ => v.sort_unstable_by_key(|&i| es[i as usize].name.to_ascii_lowercase()),
        }
        if self.table.sort_desc {
            v.reverse();
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, t: &Theme, filter: &str) {
        self.maybe_refresh();
        if self.dirty || self.built_filter != filter {
            self.rebuild(filter);
            self.built_filter = filter.to_owned();
            self.dirty = false;
        }

        let id = Id::new("startup");
        if self.table.header(ui, t, id) {
            self.dirty = true;
        }

        let order = std::mem::take(&mut self.order);
        // Hoisted so the row callback does not need `self.table`, which
        // `Table::body` holds mutably.
        let vis: Vec<u16> = self
            .table
            .cols
            .iter()
            .filter(|c| c.visible)
            .map(|c| c.id)
            .collect();
        let entries = std::mem::take(&mut self.entries);

        let out = self
            .table
            .body(ui, t, id, order.len(), self.selected, |rp| {
                let e = &entries[order[rp.row] as usize];
                for (slot, &cid) in vis.iter().enumerate() {
                    match cid {
                        C_NAME => rp.text(slot, &e.name, t.text, Align::Left),
                        C_STATE => {
                            let (fg, bg, label) = if e.enabled {
                                (t.ok, t.ok.gamma_multiply(0.18), "Enabled")
                            } else {
                                (
                                    t.text_faint,
                                    t.text_faint.gamma_multiply(0.14),
                                    "Disabled",
                                )
                            };
                            rp.pill(slot, label, fg, bg);
                        }
                        C_SOURCE => rp.text(slot, e.source.label(), t.text_dim, Align::Left),
                        C_CMD => rp.text_font(
                            slot,
                            &e.command,
                            t.text_faint,
                            Align::Left,
                            &t.fonts.mono,
                        ),
                        _ => {}
                    }
                }
            });

        self.entries = entries;

        if let Some(r) = out.clicked_row.or(out.context_row) {
            self.selected = Some(r);
        }

        out.response.context_menu(|ui| {
            if ui.button("Refresh now").clicked() {
                self.last_refresh = None;
                ui.close();
            }
            ui.separator();
            ui.label(
                egui::RichText::new(
                    "Enabling or disabling startup entries is not implemented;\n\
                     this tab is read-only.",
                )
                .small()
                .color(t.text_faint),
            );
        });

        self.order = order;
    }

    pub fn count(&self) -> (usize, usize) {
        let enabled = self.entries.iter().filter(|e| e.enabled).count();
        (self.entries.len(), enabled)
    }
}
