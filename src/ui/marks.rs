//! The four application marks, as geometry.
//!
//! One definition per application, drawn two ways: painted into an
//! [`azur_egui_theme::icons::Icon`] for the title bar, and rasterised into the
//! PNG/`.ico` set for the executable. The geometry is stated once so the two can
//! not drift — which is the whole of `docs/icons.md` §12 in one file.
//!
//! Coordinates are on Azur's 16-unit icon grid, the same numbers you would write
//! in an SVG `viewBox="0 0 16 16"`, and the art stays inside 2–14 so it keeps the
//! margin §2 asks for.
//!
//! Drop this file into an application and use the four functions at the bottom:
//!
//! ```ignore
//! TitleBar::new("Sessions").icon(&marks::task_manager).show(ui);
//! ui.add(Button::new("Mixer").icon(&marks::audio_mixer));
//! ```

use azur_egui_theme::icons::Pen;
use egui::{Color32, Painter, Rect};

/// One drawing instruction on the 16-unit grid.
///
/// Deliberately small: four shapes cover all four marks, and anything a mark
/// cannot say in these is detail that would die at 16px anyway.
#[derive(Clone, Copy)]
pub enum Prim {
    /// An open stroked polyline — the `M … L …` of an SVG path.
    Path(&'static [(f32, f32)]),
    /// A stroked polyline that closes back on itself — a path ending in `Z`.
    Closed(&'static [(f32, f32)]),
    /// A stroked straight line.
    Line(f32, f32, f32, f32),
    /// A rounded rectangle: `x, y, width, height, corner radius`.
    ///
    /// `fill: true` where an outline would be self-defeating — a 2.6-unit bar
    /// stroked at 1.4 units is a solid bar with a seam down it. `docs/icons.md` §4
    /// says outline first, and this is the exception it allows for shapes too
    /// narrow to hold one.
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        fill: bool,
    },
}

/// An application's mark.
pub struct Mark {
    /// For a window title and a preview caption.
    pub name: &'static str,
    /// The folder its icon files live in.
    pub slug: &'static str,
    /// Stroke weight in grid units, for the **raster** mark only: a glyph on a
    /// plate carries more weight than one in a menu row.
    ///
    /// The painted version ignores this and takes
    /// [`azur_egui_theme::icons::stroke_width`] instead, so it matches the built-in
    /// chevrons and ticks it will sit beside.
    pub stroke: f32,
    pub prims: &'static [Prim],
}

/// A bar chart: three columns on a baseline.
///
/// Ascending heights and the rule underneath say *chart* — which is what keeps it
/// apart from [`AUDIO_MIXER`]'s faders at 16px, where both are three vertical
/// things. Columns are solid and grow from the rule; faders are thin, full height,
/// and have no rule.
pub const TASK_MANAGER: Mark = Mark {
    name: "Task manager",
    slug: "task-manager",
    stroke: 1.4,
    prims: &[
        Prim::Rect {
            x: 3.6,
            y: 9.0,
            w: 2.6,
            h: 3.7,
            r: 0.7,
            fill: true,
        },
        Prim::Rect {
            x: 6.7,
            y: 6.2,
            w: 2.6,
            h: 6.5,
            r: 0.7,
            fill: true,
        },
        Prim::Rect {
            x: 9.8,
            y: 3.4,
            w: 2.6,
            h: 9.3,
            r: 0.7,
            fill: true,
        },
        Prim::Line(2.6, 13.4, 13.4, 13.4),
    ],
};

/// A folder, in one closed outline: up the left side, across the tab, down the
/// right.
///
/// No inner flap line and no rounded corners — at 16px the flap is a smudge and a
/// 1-unit radius is a sixteenth of a pixel.
pub const FILE_EXPLORER: Mark = Mark {
    name: "File explorer",
    slug: "file-explorer",
    stroke: 1.4,
    prims: &[Prim::Closed(&[
        (2.6, 12.9),
        (2.6, 3.9),
        (6.3, 3.9),
        (7.6, 5.7),
        (13.4, 5.7),
        (13.4, 12.9),
    ])],
};

/// Three faders, knobs at three heights.
///
/// The knobs are what make this a mixer rather than a chart: full-height tracks
/// with something sitting on them, all three at different settings. Knob widths
/// just meet, and the heights are staggered so neighbours never collide.
pub const AUDIO_MIXER: Mark = Mark {
    name: "Audio mixer",
    slug: "audio-mixer",
    stroke: 1.3,
    prims: &[
        Prim::Line(4.4, 3.0, 4.4, 13.0),
        Prim::Line(8.0, 3.0, 8.0, 13.0),
        Prim::Line(11.6, 3.0, 11.6, 13.0),
        Prim::Rect {
            x: 2.6,
            y: 8.7,
            w: 3.6,
            h: 1.9,
            r: 0.95,
            fill: true,
        },
        Prim::Rect {
            x: 6.2,
            y: 4.9,
            w: 3.6,
            h: 1.9,
            r: 0.95,
            fill: true,
        },
        Prim::Rect {
            x: 9.8,
            y: 7.1,
            w: 3.6,
            h: 1.9,
            r: 0.95,
            fill: true,
        },
    ],
};

/// A 2×2 grid of tiles: the app grid every launcher has taught people to read.
///
/// Solid, because a 4.6-unit tile outlined at 1.4 units is a ring with a hole the
/// size of the stroke.
///
/// The gap between tiles is 1.2 units, which looks like more air than it needs at
/// 256px and is the whole mark at 16px: a unit is one pixel there, and a 0.8-unit
/// gap rounds away and fuses all four tiles into one rounded square. Whatever
/// separates two shapes has to survive being a single pixel.
pub const PROGRAM_LAUNCHER: Mark = Mark {
    name: "Program launcher",
    slug: "program-launcher",
    stroke: 1.4,
    prims: &[
        Prim::Rect {
            x: 2.8,
            y: 2.8,
            w: 4.6,
            h: 4.6,
            r: 1.3,
            fill: true,
        },
        Prim::Rect {
            x: 8.6,
            y: 2.8,
            w: 4.6,
            h: 4.6,
            r: 1.3,
            fill: true,
        },
        Prim::Rect {
            x: 2.8,
            y: 8.6,
            w: 4.6,
            h: 4.6,
            r: 1.3,
            fill: true,
        },
        Prim::Rect {
            x: 8.6,
            y: 8.6,
            w: 4.6,
            h: 4.6,
            r: 1.3,
            fill: true,
        },
    ],
};

/// All four, in the order the preview sheet shows them.
pub const ALL: [&Mark; 4] = [
    &TASK_MANAGER,
    &FILE_EXPLORER,
    &AUDIO_MIXER,
    &PROGRAM_LAUNCHER,
];

/// Paint a mark into `rect`, in `color` — the `Icon` contract.
///
/// Weight comes from the box, not from [`Mark::stroke`], so a mark drawn here sits
/// at the same weight as the chevron beside it.
pub fn paint(mark: &Mark, painter: &Painter, rect: Rect, color: Color32) {
    let pen = Pen::new(painter, rect, color);
    for prim in mark.prims {
        match *prim {
            Prim::Path(points) => pen.path(points),
            Prim::Closed(points) => pen.closed_path(points),
            Prim::Line(x1, y1, x2, y2) => pen.line(x1, y1, x2, y2),
            Prim::Rect {
                x,
                y,
                w,
                h,
                r,
                fill,
            } => {
                if fill {
                    pen.rect_filled(x, y, w, h, r);
                } else {
                    pen.rect(x, y, w, h, r);
                }
            }
        }
    }
}

pub fn task_manager(painter: &Painter, rect: Rect, color: Color32) {
    paint(&TASK_MANAGER, painter, rect, color);
}

pub fn file_explorer(painter: &Painter, rect: Rect, color: Color32) {
    paint(&FILE_EXPLORER, painter, rect, color);
}

pub fn audio_mixer(painter: &Painter, rect: Rect, color: Color32) {
    paint(&AUDIO_MIXER, painter, rect, color);
}

pub fn program_launcher(painter: &Painter, rect: Rect, color: Color32) {
    paint(&PROGRAM_LAUNCHER, painter, rect, color);
}
