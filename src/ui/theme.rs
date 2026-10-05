//! This app's slice of the Azur theme.
//!
//! The palette, the semantic roles, egui's widget styling and Azur's components
//! all live in the `azur-egui-theme` crate, and the decisions a dense application
//! window makes on top of them live in its [`desktop`](azur_egui_theme::desktop)
//! module — one hover grey for everything, a selected row on the accent's surface
//! ramp, nothing structural rounded, 24px rows that touch, one baseline per row.
//! This window is exactly the shape that module is for, so it opts in whole:
//! [`Palette::build`] applies the palette move and [`install`] the density.
//!
//! The palettes themselves are the design system's — the same nine, in the same
//! order, as the file manager next door offers under its mark. See
//! [`azur::palettes`].
//!
//! What is left here is what only a task manager has an opinion about: which chart
//! color each resource gets, where the thresholds sit that turn a number amber or
//! red, and the one background role this window moves.
//!
//! [`Theme`] wraps the library's theme and derefs to it, so a call site reads the
//! same whichever layer a role comes from — `t.bg.layer` and `t.cpu` side by
//! side, no `t.az.bg` anywhere.

use azur_egui_theme as azur;
use egui::{Color32, CornerRadius};
use std::ops::Deref;

pub use azur::palettes::Palette;

pub struct Theme {
    az: azur::Theme,

    /// Which palette this is, for the tick in the theme menu and the settings file.
    pub palette: Palette,

    // --- graph series ---------------------------------------------------
    pub cpu: Color32,
    pub mem: Color32,
    pub disk: Color32,
    pub net: Color32,
    pub gpu: Color32,

    /// The radius for small painted furniture inside a card: pills, gauge frames.
    ///
    /// Not for anything structural -- a row, a header, a panel or a seam is square,
    /// which is [`azur::desktop::structural_radius`] and rule 3 of the preset.
    pub radius_sm: CornerRadius,
}

impl Deref for Theme {
    type Target = azur::Theme;

    fn deref(&self) -> &Self::Target {
        &self.az
    }
}

impl Theme {
    /// Azur's own dark palette, which is the default.
    ///
    /// Test-only: the window's palette comes from the settings file or the menu, and
    /// both go through [`Theme::of`]. A second way in is how a hard-coded dark theme
    /// ends up somewhere that should have followed the setting.
    #[cfg(test)]
    pub fn dark() -> Self {
        Self::of(Palette::Dark)
    }

    /// The palette by name, which is the constructor everything else goes through.
    pub fn of(palette: Palette) -> Self {
        use azur::tokens::{chart, palette as tokens, radius};

        // The application-window preset comes with the palette: one hover grey
        // shared by every place the pointer can land, moved *in the token*, so a
        // hand-painted row and an Azur button agree without either knowing about
        // the other. See `azur::desktop` and `Palette::build`.
        let (mut az, _) = palette.build();
        if palette == Palette::Dark {
            // Azur's canvas is GRAY_0, near-black. The panels are welded together
            // with seams now, so the canvas is no longer the gap between them --
            // what is left of it is the ground the Performance and Sensors cards
            // float on, and one step up is enough to keep a card's own edge visible
            // there. No other role depends on it. The other palettes name their own
            // canvas, and it is theirs to keep.
            az.bg.canvas = tokens::GRAY_1;
        }

        // The accent goes to CPU, the column a reader looks at first. Disk and
        // network take the status hues on purpose: a green line and a green number
        // mean the same thing.
        //
        // `tokens::chart` is cut for a dark surface, and these colour numbers as
        // well as lines, so the light side takes the rungs its own accent and
        // status hues sit on -- the ones Azur's light theme already measured as
        // text -- and violet and magenta walked down to the same lightness.
        let [cpu, mem, disk, net, gpu] = if az.dark {
            [chart::AZURE, chart::VIOLET, chart::GREEN, chart::AMBER, chart::MAGENTA]
        } else {
            [
                tokens::AZURE_60,
                Color32::from_rgb(0x7c, 0x52, 0xd9),
                tokens::GREEN_60,
                tokens::AMBER_60,
                Color32::from_rgb(0xc7, 0x41, 0x8a),
            ]
        };
        Self {
            az,
            palette,
            cpu,
            mem,
            disk,
            net,
            gpu,
            radius_sm: CornerRadius::same(radius::SMALL),
        }
    }

    /// Colour for a utilization number: calm until it actually matters.
    ///
    /// 60% is where a machine stops being idle and 90% is where it stops keeping
    /// up -- Task Manager's own reading of the same numbers.
    pub fn load_text(&self, pct: f32) -> Color32 {
        self.threshold_text(pct, 60.0, 90.0)
    }

    /// Commit charge, on the graph that draws the resident memory inside it.
    ///
    /// The memory hue, dimmed: private commit is the *envelope* -- everything the
    /// process has asked for, paged out or not -- and the working set inside it is
    /// the part that is in RAM right now. The same hue because they are one quantity
    /// at two sizes, and the dimmer of the two because the bright one is the figure a
    /// reader looks at first. Dimmed only as far as a caption in it stays legible,
    /// since the same colour labels the series.
    pub fn mem_commit(&self) -> Color32 {
        self.mem.gamma_multiply(0.72)
    }

    /// Kernel time, on the two graphs that draw it inside the CPU total.
    ///
    /// Red — the convention every tool that draws this figure uses, and the one
    /// colour on the page that cannot be mistaken for the azure it sits inside. It
    /// is the status hue rather than a red of its own, for the reason the series
    /// palette gives: a red line and a red number should mean the same thing.
    /// Shared from here because the Performance and Summary tabs both draw it, and
    /// two constants would drift.
    pub fn cpu_kernel(&self) -> Color32 {
        self.status.danger
    }
}

/// Install fonts and the Azur styling on the egui context.
pub fn install(ctx: &egui::Context, t: &Theme) {
    azur::fonts::install(ctx);
    restyle(ctx, t);
}

/// The styling alone, for a change of palette: the fonts do not change with it.
///
/// Only when the palette changes, never per frame: installing a style throws away
/// egui's galley and shape caches.
pub fn restyle(ctx: &egui::Context, t: &Theme) {
    azur::install(
        ctx,
        t,
        &azur::StyleOptions {
            // Azur's token is 280px, which is written for a hint of a few words.
            // The tooltips here are sentences -- what a column means, why a
            // control is disabled, what an action will cost -- and at 280px a
            // sentence becomes a narrow column six lines tall.
            tooltip_width: super::table::TOOLTIP_W,
            // 24px controls, and egui's own label selection off because every row
            // of every table is drawn by hand -- letting egui also treat its
            // labels as selectable text only produces stray highlights. Both come
            // from the preset rather than being spelled out again here.
            ..azur::desktop::density()
        },
    );
}
