//! Process icons: an async, deduplicated path -> GPU texture cache.
//!
//! Also holds the hand-drawn nav-rail glyphs. Those are painted as vectors
//! rather than taken from an icon font: it removes a runtime dependency on a
//! specific Segoe glyph set (where a wrong codepoint renders as a tofu box) and
//! keeps them crisp at any DPI.

use super::theme::Theme;
use crate::sys::icon::{IconLoader, ICON_SIZE};
use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};
use rustc_hash::FxHashMap;
use std::sync::Arc;

enum Slot {
    Pending,
    Ready(egui::TextureHandle),
    /// The shell had no icon for this path; do not ask again.
    Missing,
}

pub struct IconCache {
    map: FxHashMap<Arc<str>, Slot>,
    loader: IconLoader,
    /// Generic fallback, drawn once into a texture.
    fallback: Option<egui::TextureHandle>,
    /// Requests issued this frame, to bound the queue on a cold start.
    budget: u32,
}

impl IconCache {
    pub fn new() -> Self {
        Self {
            map: FxHashMap::default(),
            loader: IconLoader::new(),
            fallback: None,
            budget: 0,
        }
    }

    /// Take delivered icons and upload them. Call once per frame.
    pub fn pump(&mut self, ctx: &egui::Context) {
        self.budget = 24;
        for px in self.loader.drain() {
            if px.rgba.len() != ICON_SIZE * ICON_SIZE * 4 {
                self.map.insert(px.key, Slot::Missing);
                continue;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([ICON_SIZE, ICON_SIZE], &px.rgba);
            let tex = ctx.load_texture(
                format!("icon:{}", px.key),
                image,
                egui::TextureOptions::LINEAR,
            );
            #[cfg(feature = "memtrace")]
            crate::memtrace::note_texture_upload();
            self.map.insert(px.key, Slot::Ready(tex));
        }
    }

    /// Texture for an executable path, requesting it if not yet cached.
    ///
    /// Returns the generic fallback while a real icon is in flight, so rows
    /// never flicker between "nothing" and "icon".
    pub fn get(&mut self, ctx: &egui::Context, path: Option<&Arc<str>>) -> egui::TextureId {
        let fallback = self.fallback_tex(ctx);
        #[cfg(feature = "memtrace")]
        if crate::memtrace::no_icons() {
            return fallback;
        }
        let Some(path) = path else {
            return fallback;
        };

        match self.map.get(path) {
            Some(Slot::Ready(tex)) => tex.id(),
            Some(_) => fallback,
            None => {
                // Rate-limit first-paint bursts: 600 processes would otherwise
                // enqueue 600 shell lookups in one frame.
                if self.budget > 0 {
                    self.budget -= 1;
                    self.map.insert(path.clone(), Slot::Pending);
                    self.loader.request(path.clone());
                }
                fallback
            }
        }
    }

    fn fallback_tex(&mut self, ctx: &egui::Context) -> egui::TextureId {
        if self.fallback.is_none() {
            // A muted rounded square: present enough to keep the column
            // aligned, quiet enough not to compete with real icons.
            let n = ICON_SIZE;
            let mut px = vec![0u8; n * n * 4];
            for y in 0..n {
                for x in 0..n {
                    let inset = 2;
                    let inside = x >= inset && y >= inset && x < n - inset && y < n - inset;
                    let corner = (x == inset || x == n - inset - 1)
                        && (y == inset || y == n - inset - 1);
                    if inside && !corner {
                        let o = (y * n + x) * 4;
                        let edge = x == inset
                            || y == inset
                            || x == n - inset - 1
                            || y == n - inset - 1;
                        let (v, a) = if edge { (0x8a, 190) } else { (0x4a, 110) };
                        px[o] = v;
                        px[o + 1] = v;
                        px[o + 2] = v;
                        px[o + 3] = a;
                    }
                }
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([n, n], &px);
            self.fallback = Some(ctx.load_texture(
                "icon:fallback",
                img,
                egui::TextureOptions::LINEAR,
            ));
        }
        self.fallback.as_ref().unwrap().id()
    }

    /// Drop textures for paths no longer on screen, keeping VRAM bounded on
    /// machines that churn through short-lived processes.
    pub fn trim(&mut self, keep: impl Fn(&Arc<str>) -> bool) {
        if self.map.len() > 512 {
            self.map.retain(|k, v| matches!(v, Slot::Missing) || keep(k));
        }
    }
}

// ---------------------------------------------------------------------------
// Nav-rail glyphs
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Processes,
    Performance,
    Details,
    Services,
    Startup,
    Search,
    Close,
    Shield,
    /// Panel outline with a filled left column: the sidebar toggle.
    Sidebar,
}

/// Draw a 16x16-ish glyph centred in `rect`.
pub fn glyph(painter: &Painter, rect: Rect, g: Glyph, color: Color32, _t: &Theme) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.5;
    let stroke = Stroke::new(1.5, color);

    match g {
        // Three stacked bars: a list of processes.
        Glyph::Processes => {
            for i in 0..3 {
                let y = c.y + (i as f32 - 1.0) * s * 0.62;
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(c.x - s * 0.8, y - s * 0.14),
                        Vec2::new(s * 1.6, s * 0.28),
                    ),
                    egui::CornerRadius::same(1),
                    color,
                );
            }
        }
        // A rising trace inside axes.
        Glyph::Performance => {
            painter.add(egui::Shape::line(
                vec![
                    Pos2::new(c.x - s * 0.85, c.y - s * 0.85),
                    Pos2::new(c.x - s * 0.85, c.y + s * 0.8),
                    Pos2::new(c.x + s * 0.9, c.y + s * 0.8),
                ],
                Stroke::new(1.4, color.gamma_multiply(0.55)),
            ));
            painter.add(egui::Shape::line(
                vec![
                    Pos2::new(c.x - s * 0.6, c.y + s * 0.35),
                    Pos2::new(c.x - s * 0.15, c.y - s * 0.3),
                    Pos2::new(c.x + s * 0.25, c.y + s * 0.1),
                    Pos2::new(c.x + s * 0.75, c.y - s * 0.62),
                ],
                stroke,
            ));
        }
        // A grid: rows and columns of detail.
        Glyph::Details => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 1.7));
            painter.rect_stroke(
                r,
                egui::CornerRadius::same(2),
                Stroke::new(1.3, color),
                egui::StrokeKind::Inside,
            );
            painter.hline(r.x_range(), c.y, Stroke::new(1.0, color));
            painter.vline(c.x, r.y_range(), Stroke::new(1.0, color));
        }
        // Interlocking gear-ish rings.
        Glyph::Services => {
            painter.circle_stroke(c, s * 0.78, Stroke::new(1.4, color));
            painter.circle_filled(c, s * 0.26, color);
            for i in 0..4 {
                let a = std::f32::consts::FRAC_PI_2 * i as f32 + std::f32::consts::FRAC_PI_4;
                let (sa, ca) = a.sin_cos();
                painter.add(egui::Shape::line_segment(
                    [
                        Pos2::new(c.x + ca * s * 0.78, c.y + sa * s * 0.78),
                        Pos2::new(c.x + ca * s * 1.05, c.y + sa * s * 1.05),
                    ],
                    Stroke::new(1.4, color),
                ));
            }
        }
        // An upward arrow leaving a baseline: launch at boot.
        Glyph::Startup => {
            painter.add(egui::Shape::line_segment(
                [
                    Pos2::new(c.x, c.y + s * 0.75),
                    Pos2::new(c.x, c.y - s * 0.7),
                ],
                stroke,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(c.x - s * 0.45, c.y - s * 0.3),
                    Pos2::new(c.x + s * 0.45, c.y - s * 0.3),
                    Pos2::new(c.x, c.y - s * 0.95),
                ],
                color,
                Stroke::NONE,
            ));
            painter.hline(
                egui::Rangef::new(c.x - s * 0.8, c.x + s * 0.8),
                c.y + s * 0.85,
                Stroke::new(1.4, color.gamma_multiply(0.6)),
            );
        }
        Glyph::Search => {
            painter.circle_stroke(
                Pos2::new(c.x - s * 0.15, c.y - s * 0.15),
                s * 0.55,
                Stroke::new(1.5, color),
            );
            painter.add(egui::Shape::line_segment(
                [
                    Pos2::new(c.x + s * 0.28, c.y + s * 0.28),
                    Pos2::new(c.x + s * 0.8, c.y + s * 0.8),
                ],
                stroke,
            ));
        }
        Glyph::Close => {
            let d = s * 0.55;
            painter.add(egui::Shape::line_segment(
                [
                    Pos2::new(c.x - d, c.y - d),
                    Pos2::new(c.x + d, c.y + d),
                ],
                stroke,
            ));
            painter.add(egui::Shape::line_segment(
                [
                    Pos2::new(c.x + d, c.y - d),
                    Pos2::new(c.x - d, c.y + d),
                ],
                stroke,
            ));
        }
        // A panel with its left column filled in: what the button does, drawn
        // rather than described. Reads the same collapsed or expanded, so the
        // control does not appear to change identity when clicked.
        Glyph::Sidebar => {
            let r = Rect::from_center_size(c, Vec2::new(s * 1.75, s * 1.5));
            painter.rect_stroke(
                r,
                egui::CornerRadius::same(2),
                Stroke::new(1.3, color),
                egui::StrokeKind::Inside,
            );
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(r.left() + 1.0, r.top() + 1.0),
                    Pos2::new(r.left() + s * 0.62, r.bottom() - 1.0),
                ),
                egui::CornerRadius::same(1),
                color,
            );
        }
        // Shield: elevation state.
        Glyph::Shield => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(c.x, c.y - s * 0.9),
                    Pos2::new(c.x + s * 0.72, c.y - s * 0.5),
                    Pos2::new(c.x + s * 0.55, c.y + s * 0.55),
                    Pos2::new(c.x, c.y + s * 0.95),
                    Pos2::new(c.x - s * 0.55, c.y + s * 0.55),
                    Pos2::new(c.x - s * 0.72, c.y - s * 0.5),
                ],
                color,
                Stroke::NONE,
            ));
        }
    }
}
