//! The product mark in the title bar: the icon artwork, drawn as a texture.
//!
//! The same plate the executable and the taskbar wear -- `assets/app.ico` and
//! `assets/app-64.png` come out of the same source image -- so the icon in the
//! caption is the icon on the file, not a second drawing of it.
//!
//! That it is artwork rather than geometry is the whole difference from
//! [`super::icons`]: a vector glyph is right at every size, and a raster one is
//! right at exactly the size it was reduced to. `TitleBar` draws an app icon in a
//! 16-*point* box, which is 16 physical pixels on a 1x display and 32 on a 2x one,
//! so three sources are embedded and the nearest one at or above what the display
//! asks for is uploaded. 16 and 32 are the two that land 1:1; the 64 is there so a
//! 250 %-scaled display reduces a larger image rather than magnifying a smaller one.

/// The sources, smallest first. The 64 is the file `main` also hands to
/// `ViewportBuilder::with_icon` -- one image, two consumers.
const SOURCES: [(u32, &[u8]); 3] = [
    (16, include_bytes!("../../assets/app-16.png")),
    (32, include_bytes!("../../assets/app-32.png")),
    (64, include_bytes!("../../assets/app-64.png")),
];

/// The box `TitleBar` gives the app icon, in points.
const BOX_PT: f32 = 16.0;

/// The mark's texture, uploaded when the size it should be drawn at changes.
#[derive(Default)]
pub struct AppMark {
    /// Which source is on the GPU. Holding the size beside the handle is what makes
    /// a steady display scale upload nothing after the first frame, and a window
    /// dragged onto a differently scaled monitor swap textures rather than keep the
    /// wrong one.
    tex: Option<(u32, egui::TextureHandle)>,
}

impl AppMark {
    /// The mark as an [`azur_egui_theme::icons::Icon`], ready for `TitleBar::icon`.
    ///
    /// The tint an `Icon` is handed is ignored: a glyph takes its colour from the
    /// component it sits in, and this is a picture that carries its own.
    pub fn icon(
        &mut self,
        ctx: &egui::Context,
    ) -> impl Fn(&egui::Painter, egui::Rect, egui::Color32) {
        let tex = self.texture(ctx);
        move |painter: &egui::Painter, rect: egui::Rect, _: egui::Color32| {
            painter.image(
                tex,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }

    fn texture(&mut self, ctx: &egui::Context) -> egui::TextureId {
        let want = (BOX_PT * ctx.pixels_per_point()).round() as u32;
        let (size, png) = SOURCES
            .iter()
            .find(|(size, _)| *size >= want)
            .copied()
            // Past 4x there is nothing larger to reach for; the biggest is magnified.
            .unwrap_or(SOURCES[SOURCES.len() - 1]);

        if self.tex.as_ref().map(|(s, _)| *s) != Some(size) {
            // `expect` on an asset compiled into the binary, as in `main`: it can
            // only fail if the PNG in the repository is corrupt, which is a build
            // fault rather than something to paint around.
            let src = eframe::icon_data::from_png_bytes(png)
                .expect("an assets/app-*.png is not a PNG");
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [src.width as usize, src.height as usize],
                &src.rgba,
            );
            let tex = ctx.load_texture(
                format!("app-mark:{size}"),
                image,
                egui::TextureOptions::LINEAR,
            );
            #[cfg(feature = "memtrace")]
            crate::memtrace::note_texture_upload();
            self.tex = Some((size, tex));
        }

        self.tex.as_ref().expect("just set").1.id()
    }
}
