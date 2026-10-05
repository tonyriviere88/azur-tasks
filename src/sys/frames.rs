//! Frame pacing for the foreground application, from an ETW trace.
//!
//! One present event per frame, timestamped on the QPC clock, gives the interval
//! between frames — and from a window of intervals, everything a reader wants to know
//! about smoothness. See [`super::etw`] for the session itself, and for why all of
//! this needs administrator rights.
//!
//! ## Which event is a present
//!
//! Two providers publish a present: `Microsoft-Windows-DXGI` sees the application's
//! `Present` call, and `Microsoft-Windows-DxgKrnl` sees the graphics kernel carry it
//! out. Their event ids are in no header — they come from the providers' manifests —
//! so the two constants below started out as the numbers every tool that does this
//! uses, written to survive being wrong about them.
//!
//! They have since been measured. [`super::etw::tests::probes_the_frame_providers`],
//! run elevated against a 144 Hz display, resolved every kind the two providers emit
//! through TDH: of 82 of them, DXGI 42 is `Present/Start` and DxgKrnl 184 is
//! `Present/Info`, and both reported the same process at 141.6 events a second. Which
//! is the answer, on this Windows build and this hardware.
//!
//! The redundancy stays anyway, because a manifest belongs to a Windows build and not
//! to us: both sources are counted, per process and per source, and whichever produced
//! more frames for a given process is the one reported. An id that changes meaning
//! costs the *other* source's numbers, not all of them.

use super::etw::{self, provider, Event, Session};

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// `Microsoft-Windows-DXGI`, `Present`/`Start`: the application's own call.
///
/// Id 43 is the matching `Present`/`Stop` at the same rate. Counting one of the pair is
/// the point — counting both would double every frame rate on screen.
const DXGI_PRESENT_START: u16 = 42;
/// `Microsoft-Windows-DxgKrnl`, `Present`/`Info`: the kernel carrying one out.
const DXGKRNL_PRESENT: u16 = 184;

/// How many recent presents to keep per process.
///
/// 512 is four seconds at 120 Hz, which is enough for a stable 99th percentile and
/// short enough that the numbers still follow what is happening now.
const RING: usize = 512;

/// The window the statistics are computed over. Long enough that a percentile means
/// something, short enough to feel live.
const WINDOW_SECS: f64 = 2.0;

/// Which provider a process's frames were counted from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Dxgi,
    DxgKrnl,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dxgi => "DXGI",
            Self::DxgKrnl => "DxgKrnl",
        }
    }
}

/// What a window of frame intervals says about smoothness.
#[derive(Clone, Copy, Debug)]
pub struct FrameStats {
    pub source: Source,
    /// Frames per second over the window.
    pub fps: f32,
    /// Mean interval between frames, milliseconds.
    pub avg_ms: f32,
    /// The rate the slowest one frame in a hundred arrives at — "1% low", the figure
    /// that says whether a nominally smooth average is actually smooth.
    pub low1_fps: f32,
    /// Share of frames whose interval differs from the one before it by more than a
    /// fifth of the window's median, as a percentage.
    ///
    /// Micro-stutter has no standard definition, so this states its own: judder is a
    /// frame arriving at a visibly different moment from its neighbours, and "visibly"
    /// has to scale with the frame rate — 2 ms is nothing at 30 Hz and a third of a
    /// frame at 144 Hz. Hence a proportion of the median rather than a fixed
    /// milliseconds.
    pub stutter_pct: f32,
}

/// Why there are no frame numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Tracing.
    Tracing,
    /// Not elevated. The one case a reader can do something about.
    NeedsElevation,
    /// The session could not be started for some other reason.
    Failed(u32),
}

impl Status {
    /// The line the Sensors tab shows under the section.
    pub fn note(self) -> String {
        match self {
            Self::Tracing => "Frame timing from an ETW trace of the graphics providers.".into(),
            Self::NeedsElevation => "Frame timing comes from an ETW trace session, and Windows \
                 grants those to elevated processes only. Restart as administrator to see FPS, \
                 frame time and stutter for whatever is in the foreground."
                .into(),
            Self::Failed(e) => format!(
                "The graphics trace session could not be started (Windows error {e}), so there \
                 are no frame numbers."
            ),
        }
    }
}

/// Recent present timestamps for one process, from one provider.
struct Ring {
    /// QPC ticks, oldest first, at most [`RING`] of them.
    at: std::collections::VecDeque<i64>,
    /// When this ring was last written to, for evicting processes that have stopped.
    last: i64,
}

#[derive(Default)]
struct Table {
    rings: HashMap<(u32, u8), Ring>,
}

impl Table {
    fn push(&mut self, pid: u32, source: u8, at: i64) {
        let ring = self.rings.entry((pid, source)).or_insert_with(|| Ring {
            at: std::collections::VecDeque::with_capacity(RING),
            last: at,
        });
        if ring.at.len() == RING {
            ring.at.pop_front();
        }
        ring.at.push_back(at);
        ring.last = at;

        // A machine can present from dozens of processes at once and they come and go.
        // Bounded by eviction rather than by a cap so that the *interesting* process is
        // never the one dropped: anything silent for a while is gone anyway.
        if self.rings.len() > 64 {
            let cutoff = at - 30 * etw::qpc_frequency();
            self.rings.retain(|_, r| r.last > cutoff);
        }
    }
}

/// Owns the trace session and the thread consuming it.
pub struct FrameMonitor {
    table: Arc<Mutex<Table>>,
    status: Status,
    /// Dropping this stops the session, which is what ends the consumer thread.
    session: Option<Session>,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FrameMonitor {
    /// Start tracing, or record why not.
    ///
    /// Cheap and non-fatal when it fails, which on any unelevated run it will.
    pub fn start() -> Self {
        let table: Arc<Mutex<Table>> = Arc::new(Mutex::new(Table::default()));
        let stopping = Arc::new(AtomicBool::new(false));

        let session = match Session::start("aztm-frames") {
            Ok(s) => s,
            // `ERROR_ACCESS_DENIED`. The expected outcome, not an error worth noise.
            Err(5) => {
                return Self {
                    table,
                    status: Status::NeedsElevation,
                    session: None,
                    stopping,
                    thread: None,
                }
            }
            Err(e) => {
                return Self {
                    table,
                    status: Status::Failed(e),
                    session: None,
                    stopping,
                    thread: None,
                }
            }
        };

        // Level 4 is "informational": present events are informational, and asking for
        // verbose would add pages of detail nothing here reads.
        let _ = session.enable(&provider::DXGI, 4, provider::DXGI_ALL);
        let _ = session.enable(&provider::DXGKRNL, 4, provider::DXGKRNL_BASE);

        let thread = {
            let table = Arc::clone(&table);
            let stopping = Arc::clone(&stopping);
            let name = session.name().to_vec();
            std::thread::Builder::new()
                .name("aztm-frames".into())
                .spawn(move || {
                    let mut sink = |e: &Event, _raw: *const _| {
                        if stopping.load(Ordering::Relaxed) {
                            return;
                        }
                        let source = match (e.id, etw::same_guid(&e.provider, &provider::DXGI)) {
                            (DXGI_PRESENT_START, true) => 0,
                            (DXGKRNL_PRESENT, false) => 1,
                            _ => return,
                        };
                        if let Ok(mut t) = table.lock() {
                            t.push(e.pid, source, e.timestamp);
                        }
                    };
                    let _ = etw::consume(&name, &mut sink);
                })
                .ok()
        };

        Self {
            table,
            status: Status::Tracing,
            session: Some(session),
            stopping,
            thread,
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    /// Frame statistics for one process, if it has presented enough to have any.
    ///
    /// Whichever provider saw more of its frames wins: see the module note on which
    /// event is a present.
    pub fn stats(&self, pid: u32) -> Option<FrameStats> {
        let table = self.table.lock().ok()?;
        let freq = etw::qpc_frequency() as f64;
        [(0u8, Source::Dxgi), (1u8, Source::DxgKrnl)]
            .into_iter()
            .filter_map(|(key, source)| {
                let ring = table.rings.get(&(pid, key))?;
                stats_of(&ring.at, freq, source)
            })
            .max_by_key(|s| s.fps.max(0.0) as u32)
    }
}

impl Drop for FrameMonitor {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        // Stopping the session is what makes `ProcessTrace` return, which is what lets
        // the thread finish. Order matters: joining first would wait forever.
        self.session = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Reduce a ring of present timestamps to the four numbers on screen.
///
/// Split out from [`FrameMonitor`] so it can be tested against made-up traces: this
/// is where a percentile or a stutter rule can be quietly wrong, and it is the part
/// that does not need a graphics card to exercise.
fn stats_of(at: &std::collections::VecDeque<i64>, freq: f64, source: Source) -> Option<FrameStats> {
    let newest = *at.back()?;
    let cutoff = newest - (WINDOW_SECS * freq) as i64;
    // Intervals that *began* inside the window, in milliseconds.
    //
    // The start and not the end: a process that rendered, idled for a minute and then
    // rendered again has one interval a minute long, and that interval ends inside the
    // window. Counting it puts the whole idle period into the average and reports one
    // frame a second for an app running at a hundred.
    let mut gaps: Vec<f32> = at
        .iter()
        .zip(at.iter().skip(1))
        .filter(|(&a, _)| a > cutoff)
        .map(|(&a, &b)| ((b - a) as f64 / freq * 1000.0) as f32)
        .collect();
    // Two intervals is not a frame rate. Anything less and the answer would swing
    // wildly between ticks, which reads as a broken gauge rather than a quiet app.
    if gaps.len() < 3 {
        return None;
    }

    let total: f32 = gaps.iter().sum();
    let avg_ms = total / gaps.len() as f32;
    let fps = 1000.0 / avg_ms;

    // Sorted for the percentile, which also gives the median the stutter rule needs.
    // The order is not wanted afterwards, so the deltas are taken first.
    let jumps = gaps
        .iter()
        .zip(gaps.iter().skip(1))
        .map(|(a, b)| (b - a).abs())
        .collect::<Vec<f32>>();
    gaps.sort_by(f32::total_cmp);
    let median = gaps[gaps.len() / 2];
    // The 99th percentile *interval* is the slowest one frame in a hundred, and its
    // reciprocal is what everyone calls the 1% low.
    let p99 = gaps[((gaps.len() as f32 * 0.99).ceil() as usize).min(gaps.len()) - 1];
    let low1_fps = if p99 > 0.0 { 1000.0 / p99 } else { 0.0 };

    let threshold = (median * 0.2).max(0.1);
    let stutter = jumps.iter().filter(|d| **d > threshold).count();
    let stutter_pct = if jumps.is_empty() {
        0.0
    } else {
        stutter as f32 / jumps.len() as f32 * 100.0
    };

    Some(FrameStats {
        source,
        fps,
        avg_ms,
        low1_fps,
        stutter_pct,
    })
}

/// The process owning the foreground window, or zero.
pub fn foreground_pid() -> u32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    let mut pid = 0u32;
    let h = unsafe { GetForegroundWindow() };
    if !h.is_null() {
        unsafe { GetWindowThreadProcessId(h, &mut pid) };
    }
    pid
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A trace at a fixed rate, with `jitter_ms` added to every other frame.
    fn trace(freq: f64, count: usize, ms: f64, jitter_ms: f64) -> VecDeque<i64> {
        let mut at = VecDeque::new();
        let mut t = 0.0f64;
        for i in 0..count {
            at.push_back((t / 1000.0 * freq) as i64);
            t += ms + if i % 2 == 0 { jitter_ms } else { 0.0 };
        }
        at
    }

    /// A perfectly even 60 Hz trace reads as 60 fps, 16.67 ms, and no stutter.
    #[test]
    fn reads_an_even_trace_as_its_own_rate() {
        let freq = 10_000_000.0;
        let s = stats_of(&trace(freq, 120, 1000.0 / 60.0, 0.0), freq, Source::Dxgi).unwrap();
        assert!((s.fps - 60.0).abs() < 0.5, "fps {}", s.fps);
        assert!((s.avg_ms - 16.667).abs() < 0.05, "avg {}", s.avg_ms);
        assert!((s.low1_fps - 60.0).abs() < 1.0, "1% low {}", s.low1_fps);
        assert!(s.stutter_pct < 1.0, "stutter {}", s.stutter_pct);
    }

    /// Alternating frame times are the definition of judder: the average can look
    /// perfect while every frame lands at the wrong moment, which is exactly why the
    /// average is not the only number on screen.
    #[test]
    fn calls_alternating_intervals_stutter() {
        let freq = 10_000_000.0;
        // 8 ms, 16 ms, 8, 16 ...: a 12 ms mean, so ~83 fps average and awful pacing.
        let s = stats_of(&trace(freq, 200, 8.0, 8.0), freq, Source::Dxgi).unwrap();
        assert!((s.fps - 83.3).abs() < 2.0, "fps {}", s.fps);
        assert!(s.stutter_pct > 90.0, "stutter {}", s.stutter_pct);
        // And the 1% low is the slow half, not the mean.
        assert!(s.low1_fps < 70.0, "1% low {}", s.low1_fps);
    }

    /// Occasional stalls in an otherwise smooth run barely move the average, and are
    /// supposed to show up in the 1% low. That gap is the point of reporting both.
    ///
    /// "Slowest one frame in a hundred" means what it says, so the trace has to hold
    /// more than one stall in a hundred frames for the 99th percentile to be one. A
    /// *single* hitch among two hundred frames sits at the 99.5th percentile and is
    /// correctly excluded — the metric that catches those is the stutter rate.
    #[test]
    fn shows_repeated_hitches_in_the_1_percent_low() {
        let freq = 10_000_000.0;
        let mut at = trace(freq, 150, 1000.0 / 144.0, 0.0);
        // Three 50 ms stalls among 150 frames: 2%, comfortably past the percentile.
        for _ in 0..3 {
            let last = *at.back().unwrap();
            at.push_back(last + (0.050 * freq) as i64);
            for _ in 0..10 {
                let last = *at.back().unwrap();
                at.push_back(last + (1000.0 / 144.0 / 1000.0 * freq) as i64);
            }
        }
        let s = stats_of(&at, freq, Source::Dxgi).unwrap();
        assert!(s.fps > 90.0, "the average hardly notices: {}", s.fps);
        assert!(s.low1_fps < 40.0, "but the 1% low does: {}", s.low1_fps);
    }

    /// A lone hitch is a 0.5% event, so the percentile leaves it out and the stutter
    /// rate is what reports it. Pinned because it is a limit worth not forgetting.
    #[test]
    fn leaves_a_lone_hitch_to_the_stutter_rate() {
        let freq = 10_000_000.0;
        let mut at = trace(freq, 200, 1000.0 / 144.0, 0.0);
        let last = *at.back().unwrap();
        at.push_back(last + (0.050 * freq) as i64);
        let s = stats_of(&at, freq, Source::Dxgi).unwrap();
        assert!(
            s.low1_fps > 100.0,
            "one in two hundred is not one in a hundred"
        );
        assert!(s.stutter_pct > 0.0, "but the pacing was disturbed");
    }

    /// Too few frames is not a slow frame rate; it is no answer.
    #[test]
    fn declines_to_guess_from_two_frames() {
        let freq = 10_000_000.0;
        assert!(stats_of(&trace(freq, 2, 16.0, 0.0), freq, Source::Dxgi).is_none());
        assert!(stats_of(&VecDeque::new(), freq, Source::Dxgi).is_none());
    }

    /// Only the window is considered, so a process that idled for a minute and then
    /// rendered reports what it is doing now rather than an average over the pause.
    #[test]
    fn ignores_what_happened_before_the_window() {
        let freq = 10_000_000.0;
        let mut at = VecDeque::new();
        at.push_back(0);
        // A minute of nothing, then a smooth 100 Hz.
        let start = (60.0 * freq) as i64;
        for i in 0..100 {
            at.push_back(start + (i as f64 * 0.010 * freq) as i64);
        }
        let s = stats_of(&at, freq, Source::Dxgi).unwrap();
        assert!((s.fps - 100.0).abs() < 1.0, "fps {}", s.fps);
    }
}
