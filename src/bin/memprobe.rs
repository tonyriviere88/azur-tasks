//! Isolation probe: a bare eframe window with a single label.
//!
//! Used to separate the framework's baseline memory cost -- and, with
//! `YATM_PROBE_SPIN=1`, its per-frame memory *growth* -- from this app's own.
//! Renderer is chosen with `YATM_PROBE_RENDERER=wgpu|glow`.
//!
//! Spinning matters because the leak being chased is per frame, not per second:
//! comparing a repainting probe against the app gives bytes-per-frame for each,
//! and that says whether the growth belongs to the framework or to us.

fn main() -> eframe::Result<()> {
    let use_glow = std::env::var("YATM_PROBE_RENDERER").as_deref() == Ok("glow");

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1320.0, 840.0])
            .with_title("memprobe"),
        renderer: if use_glow {
            eframe::Renderer::Glow
        } else {
            eframe::Renderer::Wgpu
        },
        multisampling: 0,
        dithering: false,
        ..Default::default()
    };

    if !use_glow && std::env::var_os("YATM_PROBE_ALL_BACKENDS").is_none() {
        use eframe::wgpu;
        if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup
        {
            setup.instance_descriptor.backends = wgpu::Backends::DX12;
            setup.power_preference = wgpu::PowerPreference::LowPower;
        }
    }

    let spin = std::env::var_os("YATM_PROBE_SPIN").is_some();
    // Number of text cells to paint per frame. A real table paints a few
    // hundred, and text is what dominates the vertex data egui uploads, so this
    // is the knob that decides whether growth follows submitted work.
    let cells: usize = std::env::var("YATM_PROBE_CELLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    // `YATM_PROBE_JITTER=1` varies the cell count every frame, so the vertex
    // buffer egui hands the driver is a different size each time. A fixed count
    // hands it the same size over and over, which is the case that stayed flat.
    let jitter = std::env::var_os("YATM_PROBE_JITTER").is_some();

    eframe::run_native(
        "memprobe",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(Probe {
                spin,
                cells,
                jitter,
                frames: 0,
                started: std::time::Instant::now(),
                reported: 0,
            }))
        }),
    )
}

struct Probe {
    spin: bool,
    cells: usize,
    jitter: bool,
    frames: u64,
    started: std::time::Instant,
    reported: u64,
}

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, _f: &mut eframe::Frame) {
        self.frames += 1;
        let secs = self.started.elapsed().as_secs();
        if secs != self.reported {
            self.reported = secs;
            println!("[probe] t={secs}s frames={}", self.frames);
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.label("baseline");

            // Paint the cells the same way the real table does -- straight to
            // the painter, with text that changes every frame so nothing can be
            // served from a cache.
            if self.cells > 0 {
                let painter = ui.painter().clone();
                let font = egui::FontId::proportional(13.0);
                let rect = ui.max_rect();
                let cols = 12;
                let n = self.frames;
                // A cheap deterministic wobble; no RNG needed, and reproducible.
                let count = if self.jitter {
                    let phase = (n % 97) as f32 / 97.0;
                    self.cells / 2 + (self.cells as f32 * 0.5 * phase) as usize
                } else {
                    self.cells
                };
                for i in 0..count {
                    let (cx, cy) = (i % cols, i / cols);
                    let pos = rect.min
                        + egui::vec2(8.0 + cx as f32 * 105.0, 24.0 + cy as f32 * 18.0);
                    painter.text(
                        pos,
                        egui::Align2::LEFT_TOP,
                        format!("{}.{:03}", n.wrapping_add(i as u64), i),
                        font.clone(),
                        egui::Color32::from_gray(200),
                    );
                }
            }
        });

        if self.spin {
            ui.ctx().request_repaint();
        }
    }
}
