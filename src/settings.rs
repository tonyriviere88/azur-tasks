//! Persisted UI state: window geometry, nav-rail state, per-table columns.
//!
//! Stored as flat `key=value` text in `%APPDATA%\yatm\settings.ini`, parsed by
//! hand. A settings file is a dozen scalars and a few column widths; reaching
//! for a serialization crate would pull in a derive macro and a parser to avoid
//! writing thirty lines.
//!
//! Nothing here is load-bearing. A missing, truncated or hand-mangled file
//! leaves the defaults in place rather than failing the launch, and unknown keys
//! survive a round trip so a newer build's settings are not destroyed by an
//! older one.

use crate::ui::table::Table;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long to wait before writing after something changes. Dragging a window
/// edge changes the geometry on every frame; this collapses that into one write.
const WRITE_DELAY: Duration = Duration::from_secs(2);

pub struct Settings {
    /// `BTreeMap` so the file comes out in a stable order and stays readable
    /// and diffable.
    values: BTreeMap<String, String>,
    path: Option<PathBuf>,
    /// Set when a value changed; cleared by `flush`.
    dirty: bool,
    dirty_since: Option<Instant>,
}

impl Settings {
    pub fn load() -> Self {
        let path = dir().map(|d| d.join("settings.ini"));
        let mut values = BTreeMap::new();
        if let Some(p) = &path {
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if let Some((k, v)) = line.split_once('=') {
                        values.insert(k.trim().to_owned(), v.trim().to_owned());
                    }
                }
            }
        }
        Self {
            values,
            path,
            dirty: false,
            dirty_since: None,
        }
    }

    // --- scalars ----------------------------------------------------------

    pub fn f32(&self, key: &str) -> Option<f32> {
        self.values.get(key)?.parse().ok()
    }

    pub fn bool(&self, key: &str, default: bool) -> bool {
        match self.values.get(key).map(String::as_str) {
            Some("1" | "true") => true,
            Some("0" | "false") => false,
            _ => default,
        }
    }

    pub fn set(&mut self, key: &str, value: impl ToString) {
        let value = value.to_string();
        // Only mark dirty on a real change, or the geometry written every frame
        // would keep the file permanently pending.
        if self.values.get(key) == Some(&value) {
            return;
        }
        self.values.insert(key.to_owned(), value);
        self.dirty = true;
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    pub fn set_bool(&mut self, key: &str, value: bool) {
        self.set(key, if value { 1 } else { 0 });
    }

    // --- window geometry --------------------------------------------------

    /// Saved position and inner size, if a usable one was stored.
    pub fn window_rect(&self) -> Option<[f32; 4]> {
        let r = [
            self.f32("window.x")?,
            self.f32("window.y")?,
            self.f32("window.w")?,
            self.f32("window.h")?,
        ];
        // A window restored off-screen, or at a nonsense size, is worse than
        // one that ignores the saved geometry. Monitors get unplugged.
        let sane = r[2] >= 640.0 && r[3] >= 400.0 && r[0] > -30_000.0 && r[1] > -30_000.0;
        sane.then_some(r)
    }

    // --- tables -----------------------------------------------------------

    /// Apply a saved column layout and sort order.
    ///
    /// Columns are keyed by their stable `id`, not by position, so adding or
    /// reordering columns in a later build leaves a saved file still usable.
    pub fn restore_table(&self, name: &str, table: &mut Table) {
        for c in table.cols.iter_mut() {
            let Some(v) = self.values.get(&format!("{name}.col.{}", c.id)) else {
                continue;
            };
            let mut parts = v.split(',');
            if let Some(vis) = parts.next() {
                c.visible = vis.trim() == "1";
            }
            if let Some(w) = parts.next().and_then(|w| w.trim().parse::<f32>().ok()) {
                // Clamp: a stored width from a different DPI or a corrupted
                // file should not make a column unusable.
                c.width = w.clamp(c.min_width, 1200.0);
            }
        }
        // Never leave every column hidden.
        if !table.cols.iter().any(|c| c.visible) {
            for c in table.cols.iter_mut() {
                c.visible = true;
            }
        }
        if let Some(id) = self.values.get(&format!("{name}.sort")) {
            if let Ok(id) = id.parse::<u16>() {
                if table.cols.iter().any(|c| c.id == id) {
                    table.sort_by = id;
                }
            }
        }
        table.sort_desc = self.bool(&format!("{name}.sort_desc"), table.sort_desc);
    }

    /// Write a table's layout back if it differs from what is stored.
    ///
    /// Called every frame rather than hooked into each place that can change a
    /// column, so dragging a column edge, toggling one from the menu and
    /// clicking a header all persist without any of them knowing this exists.
    pub fn sync_table(&mut self, name: &str, table: &Table) {
        for c in &table.cols {
            let vis = if c.visible { 1 } else { 0 };
            // Width is rounded: sub-pixel drift would rewrite the file forever.
            self.set(
                &format!("{name}.col.{}", c.id),
                format!("{vis},{}", c.width.round()),
            );
        }
        self.set(&format!("{name}.sort"), table.sort_by);
        self.set_bool(&format!("{name}.sort_desc"), table.sort_desc);
    }

    // --- writing ----------------------------------------------------------

    /// Write if something changed and it has been quiet for a moment.
    pub fn maybe_flush(&mut self) {
        if !self.dirty {
            return;
        }
        if self
            .dirty_since
            .is_some_and(|t| t.elapsed() < WRITE_DELAY)
        {
            return;
        }
        self.flush();
    }

    /// Write now, whatever the delay says. Used on shutdown.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.dirty_since = None;

        let Some(path) = self.path.clone() else {
            return;
        };
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let mut text = String::with_capacity(self.values.len() * 24);
        text.push_str("# Yet Another Task Manager -- UI state. Safe to delete.\n");
        for (k, v) in &self.values {
            text.push_str(k);
            text.push('=');
            text.push_str(v);
            text.push('\n');
        }
        // A failed write is not worth telling the user about: the only cost is
        // that the window opens where it did last time.
        let _ = std::fs::write(&path, text);
    }
}

fn dir() -> Option<PathBuf> {
    // APPDATA is the roaming profile. Reading the environment avoids
    // SHGetKnownFolderPath and its COM-allocated string.
    let base = std::env::var_os("APPDATA")?;
    if base.is_empty() {
        return None;
    }
    Some(PathBuf::from(base).join("yatm"))
}
