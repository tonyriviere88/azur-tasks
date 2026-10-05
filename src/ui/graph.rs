//! Filled line graphs and sparklines, drawn as two shapes per series.
//!
//! Each series becomes one filled mesh plus one polyline, so a 60-sample graph
//! costs two draw commands regardless of sample count. The grid is drawn as a
//! fixed set of lines, phase-locked to the sample cadence like Task Manager's,
//! so it scrolls with the data instead of shimmering against it.

use super::theme::Theme;
use crate::sys::snapshot::{History, HISTORY_LEN};
use egui::{
    Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2,
};

/// How a graph picks its vertical extent.
#[derive(Clone, Copy)]
pub enum Scale {
    /// Fixed 0..=100, for percentages.
    Percent,
    /// Grow to fit the data, never shrinking below `floor`, and snapping to
    /// "nice" round maxima so the axis label stays readable.
    Auto { floor: f32 },
}

pub struct Graph<'a> {
    pub series: &'a [Series<'a>],
    pub scale: Scale,
    /// Draw the background grid.
    pub grid: bool,
    /// Number of columns/rows in the grid.
    pub grid_cols: usize,
    pub grid_rows: usize,
}

pub struct Series<'a> {
    pub hist: &'a History,
    pub color: Color32,
    /// Fill under the line. `None` draws the line only (for a second series).
    pub fill: bool,
}

/// The vertical maximum a graph settled on, so the caller can label the axis.
pub fn resolve_max(series: &[Series<'_>], scale: Scale) -> f32 {
    match scale {
        Scale::Percent => 100.0,
        Scale::Auto { floor } => {
            let peak = series
                .iter()
                .map(|s| s.hist.max())
                .fold(0.0f32, f32::max)
                .max(floor);
            nice_ceil(peak)
        }
    }
}

/// Round up to a 1 / 2 / 5 x 10^n boundary, so the axis reads "2 MB/s" rather
/// than "1.87 MB/s".
pub fn nice_ceil(v: f32) -> f32 {
    if v <= 0.0 {
        return 1.0;
    }
    let exp = v.log10().floor();
    let pow = 10f32.powf(exp);
    let m = v / pow;
    let m = if m <= 1.0 {
        1.0
    } else if m <= 2.0 {
        2.0
    } else if m <= 5.0 {
        5.0
    } else {
        10.0
    };
    m * pow
}

impl<'a> Graph<'a> {
    pub fn new(series: &'a [Series<'a>], scale: Scale) -> Self {
        Self {
            series,
            scale,
            grid: true,
            grid_cols: 10,
            grid_rows: 5,
        }
    }

    /// Paint into `rect`. Returns the resolved vertical maximum.
    pub fn paint(&self, ui: &Ui, t: &Theme, rect: Rect) -> f32 {
        let painter = ui.painter().with_clip_rect(rect);
        let max = resolve_max(self.series, self.scale);

        if self.grid {
            // Phase-lock the vertical lines to the newest sample so the grid
            // slides leftward with the data rather than standing still under it.
            let newest = self.series.first().map(|s| s.hist.len()).unwrap_or(0);
            let step = rect.width() / self.grid_cols as f32;
            let phase = (newest % (HISTORY_LEN / self.grid_cols)) as f32
                / (HISTORY_LEN / self.grid_cols) as f32;
            let mut x = rect.right() - phase * step;
            while x > rect.left() {
                painter.vline(x, rect.y_range(), Stroke::new(1.0, t.stroke.subtle));
                x -= step;
            }
            for i in 1..self.grid_rows {
                let y = rect.top() + rect.height() * i as f32 / self.grid_rows as f32;
                painter.hline(rect.x_range(), y, Stroke::new(1.0, t.stroke.subtle));
            }
        }

        for s in self.series {
            let n = s.hist.len();
            if n < 2 {
                continue;
            }

            // X is positioned so the newest sample sits on the right edge and a
            // partially filled history grows in from the right.
            let dx = rect.width() / (HISTORY_LEN - 1) as f32;
            let x_of = |i: usize| rect.right() - (n - 1 - i) as f32 * dx;
            let y_of = |v: f32| {
                let k = (v / max).clamp(0.0, 1.0);
                rect.bottom() - k * rect.height()
            };

            let mut line: Vec<Pos2> = Vec::with_capacity(n);
            for (i, v) in s.hist.iter().enumerate() {
                line.push(Pos2::new(x_of(i), y_of(v)));
            }

            if s.fill {
                fill_under(&painter, &line, rect.bottom(), fill_of(s.color));
            }

            painter.add(egui::Shape::line(line, Stroke::new(1.6, s.color)));
        }

        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(1.0, t.stroke.default),
            StrokeKind::Inside,
        );
        max
    }
}

/// The translucent band under a series line.
///
/// Shared, because it is what a wash of a resource's colour looks like in this
/// window: the Summary tab's ranking bars are the same series colour at the same
/// strength, so a card and the graph above it read as one system.
pub fn fill_of(c: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 56)
}

/// Fill the region between a polyline and a horizontal baseline.
///
/// This builds the triangles itself rather than calling
/// `Shape::convex_polygon`. The area under a curve is *not* convex, and egui's
/// convex tessellator fans every vertex from the first one -- which renders as
/// spurious lines and wedges radiating from the leftmost sample. Emitting one
/// quad per interval is both correct for any shape and cheaper: no path
/// tessellation, just 2 triangles per sample.
fn fill_under(painter: &egui::Painter, pts: &[Pos2], baseline: f32, color: Color32) {
    if pts.len() < 2 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    mesh.reserve_vertices(pts.len() * 2);
    mesh.reserve_triangles((pts.len() - 1) * 2);

    for (i, p) in pts.iter().enumerate() {
        // Two vertices per sample: the curve, and the baseline below it.
        mesh.colored_vertex(*p, color);
        mesh.colored_vertex(Pos2::new(p.x, baseline), color);
        if i > 0 {
            let base = (i as u32 - 1) * 2;
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 3, base + 2);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// A tiny filled graph, used for the nav-rail summaries and the per-core grid.
pub fn sparkline(
    ui: &Ui,
    t: &Theme,
    rect: Rect,
    hist: &History,
    color: Color32,
    max: f32,
    border: bool,
) {
    let series = [Series {
        hist,
        color,
        fill: true,
    }];
    sparklines(ui, t, rect, &series, max, border);
}

/// The same graph with more than one trace on it, for a Summary card whose
/// resource is two quantities: kernel time inside the CPU total, send beside
/// receive.
///
/// No grid and no axis labels -- that is the whole difference from [`Graph`], and
/// the reason a card is not one: at 90x40 a grid is noise, and the figure the axis
/// would carry is already the headline above the trace. Drawn in the order given,
/// so the trace that should read as *on top* comes last.
pub fn sparklines(
    ui: &Ui,
    t: &Theme,
    rect: Rect,
    series: &[Series<'_>],
    max: f32,
    border: bool,
) {
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, t.bg.canvas);

    for s in series {
        let n = s.hist.len();
        if n < 2 {
            continue;
        }
        let dx = rect.width() / (HISTORY_LEN - 1) as f32;
        let x_of = |i: usize| rect.right() - (n - 1 - i) as f32 * dx;
        let y_of = |v: f32| rect.bottom() - (v / max).clamp(0.0, 1.0) * rect.height();

        let mut line: Vec<Pos2> = Vec::with_capacity(n);
        for (i, v) in s.hist.iter().enumerate() {
            line.push(Pos2::new(x_of(i), y_of(v)));
        }
        if s.fill {
            fill_under(&painter, &line, rect.bottom(), fill_of(s.color));
        }
        painter.add(egui::Shape::line(line, Stroke::new(1.2, s.color)));
    }

    if border {
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(1.0, t.stroke.subtle),
            StrokeKind::Inside,
        );
    }
}

/// A horizontal proportion bar built from labelled segments -- the memory
/// composition strip.
pub fn segmented_bar(ui: &Ui, t: &Theme, rect: Rect, segments: &[(f32, Color32)]) {
    let painter = ui.painter();
    painter.rect_filled(rect, t.radius_sm, t.bg.canvas);

    let total: f32 = segments.iter().map(|(v, _)| *v).sum();
    if total <= 0.0 {
        return;
    }

    let inner = rect.shrink(1.0);
    let mut x = inner.left();
    for (i, (v, c)) in segments.iter().enumerate() {
        let w = inner.width() * (v / total);
        if w <= 0.0 {
            continue;
        }
        let seg = Rect::from_min_size(Pos2::new(x, inner.top()), Vec2::new(w, inner.height()));
        // Only round the outermost corners so the strip reads as one bar.
        let radius = if i == 0 {
            CornerRadius {
                nw: 3,
                sw: 3,
                ne: 0,
                se: 0,
            }
        } else if i + 1 == segments.len() {
            CornerRadius {
                nw: 0,
                sw: 0,
                ne: 3,
                se: 3,
            }
        } else {
            CornerRadius::ZERO
        };
        painter.rect_filled(seg, radius, *c);
        x += w;
    }

    painter.rect_stroke(
        rect,
        t.radius_sm,
        Stroke::new(1.0, t.stroke.default),
        StrokeKind::Inside,
    );
}
