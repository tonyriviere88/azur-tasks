// Detach from the console so launching from Explorer does not flash a window.
// A debug build keeps the console so panics and logging stay visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
#[cfg(feature = "memtrace")]
mod memtrace;
mod settings;
mod sys;
mod ui;

#[cfg(feature = "memtrace")]
#[global_allocator]
static ALLOC: memtrace::Counting = memtrace::Counting;

/// The window icon, unpacked from `assets/yatm.ico` by `build.rs`.
///
/// The `.ico` embedded in the executable is what Explorer and the taskbar read,
/// but winit registers its window class with a null `hIcon`, so the caption and
/// Alt-Tab icons have to be set from pixels at runtime. winit turns these into a
/// single `HICON` at the source resolution and lets Windows scale it, so this is
/// deliberately the largest uncompressed entry: 64px halves exactly to the 32px
/// the taskbar wants and quarters exactly to the 16px in the title bar.
mod icon {
    include!(concat!(env!("OUT_DIR"), "/icon.rs"));
}

/// Restrict wgpu to a single backend and the integrated adapter, and ask its
/// allocator to favour memory over throughput.
///
/// By default wgpu enumerates every backend it was built with, which on Windows
/// means loading the DX12, Vulkan *and* OpenGL driver stacks just to pick one.
/// Each of those maps a substantial amount of driver memory into the process.
/// A task manager draws flat rectangles and text, so it asks for the least
/// capable path that works: DX12 only, on the low-power adapter, which also
/// keeps a discrete GPU from spinning up just to host this window.
fn wgpu_config() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::wgpu;

    let mut cfg = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut cfg.wgpu_setup {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        setup.power_preference = wgpu::PowerPreference::LowPower;

        // wgpu's default allocator sub-allocates in large blocks to keep
        // allocation off the hot path. This UI uploads a few hundred kilobytes
        // of vertices a second, so the throughput that buys is irrelevant and
        // the blocks are pure footprint.
        setup.device_descriptor = std::sync::Arc::new(|adapter: &wgpu::Adapter| {
            let base = if adapter.get_info().backend == wgpu::Backend::Gl {
                wgpu::Limits::downlevel_webgl2_defaults()
            } else {
                wgpu::Limits::default()
            };
            wgpu::DeviceDescriptor {
                label: Some("yatm"),
                required_limits: wgpu::Limits {
                    max_texture_dimension_2d: 8192,
                    ..base
                },
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            }
        });
    }
    cfg
}

fn main() -> eframe::Result<()> {
    // Default to wgpu/DX12, despite OpenGL starting ~180 MB lighter.
    //
    // OpenGL starts smaller and then grows without bound: the driver keeps a
    // buffer allocation per distinct `glBufferData` size, egui_glow uploads one
    // buffer per mesh per frame, and a table of live numbers changes the size of
    // every mesh every frame. Measured at ~210 KB per frame -- around 1 GB after
    // half an hour with the window merely open. A bare eframe window with a
    // *fixed* vertex count stays flat, and starts growing the moment the count
    // is made to vary, which is what pins it on the size rather than on the
    // volume or on anything in this codebase.
    //
    // wgpu is flat. For a program left open all day that beats a smaller start,
    // so this trades ~150 MB of steady footprint for a leak. `YATM_RENDERER=glow`
    // picks OpenGL for a short session or a machine where DX12 misbehaves.
    let use_wgpu = std::env::var("YATM_RENDERER").as_deref() != Ok("glow");

    let settings = settings::Settings::load();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1320.0, 840.0])
        // Small enough to park beside something else. The toolbar sheds its
        // caption and the rail collapses to icons, so this stays usable rather
        // than merely permitted.
        .with_min_inner_size([500.0, 400.0])
        .with_title("Yet Another Task Manager")
        .with_app_id("yatm")
        .with_icon(egui::IconData {
            rgba: icon::RGBA.to_vec(),
            width: icon::SIZE,
            height: icon::SIZE,
        });

    // Restore where the window was. `window_rect` returns `None` for anything
    // implausible -- a size below the minimum, or a position from a monitor that
    // is no longer attached -- so a stale file cannot open the window off-screen.
    let restored = settings.window_rect();
    if let Some([x, y, w, h]) = restored {
        viewport = viewport
            .with_position([x, y])
            .with_inner_size([w, h]);
    }
    if settings.bool("window.maximized", false) {
        viewport = viewport.with_maximized(true);
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: if use_wgpu {
            eframe::Renderer::Wgpu
        } else {
            eframe::Renderer::Glow
        },
        wgpu_options: wgpu_config(),
        // The table draws thin 1px rules and small text; multisampling would
        // only blur them, and dithering is for gradients we do not have.
        multisampling: 0,
        dithering: false,
        // Centring would override the restored position on the first frame.
        centered: restored.is_none(),
        ..Default::default()
    };

    eframe::run_native(
        "Yet Another Task Manager",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, settings)))),
    )
}
