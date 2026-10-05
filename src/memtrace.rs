//! Allocation accounting for leak hunting. Compiled in only with
//! `--features memtrace`.
//!
//! Two relaxed atomics on every allocation is not something a latency-focused
//! app should carry by default, hence the compile-time gate rather than a
//! runtime flag.
//!
//! The number this exists to produce is *live heap bytes*, which separates two
//! very different problems that look identical from outside the process: if live
//! bytes track the growth in private bytes, something in this codebase is
//! holding memory; if live bytes are flat while private bytes climb, the growth
//! is inside a driver or a Win32 subsystem and no amount of reading our
//! containers will find it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};

static LIVE: AtomicI64 = AtomicI64::new(0);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

pub struct Counting;

// SAFETY: every method forwards to `System`, which upholds the `GlobalAlloc`
// contract; the counters are side effects that do not touch the returned block.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            LIVE.fetch_add(l.size() as i64, Relaxed);
            ALLOCS.fetch_add(1, Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        LIVE.fetch_sub(l.size() as i64, Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            LIVE.fetch_add(new as i64 - l.size() as i64, Relaxed);
            ALLOCS.fetch_add(1, Relaxed);
        }
        q
    }
}

/// Live heap bytes, and total allocations since start.
pub fn live() -> (i64, u64) {
    (LIVE.load(Relaxed), ALLOCS.load(Relaxed))
}

// --- event counters ---------------------------------------------------------
//
// Growth per *second* is ambiguous on its own: the same rate means a large
// per-frame leak at 1 fps or a small one at 60 fps, and the fixes are nowhere
// near each other. These make the denominator visible.

static FRAMES: AtomicU64 = AtomicU64::new(0);
static TEXTURES: AtomicU64 = AtomicU64::new(0);

pub fn note_frame() {
    FRAMES.fetch_add(1, Relaxed);
}

pub fn note_texture_upload() {
    TEXTURES.fetch_add(1, Relaxed);
}

/// Frames painted and textures uploaded since start.
pub fn counters() -> (u64, u64) {
    (FRAMES.load(Relaxed), TEXTURES.load(Relaxed))
}

// --- paint-path bisect ------------------------------------------------------

/// How much of the UI to paint, from `AZTM_UI_LEVEL`: 0 nothing, 1 nav rail,
/// 2 also the toolbar and status bar, 3 (default) everything. Halving the
/// painted surface and re-measuring is the only way to attribute growth that
/// happens inside a driver, where there is no allocation to instrument.
pub fn ui_level() -> u32 {
    static LEVEL: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("AZTM_UI_LEVEL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3)
    })
}

/// `AZTM_NO_ICONS=1` points every row at one shared texture, so the table stops
/// forcing a draw-call break per row.
pub fn no_icons() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("AZTM_NO_ICONS").is_some())
}
