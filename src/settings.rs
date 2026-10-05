//! Persisted UI state: window geometry, nav-rail state, per-table columns.
//!
//! Stored as flat `key=value` text in `%APPDATA%\Azur Tasks\settings.ini`,
//! parsed by hand. A settings file is a dozen scalars and a few column widths;
//! reaching for a serialization crate would pull in a derive macro and a parser
//! to avoid writing thirty lines.
//!
//! Nothing here is load-bearing. A missing, truncated or hand-mangled file
//! leaves the defaults in place rather than failing the launch, and unknown keys
//! survive a round trip so a newer build's settings are not destroyed by an
//! older one.

use crate::ui::sensors::Layout;
use crate::ui::table::Table;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
        let path = dir().map(|d| {
            migrate_legacy_dir(&d);
            d.join("settings.ini")
        });
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

    /// A stored string, as it was written.
    ///
    /// `None` when the key was never written, which is not the same as an empty
    /// value: a caller that stores a *choice* needs to tell "no choice yet" -- where
    /// the default still has to keep up with the machine -- from a choice the reader
    /// actually made.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
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

    // --- the Sensors page's arrangement -----------------------------------

    /// Restore which sensor sections and cards are shown, and in what order.
    ///
    /// Keys rather than indices, and orders that are allowed to be incomplete: a
    /// file written before a second graphics card was fitted must not hide it, and
    /// a firmware that renumbers its thermal zones must not shuffle the reader's
    /// arrangement onto the wrong ones.
    pub fn restore_sensors(&self, name: &str, layout: &mut Layout) {
        layout.sections = self.list(&format!("{name}.order"));
        layout.hidden = self.list(&format!("{name}.hidden")).into_iter().collect();
        layout.cards.clear();
        let prefix = format!("{name}.cards.");
        for (k, v) in &self.values {
            if let Some(section) = k.strip_prefix(&prefix) {
                layout.cards.insert(section.to_owned(), split(v));
            }
        }
    }

    pub fn sync_sensors(&mut self, name: &str, layout: &Layout) {
        self.set(&format!("{name}.order"), layout.sections.join(","));
        self.set(
            &format!("{name}.hidden"),
            layout.hidden.iter().cloned().collect::<Vec<_>>().join(","),
        );
        for (section, order) in &layout.cards {
            self.set(&format!("{name}.cards.{section}"), order.join(","));
        }
        // A section that no longer has an order of its own has to lose its line
        // rather than keep the last one: these keys come and go, unlike everything
        // else in this file, and "Reset layout" is exactly the case that drops one.
        // Writing only what the layout holds would leave the old line behind for the
        // next launch to read back, and the reset would undo itself overnight.
        let prefix = format!("{name}.cards.");
        let stale: Vec<String> = self
            .values
            .keys()
            .filter(|k| {
                k.strip_prefix(&prefix)
                    .is_some_and(|s| !layout.cards.contains_key(s))
            })
            .cloned()
            .collect();
        for k in stale {
            self.remove(&k);
        }
    }

    /// Drop a key, if it is there.
    fn remove(&mut self, key: &str) {
        if self.values.remove(key).is_some() {
            self.dirty = true;
            self.dirty_since.get_or_insert_with(Instant::now);
        }
    }

    /// A comma-separated value, as a list. Empty when the key is absent, which is
    /// the same as "no opinion" everywhere this is used.
    fn list(&self, key: &str) -> Vec<String> {
        self.values.get(key).map(|v| split(v)).unwrap_or_default()
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
        text.push_str("# ");
        text.push_str(crate::PRODUCT_NAME);
        text.push_str(" -- UI state. Safe to delete.\n");
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

/// Split a comma-separated value, dropping the empty parts an empty string or a
/// trailing comma would otherwise produce.
fn split(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

fn dir() -> Option<PathBuf> {
    // APPDATA is the roaming profile. Reading the environment avoids
    // SHGetKnownFolderPath and its COM-allocated string.
    let base = PathBuf::from(std::env::var_os("APPDATA").filter(|b| !b.is_empty())?);
    // The product name, spaces and all: this is a folder a person goes looking
    // for in Explorer, and it sits among others named the same way.
    Some(base.join(crate::PRODUCT_NAME))
}

/// The application was called "Azur Task Manager" until it was renamed, and that
/// folder holds someone's layout. Move it rather than start them over; if the move
/// fails they get the defaults, which is all a fresh folder costs.
fn migrate_legacy_dir(dir: &Path) {
    if let Some(base) = dir.parent().filter(|_| !dir.exists()) {
        let _ = std::fs::rename(base.join("Azur Task Manager"), dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Never touches the disk: `path` is `None`, so `flush` is a no-op.
    fn scratch() -> Settings {
        Settings {
            values: BTreeMap::new(),
            path: None,
            dirty: false,
            dirty_since: None,
        }
    }

    /// "Reset layout" has to *remove* a section's card order, not merely stop
    /// writing it.
    ///
    /// This file is otherwise write-only — every key is rewritten every frame — and
    /// the sensor layout is the one part whose set of keys shrinks. Left in, the old
    /// line is read back at the next launch and the reset undoes itself, which looks
    /// exactly like the reset having worked until the app is restarted.
    #[test]
    fn forgets_a_card_order_that_was_reset() {
        let mut s = scratch();
        let mut layout = Layout::default();
        layout.cards.insert(
            "cpu".to_owned(),
            vec!["cpu.temp".to_owned(), "cpu.usage".to_owned()],
        );
        layout.hidden.insert("gpu.0".to_owned());
        layout.sections = vec!["memory".to_owned(), "cpu".to_owned()];
        s.sync_sensors("sensors", &layout);
        assert_eq!(
            s.values.get("sensors.cards.cpu").map(String::as_str),
            Some("cpu.temp,cpu.usage")
        );

        // What the menu entry does.
        s.sync_sensors("sensors", &Layout::default());
        assert!(
            !s.values.contains_key("sensors.cards.cpu"),
            "the line survived a reset: {:?}",
            s.values
        );

        let mut back = Layout::default();
        s.restore_sensors("sensors", &mut back);
        assert!(
            back.cards.is_empty(),
            "and so the next launch reads it back"
        );
        assert!(back.sections.is_empty());
        assert!(back.hidden.is_empty());
    }

    /// A round trip, including the keys that contain a dot of their own.
    #[test]
    fn carries_a_layout_through_the_file_intact() {
        let mut s = scratch();
        let mut layout = Layout {
            sections: vec!["gpu.1".to_owned(), "cpu".to_owned(), "zones".to_owned()],
            ..Default::default()
        };
        layout.hidden.insert("cpu.clock".to_owned());
        layout.hidden.insert("gpu.0".to_owned());
        layout.cards.insert(
            "gpu.1".to_owned(),
            vec!["gpu.1.temp".to_owned(), "gpu.1.hotspot".to_owned()],
        );
        s.sync_sensors("sensors", &layout);

        let mut back = Layout::default();
        s.restore_sensors("sensors", &mut back);
        assert_eq!(back.sections, layout.sections);
        assert_eq!(back.hidden, layout.hidden);
        assert_eq!(back.cards, layout.cards);
    }

    /// A hand-mangled or half-written value must not produce empty keys, which
    /// would match nothing and quietly sort every real key after them.
    #[test]
    fn ignores_the_gaps_in_a_mangled_list() {
        assert_eq!(split(""), Vec::<String>::new());
        assert_eq!(split(",,"), Vec::<String>::new());
        assert_eq!(split("cpu, ,memory,"), vec!["cpu", "memory"]);
    }
}
