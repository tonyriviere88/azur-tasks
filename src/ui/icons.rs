//! Process icons: an async, deduplicated path -> GPU texture cache.
//!
//! Also holds the hand-drawn nav-rail glyphs. Those are painted as vectors
//! rather than taken from an icon font: it removes a runtime dependency on a
//! specific Segoe glyph set (where a wrong codepoint renders as a tofu box) and
//! keeps them crisp at any DPI.

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
    /// Four tiles, one of them filled: a page of readings.
    Summary,
    Processes,
    Performance,
    Details,
    Services,
    Startup,
    Shield,
    /// Panel outline with a filled left column: the sidebar toggle.
    Sidebar,
    /// Panel outline with a filled bottom strip: the inspector toggle.
    BottomPanel,
    /// A circular arrow: sample now.
    Refresh,
    /// A chronometer: the sampling cadence.
    Stopwatch,
    /// A dial with a needle: the sensors.
    Gauge,
}

/// The glyphs as `azur_egui_theme::icons::Icon` functions.
///
/// A component that takes an icon takes `&dyn Fn(&Painter, Rect, Color32)`, which
/// is [`draw`] with the glyph already chosen — so `NavItem`, `CommandItem`,
/// `MenuItem` and `IconButton` can all carry this app's own artwork.
pub mod icon {
    use super::{draw, Glyph};
    use egui::{Color32, Painter, Rect};

    pub fn summary(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Summary, color);
    }
    pub fn processes(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Processes, color);
    }
    pub fn performance(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Performance, color);
    }
    pub fn details(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Details, color);
    }
    pub fn services(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Services, color);
    }
    pub fn startup(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Startup, color);
    }
    pub fn shield(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Shield, color);
    }
    pub fn sidebar(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Sidebar, color);
    }
    pub fn bottom_panel(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::BottomPanel, color);
    }
    pub fn refresh(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Refresh, color);
    }
    pub fn stopwatch(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Stopwatch, color);
    }
    pub fn gauge(p: &Painter, rect: Rect, color: Color32) {
        draw(p, rect, Glyph::Gauge, color);
    }
}

/// Draw a 16x16-ish glyph centred in `rect`.
pub fn draw(painter: &Painter, rect: Rect, g: Glyph, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.5;
    let stroke = Stroke::new(1.5, color);

    match g {
        // Four tiles in a square, the first of them filled: a page of readings, of
        // which one is the one you came for. Distinct at 16px from `Processes`
        // (a row with two branching under it) and from `Details` (three bulleted
        // lines).
        Glyph::Summary => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 1.72));
            let gap = s * 0.26;
            let tile = (r.width() - gap) * 0.5;
            for (i, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
                .into_iter()
                .enumerate()
            {
                let t = Rect::from_min_size(
                    Pos2::new(r.left() + dx * (tile + gap), r.top() + dy * (tile + gap)),
                    Vec2::splat(tile),
                );
                if i == 0 {
                    painter.rect_filled(t, egui::CornerRadius::same(1), color);
                } else {
                    painter.rect_stroke(
                        t,
                        egui::CornerRadius::same(1),
                        Stroke::new(1.2, color),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        // A tree view: an expanded row with two indented under it. Which is what the
        // Processes tab is and `Details` is not -- the same processes, listed flat --
        // so the nesting is the whole difference between the two glyphs.
        //
        // The caret is doing the work, and it is why there is no trunk. Elbows down
        // the left edge are how a tree view actually draws itself, but in a 16px box
        // a top bar, a left spine and two stubs come out as a letter E, and no amount
        // of moving the spine fixes it -- the silhouette is the problem, not the
        // proportions. A caret has no such collision: nothing else in this rail is a
        // triangle beside a row, it says "this row opens" on its own, and with the
        // rows stepping right under it the reader is told the same thing twice.
        Glyph::Processes => {
            let x0 = c.x - s * 0.85;
            let right = c.x + s * 0.85;
            let h = s * 0.26;
            let rows = [c.y - s * 0.62, c.y, c.y + s * 0.62];
            let bar = |x: f32, y: f32| {
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(x, y - h * 0.5), Vec2::new(right - x, h)),
                    egui::CornerRadius::same(1),
                    color,
                );
            };
            // Pointing down, because the two rows under it are its children: a caret
            // still pointing right would be a row that has not been opened yet.
            let (cw, ch) = (s * 0.54, s * 0.34);
            let cx = x0 + cw * 0.5;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(cx - cw * 0.5, rows[0] - ch * 0.5),
                    Pos2::new(cx + cw * 0.5, rows[0] - ch * 0.5),
                    Pos2::new(cx, rows[0] + ch * 0.5),
                ],
                color,
                Stroke::NONE,
            ));
            bar(x0 + s * 0.62, rows[0]);
            // A mark on each child: the caret says the first row opens, and a dot on
            // the two under it says they are what came out of it. Their own rows are
            // short as a consequence -- a caret, a dot and two bars is all a 16px box
            // has room for -- and short is right anyway, since the row that matters
            // here is the parent's.
            //
            // One indent level, and it is the parent's own left edge: the children's
            // marks sit in the column the parent's row starts in, with the caret in
            // the column to the left of it. Any deeper and a 16px glyph has spent
            // half its width on white space.
            let dot = s * 0.13;
            for y in [rows[1], rows[2]] {
                let x = x0 + s * 0.62;
                painter.circle_filled(Pos2::new(x + dot, y), dot, color);
                bar(x + dot * 2.8, y);
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
        // A bulleted list: three rows, each with a mark of its own and none of them
        // under any other. This tab lists the same processes `Processes` does and
        // draws them flat, so what separates the two glyphs is where the marks sit:
        // three of them on one margin here, against a caret and two marks stepped in
        // under it there. The dot is a shade taller than the bar it heads, or at 16px
        // the two merge into one long line.
        Glyph::Details => {
            let dot = s * 0.17;
            let left = c.x - s * 0.85;
            let text = left + dot * 3.6;
            for i in 0..3 {
                let y = c.y + (i as f32 - 1.0) * s * 0.62;
                painter.circle_filled(Pos2::new(left + dot, y), dot, color);
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(text, y - s * 0.14),
                        Vec2::new(left + s * 1.7 - text, s * 0.28),
                    ),
                    egui::CornerRadius::same(1),
                    color,
                );
            }
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
        // The same panel as `Sidebar`, filled along the bottom instead: what the
        // inspector does to the view it opens in. Built from the same rect and the
        // same 1px inset, so the two toggles read as one family of control.
        Glyph::BottomPanel => {
            let r = Rect::from_center_size(c, Vec2::new(s * 1.75, s * 1.5));
            painter.rect_stroke(
                r,
                egui::CornerRadius::same(2),
                Stroke::new(1.3, color),
                egui::StrokeKind::Inside,
            );
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(r.left() + 1.0, r.bottom() - s * 0.55),
                    Pos2::new(r.right() - 1.0, r.bottom() - 1.0),
                ),
                egui::CornerRadius::same(1),
                color,
            );
        }
        // An open circle with an arrowhead on the end: re-sample.
        Glyph::Refresh => {
            let r = s * 0.72;
            // Leave a gap at the top-right for the head, so the ring reads as an
            // arrow rather than as a letter O.
            let arc: Vec<Pos2> = (0..=22)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_4
                        + std::f32::consts::TAU * 0.82 * i as f32 / 22.0;
                    Pos2::new(c.x + a.cos() * r, c.y + a.sin() * r)
                })
                .collect();
            painter.add(egui::Shape::line(arc, Stroke::new(1.4, color)));
            let tip = Pos2::new(c.x + r * 0.71, c.y - r * 0.71);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(tip.x - s * 0.1, tip.y - s * 0.45),
                    Pos2::new(tip.x + s * 0.45, tip.y - s * 0.1),
                    Pos2::new(tip.x - s * 0.35, tip.y + s * 0.3),
                ],
                color,
                Stroke::NONE,
            ));
        }
        // A dial: 240 degrees of arc with a needle at two thirds, which is what
        // the tab it opens is full of.
        Glyph::Gauge => {
            let r = s * 0.8;
            // Open at the bottom, so it reads as a gauge rather than a clock.
            let arc: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (0.8 + 1.4 * i as f32 / 16.0);
                    Pos2::new(c.x + a.cos() * r, c.y + s * 0.2 + a.sin() * r)
                })
                .collect();
            painter.add(egui::Shape::line(arc, Stroke::new(1.4, color)));
            let a = std::f32::consts::PI * 1.75;
            painter.add(egui::Shape::line_segment(
                [
                    Pos2::new(c.x, c.y + s * 0.2),
                    Pos2::new(c.x + a.cos() * r * 0.8, c.y + s * 0.2 + a.sin() * r * 0.8),
                ],
                Stroke::new(1.5, color),
            ));
            painter.circle_filled(Pos2::new(c.x, c.y + s * 0.2), s * 0.14, color);
        }
        // A chronometer: how often the machine is sampled.
        //
        // A plain clock would do for "time", but the crown is what says *elapsed*
        // time -- which is what a sampling interval is.
        Glyph::Stopwatch => {
            let r = s * 0.72;
            // The dial sits low in the box to leave room for the crown above it,
            // so the glyph still centres on the box as a whole.
            let dial = Pos2::new(c.x, c.y + s * 0.16);
            painter.circle_stroke(dial, r, Stroke::new(1.4, color));

            let crown = dial.y - r - s * 0.32;
            painter.add(egui::Shape::line_segment(
                [Pos2::new(c.x, crown), Pos2::new(c.x, dial.y - r)],
                Stroke::new(1.5, color),
            ));
            painter.hline(
                egui::Rangef::new(c.x - s * 0.26, c.x + s * 0.26),
                crown,
                Stroke::new(1.5, color),
            );

            // Two hands, one at twelve and one short of four. A single hand reads
            // as a compass needle; two read as a watch.
            painter.add(egui::Shape::line_segment(
                [dial, Pos2::new(dial.x, dial.y - r * 0.62)],
                Stroke::new(1.4, color),
            ));
            painter.add(egui::Shape::line_segment(
                [dial, Pos2::new(dial.x + r * 0.44, dial.y + r * 0.3)],
                Stroke::new(1.3, color),
            ));
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
