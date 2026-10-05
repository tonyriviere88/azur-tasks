//! The app's semantic palette, mapped onto the Azur design tokens.
//!
//! Everything this app paints by hand — the nav rail, the table, the graphs —
//! asks for a *role* (`surface`, `text_dim`, `border_soft`) rather than a color.
//! This module is the only place where a role becomes a value, and every value
//! comes from [`crate::ui::azur_theme::palette`]; egui's own widget styling is
//! set up by [`azur_theme::apply`] in [`install`]. Nothing else in the UI names
//! a color of its own.

use super::azur_theme::{self, palette, radius, series};
use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke};

/// Named font role. `FontId` is cheap to clone, but building one per cell per
/// frame still means re-hashing; keeping them in the theme makes the call sites
/// read better too.
pub struct Fonts {
    /// Table cells, most body text.
    pub body: FontId,
    /// Column headers, labels.
    pub small: FontId,
    /// Group headers, card titles.
    pub medium: FontId,
    /// Section headings in the nav rail.
    pub nav: FontId,
    /// Command lines, paths, PIDs where alignment helps.
    pub mono: FontId,
}

pub struct Theme {
    // --- surfaces -------------------------------------------------------
    /// Window background: Azur's `background-canvas`.
    pub bg: Color32,
    /// Navigation rail and status bar — chrome, raised off the canvas.
    pub nav: Color32,
    /// Cards, table header strip.
    pub surface: Color32,
    /// Raised surface: hover, selected tab.
    pub surface_hi: Color32,
    /// Alternating table row.
    pub row_alt: Color32,
    pub row_hover: Color32,
    pub row_selected: Color32,

    // --- lines ----------------------------------------------------------
    pub border: Color32,
    pub border_soft: Color32,
    pub grid: Color32,

    // --- scrollbars -----------------------------------------------------
    /// Groove behind the handle.
    pub scroll_track: Color32,
    /// The handle at rest, and under the pointer.
    pub scroll_handle: Color32,
    pub scroll_handle_hover: Color32,

    /// Recessed plot background: graphs and the memory composition strip.
    pub plot_bg: Color32,

    // --- text -----------------------------------------------------------
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,

    // --- accents --------------------------------------------------------
    pub accent: Color32,
    pub accent_soft: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub crit: Color32,

    // --- graph series ---------------------------------------------------
    pub cpu: Color32,
    pub mem: Color32,
    pub disk: Color32,
    pub net: Color32,
    pub gpu: Color32,

    pub fonts: Fonts,

    pub radius_sm: CornerRadius,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            // Azur's dark scale is canvas-first: the page is the darkest thing
            // on screen and panels sit *above* it. So the rail and the status
            // bar are a step lighter than the content area, not darker -- the
            // inverse of what Fluent does, and the reason the border between
            // them can stay as quiet as it is.
            //
            // The ladder starts at GRAY_3 rather than at the bottom of the
            // scale: GRAY_0 through GRAY_2 are near-black, which is a lot of
            // black for a window that stays open all day. Every level moves up
            // together, so this is a lift in brightness and not a loss of depth
            // -- each one still clears the one below it as it did before.
            bg: palette::GRAY_3,           // background-canvas
            nav: palette::GRAY_2,          // background-layer: chrome, above the canvas
            surface: palette::GRAY_5,      // background-layer-alt: cards, header strip
            surface_hi: palette::GRAY_6,   // background-control-hover
            row_alt: palette::GRAY_4,      // one step off the canvas: zebra, not stripes
            row_hover: palette::GRAY_5,
            // `accent-subtle` alone is nearly invisible under a row of text, and
            // unlike egui's selection there is no accent stroke here to carry
            // it. Lifting it halfway to `azure-60` keeps the selected row inside
            // the accent ramp while making it legible at a glance.
            row_selected: palette::AZURE_10.lerp_to_gamma(palette::AZURE_60, 0.5),

            border: palette::GRAY_7,       // stroke-default, kept clear of surface_hi
            border_soft: palette::GRAY_5,  // stroke-subtle
            // Not lifted with the rest: the plot it rules only moved a few
            // units (the low end of the scale is tightly spaced), so a full step
            // here would turn quiet guides into a cage.
            grid: palette::GRAY_5,

            // The two recessed roles are the ones that do *not* move up with the
            // canvas: a groove and a plot read as cut into what surrounds them,
            // and that gap has to come from below. They are why the bottom of
            // the scale is still worth having.
            scroll_track: palette::GRAY_3,
            scroll_handle: palette::GRAY_6,
            scroll_handle_hover: palette::GRAY_9,

            plot_bg: palette::GRAY_2,

            text: palette::GRAY_16,        // text-primary
            text_dim: palette::GRAY_12,    // text-secondary
            text_faint: palette::GRAY_10,  // text-tertiary

            accent: palette::AZURE_ACCENT,
            accent_soft: palette::AZURE_10,
            ok: palette::GREEN_70,
            warn: palette::AMBER_70,
            crit: palette::RED_70,

            cpu: series::CPU,
            mem: series::MEM,
            disk: series::DISK,
            net: series::NET,
            gpu: series::GPU,

            fonts: Fonts {
                body: FontId::new(13.0, FontFamily::Proportional),
                small: FontId::new(12.0, FontFamily::Proportional),
                medium: FontId::new(13.5, FontFamily::Name("semibold".into())),
                nav: FontId::new(13.0, FontFamily::Proportional),
                mono: FontId::new(12.0, FontFamily::Monospace),
            },

            radius_sm: CornerRadius::same(radius::SMALL),
        }
    }

    /// Task Manager tints its CPU / memory / disk cells on a warm scale so a hot
    /// process is findable without reading a single number. `t` is 0..=1.
    pub fn heat(&self, t: f32) -> Color32 {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.001 {
            return Color32::TRANSPARENT;
        }
        // Amber -> red along Azur's status hues, with alpha ramping in so light
        // load stays calm and a busy process is the only thing that glows.
        let c = self.warn.lerp_to_gamma(self.crit, t);
        let alpha = (26.0 + 74.0 * t) as u8;
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
    }

    /// Colour for a utilization number: calm until it actually matters.
    pub fn load_text(&self, pct: f32) -> Color32 {
        if pct >= 90.0 {
            self.crit
        } else if pct >= 60.0 {
            self.warn
        } else {
            self.text
        }
    }
}

/// Make hover change only the background colour, for lists of selectable rows.
///
/// `Button::selectable` — which backs `Ui::selectable_label` and so every entry
/// in a combo popup — draws no frame while an entry is neither selected nor
/// hovered, then a fully stroked one the moment it is. `Frame::total_margin`
/// counts `stroke.width`, so appearing borders do not just decorate: they make
/// the entry 2px wider and taller.
///
/// In a combo popup that is a feedback loop. The popup sets its own minimum
/// width from its content, so the wider entry widens the popup, which widens the
/// content, which widens the popup; the vertical scrollbar flickers in and out
/// as the height changes and the whole thing creeps outwards under the cursor.
/// Clearing the stroke in *every* state fixes both halves at once: no border to
/// appear, and identical geometry hovered or not.
pub fn list_items(ui: &mut egui::Ui, t: &Theme) {
    let w = &mut ui.visuals_mut().widgets;
    for (state, fill) in [
        // Transparent when idle, so entries read as a list and not as a
        // column of buttons.
        (&mut w.inactive, Color32::TRANSPARENT),
        (&mut w.hovered, t.surface_hi),
        (&mut w.active, t.surface_hi),
        (&mut w.open, t.surface_hi),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::NONE;
    }
}

/// Install fonts and push our palette into egui's own widget styling.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Segoe UI is present on every supported Windows version. Using the system
    // font rather than an embedded one keeps the binary small and makes the app
    // look native next to Task Manager itself.
    let mut loaded_ui = false;
    for (key, path) in [
        ("segoe", r"C:\Windows\Fonts\segoeui.ttf"),
        ("segoe-semibold", r"C:\Windows\Fonts\segoeuisb.ttf"),
        ("segoe-bold", r"C:\Windows\Fonts\segoeuib.ttf"),
        ("consolas", r"C:\Windows\Fonts\consola.ttf"),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(key.to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            if key == "segoe" {
                loaded_ui = true;
            }
        }
    }

    if loaded_ui {
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe".to_owned());
    }
    if fonts.font_data.contains_key("consolas") {
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "consolas".to_owned());
    }

    // A dedicated semibold family, so headings can be heavier without faking it
    // by scaling the regular weight up.
    let semibold: Vec<String> = ["segoe-semibold", "segoe-bold", "segoe"]
        .iter()
        .filter(|k| fonts.font_data.contains_key(**k))
        .map(|k| (*k).to_owned())
        .collect();
    fonts.families.insert(
        FontFamily::Name("semibold".into()),
        if semibold.is_empty() {
            // Fall back to egui's bundled font so headings still render.
            fonts
                .families
                .get(&FontFamily::Proportional)
                .cloned()
                .unwrap_or_default()
        } else {
            semibold
        },
    );

    ctx.set_fonts(fonts);

    // Azur owns the base look: surfaces, widget states, rounding and the 4px
    // spacing rhythm. It replaces the whole `Style`, so it goes first and
    // everything below is this app's own departure from it.
    azur_theme::apply(ctx, azur_theme::Theme::Dark);

    let t = Theme::dark();
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        let v = &mut s.visuals;
        // The panels are painted with explicit `Frame` fills, but a default-framed
        // one (and the area behind a resize handle) should still land on the same
        // canvas the rest of the app uses.
        v.panel_fill = t.bg;
        // Azur's popover surface reads as a card in the middle of a dialog; the
        // app's own cards are `surface`, and the modal should match them.
        v.window_fill = t.surface;
        v.window_stroke = Stroke::new(1.0, t.border);
        v.faint_bg_color = t.row_alt;
        v.extreme_bg_color = t.plot_bg;
        v.text_cursor.blink = true;

        // A selected list entry is a fill, not a fill plus a ring: egui expands
        // a widget's rect by its stroke width, and in a combo popup that feeds
        // back into the popup's own width (see `list_items`).
        v.selection.bg_fill = t.accent_soft;
        v.selection.stroke = Stroke::new(1.0, t.accent);

        // This canvas sits higher up the scale than Azur's, so the control layer
        // has to rise with it -- `background-control` against a lifted canvas is
        // barely raised at all. Controls take `surface`, which is also what the
        // hand-drawn search field is filled with, so the toolbar reads as one
        // row of controls at one height. Azur's strokes and its accent-filled
        // pressed state are left alone.
        for (w, fill) in [
            (&mut v.widgets.inactive, t.surface),
            (&mut v.widgets.hovered, t.surface_hi),
            (&mut v.widgets.open, t.surface_hi),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
        }

        // Azur pins its controls at 32px; this window is a dense table view and
        // the toolbar is laid out at 28. Matching it keeps menu entries and
        // buttons the same height as the chrome around them.
        s.spacing.interact_size.y = azur_theme::control::SMALL;
        s.spacing.scroll.bar_width = 10.0;
        // A floating scrollbar overlaps the last column's text.
        s.spacing.scroll.floating = false;
        s.interaction.tooltip_delay = 0.35;
        // Every row of every table is drawn by hand; letting egui also treat its
        // labels as selectable text only produces stray highlights.
        s.interaction.selectable_labels = false;
    });
}
