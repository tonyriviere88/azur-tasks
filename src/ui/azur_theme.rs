//! Azur theme for egui — a port of the Azur design tokens (see `tokens.css`)
//! onto `egui::Visuals` / `egui::Style`. This does not reimplement Azur's React
//! components; egui already provides widgets (Button, ComboBox, CollapsingHeader
//! for tree-like UI, tables, etc.) — this module just makes them *look* like
//! Azur by configuring color, spacing, and rounding.
//!
//! Two things use it:
//!
//! * [`apply`] installs the base `Visuals`/`Spacing` on a context. Call it
//!   before any app-specific tweaks, since it replaces the whole `Style`.
//! * [`palette`] is the token scale itself. Everything this app paints by hand —
//!   the nav rail, the table, the graphs — goes through the semantic roles in
//!   [`crate::ui::theme::Theme`], which are mapped onto these tokens rather than
//!   onto colors of their own.
//!
//! Written against egui 0.35: `CornerRadius` (not `Rounding`), `Shadow` with
//! integer offset/blur, and `Context::set_style_of`.

use egui::{Color32, CornerRadius, Shadow, Stroke, Style};

pub enum Theme {
    Dark,
    /// This app ships dark-only; the light side of the token set is kept so a
    /// theme switch stays a one-line change rather than a second port.
    #[allow(dead_code)]
    Light,
}

/// Raw Azur palette, transcribed from `src/styles/tokens.css`.
///
/// Transcribed whole, so the scale can be read here as it is in `tokens.css` —
/// hence the allow: the steps this app has no use for yet are still the
/// reference for the ones it does.
#[allow(dead_code)]
pub mod palette {
    use egui::Color32;

    // Neutrals (dark-first scale)
    pub const GRAY_0: Color32 = Color32::from_rgb(0x07, 0x08, 0x09);
    pub const GRAY_1: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x10);
    pub const GRAY_2: Color32 = Color32::from_rgb(0x14, 0x17, 0x1a);
    pub const GRAY_3: Color32 = Color32::from_rgb(0x19, 0x1b, 0x1f);
    pub const GRAY_4: Color32 = Color32::from_rgb(0x20, 0x23, 0x29);
    pub const GRAY_5: Color32 = Color32::from_rgb(0x28, 0x2b, 0x32);
    pub const GRAY_6: Color32 = Color32::from_rgb(0x33, 0x37, 0x3f);
    pub const GRAY_7: Color32 = Color32::from_rgb(0x3f, 0x43, 0x4c);
    pub const GRAY_8: Color32 = Color32::from_rgb(0x4d, 0x52, 0x5c);
    pub const GRAY_9: Color32 = Color32::from_rgb(0x5c, 0x62, 0x6d);
    pub const GRAY_10: Color32 = Color32::from_rgb(0x6d, 0x73, 0x80);
    pub const GRAY_12: Color32 = Color32::from_rgb(0x9a, 0xa1, 0xad);
    pub const GRAY_13: Color32 = Color32::from_rgb(0xb3, 0xb9, 0xc4);
    pub const GRAY_14: Color32 = Color32::from_rgb(0xcc, 0xd1, 0xd9);
    pub const GRAY_15: Color32 = Color32::from_rgb(0xe4, 0xe7, 0xec);
    pub const GRAY_16: Color32 = Color32::from_rgb(0xf5, 0xf6, 0xf8);
    pub const WHITE: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);

    // Azure accent
    pub const AZURE_10: Color32 = Color32::from_rgb(0x07, 0x1a, 0x2c);
    pub const AZURE_60: Color32 = Color32::from_rgb(0x26, 0x78, 0xc2);
    pub const AZURE_ACCENT: Color32 = Color32::from_rgb(0x3a, 0xa0, 0xff);
    pub const AZURE_ACCENT_HOVER: Color32 = Color32::from_rgb(0x5c, 0xb0, 0xff);
    pub const AZURE_ACCENT_ACTIVE: Color32 = Color32::from_rgb(0x24, 0x88, 0xe6);
    pub const AZURE_100: Color32 = Color32::from_rgb(0xc2, 0xe2, 0xff);

    // Status
    pub const GREEN_70: Color32 = Color32::from_rgb(0x3f, 0xc8, 0x5f);
    pub const AMBER_70: Color32 = Color32::from_rgb(0xe0, 0xa5, 0x30);
    pub const RED_70: Color32 = Color32::from_rgb(0xea, 0x4d, 0x54);
    pub const GREEN_60: Color32 = Color32::from_rgb(0x29, 0xa7, 0x44);
    pub const AMBER_60: Color32 = Color32::from_rgb(0xc9, 0x8a, 0x12);
    pub const RED_60: Color32 = Color32::from_rgb(0xd6, 0x3a, 0x41);
}

/// Data-series colors for the sparklines and the performance graphs.
///
/// Azur's token file has no chart scale, so these are the accent and the two
/// status hues (which the graphs share with the load colors, deliberately: a
/// red line and a red number mean the same thing), plus a violet and a magenta
/// picked to hold their own against the azure at the same lightness.
pub mod series {
    use super::palette;
    use egui::Color32;

    pub const CPU: Color32 = palette::AZURE_ACCENT;
    pub const MEM: Color32 = Color32::from_rgb(0xa2, 0x7b, 0xf0);
    pub const DISK: Color32 = palette::GREEN_70;
    pub const NET: Color32 = palette::AMBER_70;
    pub const GPU: Color32 = Color32::from_rgb(0xf0, 0x6c, 0xb0);
}

/// Azur's radius scale, in px (matches `--azur-radius-*`).
#[allow(dead_code)]
pub mod radius {
    pub const SMALL: u8 = 4;
    pub const MEDIUM: u8 = 6;
    pub const LARGE: u8 = 8;
    pub const XLARGE: u8 = 12;
}

/// Azur's 4px spacing scale (matches `--azur-space-*`).
#[allow(dead_code)]
pub mod space {
    pub const S2: f32 = 4.0;
    pub const S3: f32 = 8.0;
    pub const S4: f32 = 12.0;
    pub const S5: f32 = 16.0;
    pub const S6: f32 = 20.0;
}

/// Control heights from Azur's size scale, in px.
pub mod control {
    pub const SMALL: f32 = 28.0;
    pub const MEDIUM: f32 = 32.0;
}

pub fn dark_visuals() -> egui::Visuals {
    use palette::*;

    let mut v = egui::Visuals::dark();

    // Surfaces
    v.panel_fill = GRAY_0; // background-canvas
    v.window_fill = GRAY_3; // background-layer-alt (popovers/menus/dialogs)
    v.extreme_bg_color = GRAY_2; // background-layer (recessed surfaces, e.g. text edit bg)
    v.faint_bg_color = GRAY_1; // subtle zebra fill, one step off the canvas
    v.code_bg_color = GRAY_4;

    // Window (used for popups/menus/tooltips/floating panels)
    v.window_corner_radius = CornerRadius::same(radius::LARGE);
    v.menu_corner_radius = CornerRadius::same(radius::MEDIUM);
    v.window_stroke = Stroke::new(1.0, GRAY_4); // stroke-subtle
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.popup_shadow = v.window_shadow;

    // Selection (e.g. selected list row, text selection)
    v.selection.bg_fill = AZURE_10; // accent-subtle
    v.selection.stroke = Stroke::new(1.0, AZURE_ACCENT);

    v.hyperlink_color = AZURE_ACCENT_HOVER; // text-link
    v.warn_fg_color = AMBER_70;
    v.error_fg_color = RED_70;

    // Widget states: noninteractive / inactive / hovered / active / open.
    //
    // egui draws plain labels with `noninteractive.fg_stroke`, so that one gets
    // text-primary; text-secondary is a role the app asks for by name
    // (`Theme::text_dim`), not the default for body copy.
    v.widgets.noninteractive.bg_fill = GRAY_3;
    v.widgets.noninteractive.weak_bg_fill = GRAY_3;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, GRAY_4); // stroke-subtle
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, GRAY_16); // text-primary
    v.widgets.noninteractive.corner_radius = CornerRadius::same(radius::MEDIUM);

    v.widgets.inactive.bg_fill = GRAY_4; // background-control
    v.widgets.inactive.weak_bg_fill = GRAY_4;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, GRAY_7); // stroke-control
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, GRAY_16); // text-primary
    v.widgets.inactive.corner_radius = CornerRadius::same(radius::MEDIUM);

    v.widgets.hovered.bg_fill = GRAY_5; // background-control-hover
    v.widgets.hovered.weak_bg_fill = GRAY_5;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, GRAY_9); // stroke-strong
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, GRAY_16);
    v.widgets.hovered.corner_radius = CornerRadius::same(radius::MEDIUM);
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.bg_fill = AZURE_ACCENT; // accent-default (pressed/selected)
    v.widgets.active.weak_bg_fill = AZURE_ACCENT;
    v.widgets.active.bg_stroke = Stroke::new(1.0, AZURE_ACCENT_ACTIVE);
    v.widgets.active.fg_stroke = Stroke::new(1.0, WHITE); // text-on-accent
    v.widgets.active.corner_radius = CornerRadius::same(radius::MEDIUM);

    v.widgets.open.bg_fill = GRAY_5;
    v.widgets.open.weak_bg_fill = GRAY_5;
    v.widgets.open.bg_stroke = Stroke::new(1.0, AZURE_ACCENT); // stroke-focus
    v.widgets.open.fg_stroke = Stroke::new(1.0, GRAY_16);
    v.widgets.open.corner_radius = CornerRadius::same(radius::MEDIUM);

    v.dark_mode = true;
    v
}

pub fn light_visuals() -> egui::Visuals {
    use palette::*;

    let mut v = egui::Visuals::light();

    v.panel_fill = GRAY_16; // background-canvas
    v.window_fill = WHITE; // background-layer-alt
    v.extreme_bg_color = WHITE; // background-layer
    v.faint_bg_color = GRAY_15;
    v.code_bg_color = GRAY_15;

    v.window_corner_radius = CornerRadius::same(radius::LARGE);
    v.menu_corner_radius = CornerRadius::same(radius::MEDIUM);
    v.window_stroke = Stroke::new(1.0, GRAY_14);
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(40),
    };
    v.popup_shadow = v.window_shadow;

    v.selection.bg_fill = AZURE_100;
    v.selection.stroke = Stroke::new(1.0, AZURE_60);

    v.hyperlink_color = AZURE_60;
    v.warn_fg_color = AMBER_60;
    v.error_fg_color = RED_60;

    v.widgets.noninteractive.bg_fill = WHITE;
    v.widgets.noninteractive.weak_bg_fill = WHITE;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, GRAY_14);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, GRAY_1);
    v.widgets.noninteractive.corner_radius = CornerRadius::same(radius::MEDIUM);

    v.widgets.inactive.bg_fill = WHITE;
    v.widgets.inactive.weak_bg_fill = WHITE;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, GRAY_12);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, GRAY_1);
    v.widgets.inactive.corner_radius = CornerRadius::same(radius::MEDIUM);

    v.widgets.hovered.bg_fill = GRAY_15;
    v.widgets.hovered.weak_bg_fill = GRAY_15;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, GRAY_10);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, GRAY_1);
    v.widgets.hovered.corner_radius = CornerRadius::same(radius::MEDIUM);

    v.widgets.active.bg_fill = AZURE_60;
    v.widgets.active.weak_bg_fill = AZURE_60;
    v.widgets.active.bg_stroke = Stroke::new(1.0, AZURE_60);
    v.widgets.active.fg_stroke = Stroke::new(1.0, WHITE);
    v.widgets.active.corner_radius = CornerRadius::same(radius::MEDIUM);

    v.widgets.open.bg_fill = GRAY_15;
    v.widgets.open.weak_bg_fill = GRAY_15;
    v.widgets.open.bg_stroke = Stroke::new(1.0, AZURE_60);
    v.widgets.open.fg_stroke = Stroke::new(1.0, GRAY_1);
    v.widgets.open.corner_radius = CornerRadius::same(radius::MEDIUM);

    v.dark_mode = false;
    v
}

/// Azur's 4px spacing rhythm, applied to egui's layout spacing.
pub fn azur_spacing() -> egui::style::Spacing {
    let mut s = egui::style::Spacing::default();
    s.item_spacing = egui::vec2(space::S3, space::S3); // 8px
    s.button_padding = egui::vec2(space::S4, space::S2); // 12px x / 4px y
    s.menu_margin = egui::Margin::same(space::S3 as i8);
    s.indent = space::S5; // 16px, used by CollapsingHeader (tree-like nesting)
    // Azur's medium control height. Only the height is pinned: egui also uses
    // `interact_size.x` as a minimum width for combo boxes and drag values, and
    // zeroing it lets those collapse to their text.
    s.interact_size.y = control::MEDIUM;
    s.icon_spacing = space::S3;
    s
}

/// Apply the Azur theme to an egui context. Call once at startup, and again
/// whenever the user toggles dark/light (mirrors the gallery's theme switch).
///
/// This replaces the whole `Style` for the theme it installs, so any app-level
/// tweaks belong *after* it.
pub fn apply(ctx: &egui::Context, theme: Theme) {
    // `Style` owns its own `visuals` field, so build ONE Style with both
    // `visuals` and `spacing` set and install it in a single call — setting
    // visuals and style separately would have the second write clobber the
    // first, since `Style::default()` carries its own visuals.
    let mut style = Style::default();
    let (which, preference) = match theme {
        Theme::Dark => {
            style.visuals = dark_visuals();
            (egui::Theme::Dark, egui::ThemePreference::Dark)
        }
        Theme::Light => {
            style.visuals = light_visuals();
            (egui::Theme::Light, egui::ThemePreference::Light)
        }
    };
    style.spacing = azur_spacing();
    ctx.set_style_of(which, style);
    // Azur is not a system-following theme: the palette is the product's, so
    // pin the preference rather than letting the OS pick the other one.
    ctx.set_theme(preference);
}
