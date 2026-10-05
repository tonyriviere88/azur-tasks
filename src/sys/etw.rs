//! A real-time ETW session, and a consumer for it.
//!
//! Event Tracing for Windows is the only way to see another process's *frames*.
//! Presentation happens in the graphics kernel, which publishes an event per
//! present and counts nothing that a performance counter can be read for — the
//! counter sets carry engine time and memory, never frames.
//!
//! ## This needs administrator rights
//!
//! Starting a session against a system provider requires `SeSystemProfilePrivilege`,
//! which a standard user does not hold. There is no way around it and no partial
//! version of it: [`Session::start`] fails with `ERROR_ACCESS_DENIED` and everything
//! built on top reports nothing. That is why the frame section says so in as many
//! words rather than showing zeros.
//!
//! ## Timestamps
//!
//! The session is started with `ClientContext = 1`, which asks for QPC timestamps —
//! the same clock `QueryPerformanceCounter` reads, so a frame interval is the
//! difference between two event timestamps divided by the QPC frequency. The other
//! two choices (system time, CPU cycles) are respectively too coarse and not a clock.

use super::util::wide;

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_SUCCESS, ERROR_WMI_INSTANCE_NOT_FOUND, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW,
    TdhGetEventInformation, CONTROLTRACE_HANDLE, EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD,
    EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES,
    EVENT_TRACE_REAL_TIME_MODE, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME,
    TRACE_EVENT_INFO, WNODE_FLAG_TRACED_GUID,
};

/// One event, reduced to what frame timing needs.
///
/// All of it comes from the record's *header*. The payload is never parsed, which is
/// what makes this robust across Windows versions: field layouts move between builds
/// and the header does not.
#[derive(Clone, Copy)]
pub struct Event {
    pub provider: GUID,
    pub id: u16,
    pub pid: u32,
    /// QPC ticks. Divide differences by [`qpc_frequency`].
    pub timestamp: i64,
}

/// The QPC frequency, for turning tick differences into seconds.
pub fn qpc_frequency() -> i64 {
    use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;
    let mut f = 0i64;
    // Fixed at boot and never zero on any machine this app can run on; 1 keeps a
    // division safe if that ever stops being true.
    if unsafe { QueryPerformanceFrequency(&mut f) } == 0 || f == 0 {
        return 1;
    }
    f
}

/// A running trace session. Stops itself.
pub struct Session {
    handle: CONTROLTRACE_HANDLE,
    name: Vec<u16>,
}

// The handle is an opaque kernel value, used from whichever thread owns this.
unsafe impl Send for Session {}

impl Session {
    /// Start a real-time session under `name`, replacing one left behind by a
    /// previous run.
    ///
    /// A session outlives the process that started it — that is the point of the
    /// name — so a crash leaves one running and the next `StartTraceW` fails with
    /// `ERROR_ALREADY_EXISTS`. Stopping the old one and retrying is the documented
    /// recovery, and it is why the name is fixed rather than unique per run: a unique
    /// name would leak a session per crash with nothing to find them by.
    ///
    /// The leftover need not be ours to stop, though. `StartTraceW` checks the name
    /// *before* it checks privileges — an unheld name answers `ERROR_ACCESS_DENIED`
    /// where a held one answers `ERROR_ALREADY_EXISTS`, measured on this machine
    /// against `Circular Kernel Context Logger` — so an unelevated run that meets a
    /// session left behind by an elevated one gets this far and is then refused the
    /// stop. That is reported as the refusal it is, because "restart as administrator"
    /// both explains it and fixes it, where `ERROR_ALREADY_EXISTS` would surface as an
    /// unexplained number.
    pub fn start(name: &str) -> Result<Self, u32> {
        match Self::try_start(name) {
            Err(e) if e == ERROR_ALREADY_EXISTS => match Self::stop_by_name(name) {
                ERROR_SUCCESS | ERROR_WMI_INSTANCE_NOT_FOUND => Self::try_start(name),
                refused => Err(refused),
            },
            other => other,
        }
    }

    fn try_start(name: &str) -> Result<Self, u32> {
        let name = wide(name);
        let mut buf = Self::properties(&name);
        let mut handle: CONTROLTRACE_HANDLE = Default::default();
        let rc = unsafe {
            StartTraceW(
                &mut handle,
                name.as_ptr(),
                buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>(),
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(rc);
        }
        Ok(Self { handle, name })
    }

    /// `EVENT_TRACE_PROPERTIES` followed by the session name, as the API wants it:
    /// one allocation, with the name's byte offset recorded in the struct.
    fn properties(name: &[u16]) -> Vec<u8> {
        let head = std::mem::size_of::<EVENT_TRACE_PROPERTIES>();
        let mut buf = vec![0u8; head + name.len() * 2];
        // SAFETY: `buf` is at least one `EVENT_TRACE_PROPERTIES` long and zeroed,
        // which is a valid value for every field.
        let p = unsafe { &mut *buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>() };
        p.Wnode.BufferSize = buf.len() as u32;
        p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        // 1 = QPC. See the module note on timestamps.
        p.Wnode.ClientContext = 1;
        p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        // 64 KiB of buffer and a flush every second: a present-heavy trace produces a
        // few thousand small events a second, and the default 1 s flush is what keeps
        // the consumer's view roughly live rather than arriving in gulps.
        p.BufferSize = 64;
        p.MinimumBuffers = 4;
        p.MaximumBuffers = 16;
        p.FlushTimer = 1;
        p.LoggerNameOffset = head as u32;
        let tail = &mut buf[head..];
        for (i, c) in name.iter().enumerate() {
            tail[i * 2..i * 2 + 2].copy_from_slice(&c.to_le_bytes());
        }
        buf
    }

    /// Ask a provider to send its events to this session.
    ///
    /// `any_keyword` is the provider's own keyword bitmask: zero means every event it
    /// has, which for a graphics provider is a firehose, so callers name what they
    /// want.
    pub fn enable(&self, provider: &GUID, level: u8, any_keyword: u64) -> Result<(), u32> {
        let rc = unsafe {
            EnableTraceEx2(
                self.handle,
                provider,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER,
                level,
                any_keyword,
                0,
                0,
                std::ptr::null(),
            )
        };
        (rc == ERROR_SUCCESS).then_some(()).ok_or(rc)
    }

    pub fn name(&self) -> &[u16] {
        &self.name
    }

    /// Stop a session by name, whether or not this process started it, and report
    /// what ETW made of that.
    ///
    /// Three outcomes are all ordinary, which is why this reports rather than asserts:
    /// success, `ERROR_WMI_INSTANCE_NOT_FOUND` (it went away on its own), and
    /// `ERROR_ACCESS_DENIED` (it belongs to someone this process may not touch). An
    /// earlier version of this claimed the third could not happen and said so in a
    /// `debug_assert!`, which is how a leftover elevated session turned into a panic on
    /// the sampler thread — and, because the sampler is what fills the process table,
    /// into a window with nothing in it at all.
    fn stop_by_name(name: &str) -> u32 {
        let name = wide(name);
        let mut buf = Self::properties(&name);
        unsafe {
            ControlTraceW(
                Default::default(),
                name.as_ptr(),
                buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>(),
                EVENT_TRACE_CONTROL_STOP,
            )
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let mut buf = Self::properties(&self.name);
        unsafe {
            ControlTraceW(
                self.handle,
                std::ptr::null(),
                buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>(),
                EVENT_TRACE_CONTROL_STOP,
            )
        };
    }
}

/// What [`consume`] hands each event to.
///
/// The raw record comes along because identifying an event needs its manifest (see
/// [`describe`]) and the reduced [`Event`] deliberately does not carry one. It is
/// valid only for the duration of the call.
pub type Sink<'a> = dyn FnMut(&Event, *const EVENT_RECORD) + 'a;

/// Consume a running session until it stops, calling `sink` for every event.
///
/// Blocks: `ProcessTrace` returns only when the session is stopped or the callback
/// asks to stop, so this belongs on a thread of its own. Dropping the [`Session`]
/// from another thread is what ends it.
pub fn consume(session_name: &[u16], sink: &mut Sink<'_>) -> Result<(), u32> {
    // SAFETY: a zeroed `EVENT_TRACE_LOGFILEW` is the documented starting point; the
    // fields that matter are set below.
    let mut logfile: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
    logfile.LoggerName = session_name.as_ptr().cast_mut();
    logfile.Anonymous1.ProcessTraceMode =
        PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.Anonymous2.EventRecordCallback = Some(on_record);
    // The sink, reached from the callback. Rebound so that what `Context` points at is
    // the *reference*, which lives in this stack frame — and `ProcessTrace` below blocks
    // for the whole of the trace, so that is long enough. See [`context`].
    let mut sink: &mut Sink<'_> = sink;
    logfile.Context = context(&mut sink);

    let handle = unsafe { OpenTraceW(&mut logfile) };
    if handle.Value == INVALID_HANDLE_VALUE as u64 {
        return Err(unsafe { windows_sys::Win32::Foundation::GetLastError() });
    }
    let rc = unsafe { ProcessTrace(&handle, 1, std::ptr::null(), std::ptr::null()) };
    unsafe { CloseTrace(handle) };
    (rc == ERROR_SUCCESS).then_some(()).ok_or(rc)
}

/// The callback context: a pointer *to* the `&mut Sink`, never the reference itself.
///
/// `Sink` is a trait object, so `&mut Sink` is two words wide — the closure's address
/// and its vtable — and `Context` is one. An earlier version stored the reference there
/// directly, with a `.cast()` that quietly dropped the vtable half; [`on_record`] then
/// read the two words *at* the closure's address and called through the second of them.
/// In this app the sink captures two `Arc`s, so the supposed vtable was a refcount
/// block, and the process died on the first event that arrived.
///
/// That is why it survived every test: an unelevated run is refused its session, so no
/// event ever arrives and the bad pointer is never followed. Only an elevated run gets
/// that far, and then it crashes immediately.
///
/// A pointer to the reference is thin, so it fits, and both words survive.
fn context(sink: &mut &mut Sink<'_>) -> *mut std::ffi::c_void {
    (&raw mut *sink).cast()
}

/// `PEVENT_RECORD_CALLBACK`.
///
/// # Safety
/// Called by ETW with a record it owns for the duration of the call, and the
/// `UserContext` this module put in `EVENT_TRACE_LOGFILEW::Context`.
unsafe extern "system" fn on_record(record: *mut EVENT_RECORD) {
    let Some(record) = record.as_ref() else {
        return;
    };
    let context = record.UserContext as *mut &mut Sink<'_>;
    let Some(sink) = context.as_mut() else { return };
    let event = Event {
        provider: record.EventHeader.ProviderId,
        id: record.EventHeader.EventDescriptor.Id,
        pid: record.EventHeader.ProcessId,
        timestamp: record.EventHeader.TimeStamp,
    };
    sink(&event, record);
}

/// An event's task and opcode names, as its provider's manifest declares them.
///
/// Used to *identify* an event rather than to read one: matching on a name is stable
/// across Windows versions in a way that matching on a numeric id is not, and the
/// probe prints these so a reader can see what a machine actually emits.
///
/// Only the probe calls this: the app identifies a present by (provider, id), which
/// is a comparison rather than a manifest lookup and cheap enough to do per event.
///
/// # Safety
/// `record` must be a record ETW passed to a callback, valid for this call.
#[cfg_attr(not(test), allow(dead_code))]
pub unsafe fn describe(record: *const EVENT_RECORD) -> Option<(String, String)> {
    let mut size = 0u32;
    // First call sizes the buffer; it is expected to "fail" with more-data.
    TdhGetEventInformation(record, 0, std::ptr::null(), std::ptr::null_mut(), &mut size);
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let info = buf.as_mut_ptr().cast::<TRACE_EVENT_INFO>();
    if TdhGetEventInformation(record, 0, std::ptr::null(), info, &mut size) != ERROR_SUCCESS {
        return None;
    }
    let at = |offset: u32| -> String {
        if offset == 0 {
            return String::new();
        }
        let p = buf.as_ptr().add(offset as usize).cast::<u16>();
        let mut n = 0usize;
        while n < 256 && *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    };
    let task = at((*info).TaskNameOffset);
    let opcode = at((*info).OpcodeNameOffset);
    Some((task, opcode))
}

/// The providers that publish a present.
///
/// `Microsoft-Windows-DXGI` sees the application's call; `Microsoft-Windows-DxgKrnl`
/// sees the graphics kernel carry it out. Both are enabled because which one a given
/// application produces depends on how it presents — a Direct3D 12 swap chain, a
/// composed window, a fullscreen flip — and the probe is how we find out which is
/// there on real hardware.
pub mod provider {
    use windows_sys::core::GUID;

    /// `{CA11C036-0102-4A2D-A6AD-F03CFED5D3C9}`
    pub const DXGI: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
    /// `{802EC45A-1E99-4B83-9920-87C98277BA9D}`
    pub const DXGKRNL: GUID = GUID::from_u128(0x802EC45A_1E99_4B83_9920_87C98277BA9D);

    /// DxgKrnl's `Base` keyword: present and flip belong to it. Without a keyword the
    /// provider sends everything it has, which on a busy machine is tens of thousands
    /// of events a second.
    pub const DXGKRNL_BASE: u64 = 0x1;
    /// DXGI publishes few enough events that its whole set is affordable.
    pub const DXGI_ALL: u64 = 0;
}

/// Two GUIDs compare equal.
pub fn same_guid(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `on_record` can call the sink `consume` gave it.
    ///
    /// The two of them are exercised against each other with a hand-built record where
    /// ETW would be, because that is the only way to reach this code without an elevated
    /// session: the crash it guards against needed one real event to happen, and an
    /// unelevated machine never produces one. If the context ever goes back to being a
    /// fat pointer squeezed into a thin slot, this stops passing rather than waiting for
    /// the next administrator to find out.
    #[test]
    fn the_callback_can_call_the_sink_it_was_given() {
        let mut seen: Vec<(u16, u32, i64)> = Vec::new();
        {
            let mut collect =
                |e: &Event, _raw: *const EVENT_RECORD| seen.push((e.id, e.pid, e.timestamp));
            let mut sink: &mut Sink<'_> = &mut collect;

            // SAFETY: all-zero is a valid `EVENT_RECORD`, and `on_record` reads only the
            // header fields set here plus the context.
            let mut record: EVENT_RECORD = unsafe { std::mem::zeroed() };
            record.EventHeader.EventDescriptor.Id = 42;
            record.EventHeader.ProcessId = 4321;
            record.EventHeader.TimeStamp = 99;
            record.UserContext = context(&mut sink);

            // SAFETY: exactly the call ETW makes -- a record valid for the call, with the
            // context this module put there.
            unsafe { on_record(&mut record) };
        }
        assert_eq!(seen, [(42, 4321, 99)]);
    }

    /// What the graphics providers actually emit on *this* machine, and at what rate.
    ///
    /// Run this **from an elevated prompt**, with something rendering, before trusting
    /// the frame numbers on the Sensors tab:
    ///
    /// ```text
    /// set AZT_PROBE_SECS=8
    /// cargo test --bin azur-tasks probes_the_frame_providers -- --ignored --nocapture
    /// ```
    ///
    /// It starts the same session the app starts, watches for a few seconds, and
    /// prints every event kind it saw with the rate it implies for the busiest
    /// process. A present shows up as a steady 60, 120, 144 -- whatever the display is
    /// doing -- against a process you recognise; everything else can be told apart by
    /// eye.
    ///
    /// It exists because the app cannot verify itself unelevated, and code that has
    /// never met the data it parses reports plausible nonsense. That is precisely how
    /// the AMD backend shipped a filter that made every Radeon look absent.
    #[test]
    #[ignore = "needs administrator rights and something on screen; see the doc comment"]
    fn probes_the_frame_providers() {
        use std::collections::BTreeMap;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        let seconds: u64 = std::env::var("AZT_PROBE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8);

        let session = match Session::start("azt-frameprobe") {
            Ok(s) => s,
            Err(5) => panic!(
                "Access denied: an ETW session needs administrator rights. \
                 Run this from an elevated prompt."
            ),
            Err(e) => panic!("could not start the trace session: Windows error {e}"),
        };
        for (name, guid, keyword) in [
            ("DXGI", provider::DXGI, provider::DXGI_ALL),
            ("DxgKrnl", provider::DXGKRNL, provider::DXGKRNL_BASE),
        ] {
            match session.enable(&guid, 4, keyword) {
                Ok(()) => println!("enabled  {name}"),
                Err(e) => println!("enabled  {name}  FAILED (Windows error {e})"),
            }
        }

        /// One event kind, as seen: what it is called, how many, and per process the
        /// count and the span it arrived over.
        #[derive(Default)]
        struct Kind {
            task: String,
            opcode: String,
            total: u64,
            by_pid: BTreeMap<u32, (u64, i64, i64)>,
        }

        let kinds: Arc<Mutex<BTreeMap<(u32, u16), Kind>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let done = Arc::new(AtomicBool::new(false));
        let consumer = {
            let kinds = Arc::clone(&kinds);
            let done = Arc::clone(&done);
            let name = session.name().to_vec();
            std::thread::spawn(move || {
                let mut sink = |e: &Event, raw: *const EVENT_RECORD| {
                    if done.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut map = kinds.lock().expect("probe map");
                    let kind = map.entry((e.provider.data1, e.id)).or_default();
                    if kind.total == 0 {
                        // Once per kind: TDH walks the provider's manifest and is far
                        // too slow to call per event.
                        if let Some((task, opcode)) = unsafe { describe(raw) } {
                            kind.task = task;
                            kind.opcode = opcode;
                        }
                    }
                    kind.total += 1;
                    let slot = kind
                        .by_pid
                        .entry(e.pid)
                        .or_insert((0, e.timestamp, e.timestamp));
                    slot.0 += 1;
                    slot.2 = e.timestamp;
                };
                let _ = consume(&name, &mut sink);
            })
        };

        println!("watching for {seconds}s -- put something on screen that renders\n");
        std::thread::sleep(std::time::Duration::from_secs(seconds));
        done.store(true, Ordering::Relaxed);
        // Stopping the session is what makes `ProcessTrace` return.
        drop(session);
        let _ = consumer.join();

        let kinds = kinds.lock().expect("probe map");
        assert!(
            !kinds.is_empty(),
            "no events at all: either nothing rendered, or both providers refused"
        );

        let freq = qpc_frequency() as f64;
        let mut rows: Vec<(f64, String)> = Vec::new();
        for ((provider_id, event_id), kind) in kinds.iter() {
            let Some((pid, (count, first, last))) = kind
                .by_pid
                .iter()
                .max_by_key(|(_, (c, _, _))| *c)
                .map(|(p, v)| (*p, *v))
            else {
                continue;
            };
            let span = (last - first) as f64 / freq;
            let rate = if span > 0.1 && count > 1 {
                (count - 1) as f64 / span
            } else {
                0.0
            };
            // The signature being looked for: a steady per-process rate somewhere in
            // the range a display refreshes at.
            let plausible = (20.0..400.0).contains(&rate);
            rows.push((
                if plausible { rate } else { -1.0 },
                format!(
                    "{m} {provider_id:08x}/{event_id:<4} {task:<26} {opcode:<14} \
                     {total:>8} ev  pid {pid:<7} {count:>7} @ {rate:>7.1}/s",
                    m = if plausible { "*" } else { " " },
                    task = kind.task,
                    opcode = kind.opcode,
                    total = kind.total,
                ),
            ));
        }
        rows.sort_by(|a, b| b.0.total_cmp(&a.0));
        println!("{} event kind(s):\n", rows.len());
        for (_, line) in &rows {
            println!("{line}");
        }
        println!(
            "\n* marks a kind whose busiest process saw it at a plausible frame rate.\n\
             The present is the one whose rate matches the display and whose pid is the\n\
             thing that was on screen."
        );
    }
}
