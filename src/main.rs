// A `windows` subsystem binary in every profile: no console, ever. This used to ask
// `build.rs` which profile was being built and take the console only for a release one, on
// the reasoning that a dev build wants somewhere for panics and the diagnostics to go -- and
// the price was a black console window opening behind the application on every dev launch,
// including every `cargo claude-run`. `sys::util::attach_parent_console` is what keeps the
// other half working: it borrows the console of whatever launched this, so `AZT_TRACE_SAMPLE`,
// the `memtrace` figures and a panic message all still reach a shell without one ever being
// *created* for a launch from Explorer, a pinned shortcut or the Startup folder.
//
// `not(test)` and not an unconditional attribute, because the test harness is this same
// crate root: a `windows` subsystem test binary has no standard handles, and std reports "no
// stdio handle" as a *successful* write of every byte -- so the machine reports the probes in
// `sys` print (`pdh`, `etw`, `sensors`, `actions`) would come out empty and still exit 0.
// The one thing worse than a silent program is a silent test run.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
#[cfg(feature = "memtrace")]
mod memtrace;
mod settings;
mod sys;
mod ui;

#[cfg(feature = "memtrace")]
#[global_allocator]
static ALLOC: memtrace::Counting = memtrace::Counting;

/// The window icon: this application's mark, from `assets/app-source.png`.
///
/// The `.ico` in the executable's resources is what Explorer and a pinned shortcut
/// read, but winit registers its window class with a null `hIcon`, so the taskbar
/// button and Alt-Tab entry of a *running* window have to be handed pixels. Miss
/// this and they switch to egui's white `e` the moment the window opens.
///
/// 64px of the nine sizes in the `.ico`: winit makes one `HICON` at the source
/// resolution and lets Windows scale it, and 64 halves exactly to the 32 the taskbar
/// asks for and quarters exactly to the 16 in the caption. Both of those steps are a
/// clean box reduction of an image with fine detail in it — the trace across the
/// window, and the gear's ring — so an exact factor is worth more here than a larger
/// source would be. `ui::app_mark` reads the same file for the title bar.
/// The name a person sees: window title, title bar, taskbar, `%APPDATA%` folder.
/// `build.rs` cannot import it and repeats it for the `.exe`'s version resource.
pub const PRODUCT_NAME: &str = "Azur Tasks";

/// The window's size on a first run, and what `Reset window size` puts it back to.
pub const WINDOW_SIZE: [f32; 2] = [800.0, 850.0];

const WINDOW_ICON: &[u8] = include_bytes!("../assets/app-64.png");

/// The graphics configuration, which this program no longer decides.
///
/// All of it — the single backend, the adapter, and the allocator asked to favour
/// memory over throughput — is [`azur_egui_theme::render`]'s, because all three
/// decide whether a rule one pixel thick arrives one pixel thick, and this window
/// draws one under its columns. Restricting wgpu to a single backend still matters
/// for the reason it always did: left to itself it loads the DX12, Vulkan *and*
/// OpenGL driver stacks just to pick one, and each maps a substantial amount of
/// driver memory into the process.
///
/// What this program used to set, and what measuring it said:
///
/// | | private bytes | working set | threads |
/// | --- | --- | --- | --- |
/// | DX12, integrated adapter | 292.1 MB | 263.4 MB | 24 |
/// | Vulkan, integrated adapter | 187.3 MB | 220.2 MB | 19 |
/// | **Vulkan, discrete adapter** | **319.4 MB** | **304.9 MB** | **34** |
///
/// DX12 was pinned here as the cheapest path for flat rectangles and was not even
/// that — Vulkan on the same adapter is 105 MB lighter. The integrated adapter is
/// lighter again, and is *not* what this now uses: rendering there while the display
/// hangs off the discrete GPU sends every frame across adapters to be presented, and
/// whether that is lossless is a property of how a machine is wired rather than
/// something to assume. The library documents both, and `AZUR_ADAPTER=low` takes the
/// 131 MB back on a machine where it has been checked.
///
/// Checked rather than assumed, on either adapter: the 1px rules under the column
/// headers keep their whole contrast in one row, measured at a leak of 0.000.
fn wgpu_config() -> eframe::egui_wgpu::WgpuConfiguration {
    azur_egui_theme::render::wgpu_options()
}

fn main() -> eframe::Result<()> {
    // Before anything that can print, and before the sampler thread that `AZT_TRACE_SAMPLE`
    // makes print. This is a `windows` subsystem binary, so stdout and stderr reach a shell
    // only if there is one to borrow, and silently swallow every byte if there is not. See
    // the attribute at the top of this file.
    sys::util::attach_parent_console();

    // wgpu is the only renderer, and there is no environment variable for the other
    // one because the other one is no longer compiled in. See `wgpu_config` for which
    // backend, and why that part is not decided here; `AZUR_BACKEND` changes backend
    // without leaving wgpu.
    //
    // OpenGL was here, and won on the first measurement only: a bare eframe window
    // starts ~180 MB lighter under glow and then grows about 210 KB per painted frame,
    // indefinitely -- around 1 GB after half an hour with the window merely open. The
    // driver keeps an allocation per distinct `glBufferData` size, `egui_glow` uploads
    // one buffer per mesh per frame, and a table of live numbers changes the size of
    // nearly every mesh on every frame. A probe with a *fixed* vertex count stays
    // flat and starts growing the moment the count is made to vary, which is what
    // pins it on the size rather than on the volume or on anything here.
    //
    // So the trade was ~150 MB of steady footprint against a leak, and for a program
    // left open all day it was not close. Keeping the loser as an escape hatch cost
    // 205,824 bytes of every copy of this executable; the README carries the
    // measurements, which is what they were for.

    let settings = settings::Settings::load();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(WINDOW_SIZE)
        // Small enough to park beside something else. The toolbar sheds its
        // caption and the rail collapses to icons, so this stays usable rather
        // than merely permitted.
        .with_min_inner_size([500.0, 400.0])
        .with_title(PRODUCT_NAME)
        .with_app_id("azt")
        // The window draws Azur's own `TitleBar`, so it asks the platform not to
        // draw a second one. The frame keeps its shadow and its resize borders;
        // only the caption is ours.
        .with_decorations(false)
        // `expect` on an asset compiled into the binary: it can only fail if the
        // PNG in the repository is corrupt or `image` lost its `png` feature, and
        // both are build faults that should be loud rather than a window that
        // quietly wears egui's icon instead of this application's.
        .with_icon(
            eframe::icon_data::from_png_bytes(WINDOW_ICON).expect("assets/app-64.png is not a PNG"),
        );

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
        renderer: eframe::Renderer::Wgpu,
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
        PRODUCT_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, settings)))),
    )
}
