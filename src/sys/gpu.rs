//! GPU utilization and video-memory usage via PDH performance counters.
//!
//! There is no kernel call that reports GPU load the way
//! `NtQuerySystemInformation` reports CPU load; the GPU counters live in the
//! `GPU Engine` / `GPU Adapter Memory` performance-counter sets, which is what
//! Task Manager reads too. Everything here degrades to "no GPU tab" rather than
//! failing the app: these counters are absent in some VMs and on some drivers.

use super::snapshot::GpuStats;
use super::util::{from_wide_nul, wide};
use rustc_hash::FxHashMap;
use std::sync::Arc;

use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

const PDH_CSTATUS_VALID_DATA: u32 = 0;
const PDH_MORE_DATA: u32 = 0x8000_07D2;

/// `\GPU Engine(*)\Utilization Percentage`
const PATH_ENGINE: &str = r"\GPU Engine(*)\Utilization Percentage";
/// `\GPU Adapter Memory(*)\Dedicated Usage`
const PATH_DEDICATED: &str = r"\GPU Adapter Memory(*)\Dedicated Usage";
/// `\GPU Adapter Memory(*)\Shared Usage`
const PATH_SHARED: &str = r"\GPU Adapter Memory(*)\Shared Usage";

struct Counter {
    handle: PDH_HCOUNTER,
    buf: Vec<u8>,
}

impl Counter {
    fn add(query: PDH_HQUERY, path: &str) -> Option<Self> {
        let wpath = wide(path);
        let mut handle: PDH_HCOUNTER = std::ptr::null_mut();
        let rc = unsafe { PdhAddEnglishCounterW(query, wpath.as_ptr(), 0, &mut handle) };
        (rc == 0).then_some(Self {
            handle,
            buf: vec![0u8; 16 * 1024],
        })
    }

    /// Read the counter's instances as `(instance_name, value)` pairs.
    fn read(&mut self, out: &mut Vec<(String, f64)>) {
        out.clear();
        for _ in 0..3 {
            let mut size = self.buf.len() as u32;
            let mut count = 0u32;
            let rc = unsafe {
                PdhGetFormattedCounterArrayW(
                    self.handle,
                    PDH_FMT_DOUBLE,
                    &mut size,
                    &mut count,
                    self.buf.as_mut_ptr().cast(),
                )
            };

            if rc == PDH_MORE_DATA {
                self.buf.resize((size as usize).max(self.buf.len() * 2), 0);
                continue;
            }
            if rc != 0 {
                return;
            }

            let items = self.buf.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            for i in 0..count as usize {
                let item: PDH_FMT_COUNTERVALUE_ITEM_W =
                    unsafe { std::ptr::read_unaligned(items.add(i)) };
                if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA || item.szName.is_null() {
                    continue;
                }
                // szName points into `self.buf`, after the item array.
                let name = unsafe { read_pwstr(item.szName) };
                // SAFETY: CStatus == VALID_DATA means the union holds a double.
                let value = unsafe { item.FmtValue.Anonymous.doubleValue };
                out.push((name, value));
            }
            return;
        }
    }
}

unsafe fn read_pwstr(p: *const u16) -> String {
    let mut len = 0usize;
    // Instance names are short; the bound just prevents a runaway on a
    // malformed buffer.
    while len < 512 && *p.add(len) != 0 {
        len += 1;
    }
    from_wide_nul(std::slice::from_raw_parts(p, len))
}

pub struct GpuProbe {
    query: PDH_HQUERY,
    engine: Option<Counter>,
    dedicated: Option<Counter>,
    shared: Option<Counter>,
    items: Vec<(String, f64)>,
    /// LUID -> display name, resolved once.
    names: FxHashMap<u64, Arc<str>>,
    /// Stable ordering so the adapter list does not reshuffle between ticks.
    order: Vec<u64>,
    /// Real adapter identities from DXGI, keyed by LUID.
    adapters: FxHashMap<u64, super::dxgi::Adapter>,
    /// Set once the first tick has primed the counters.
    primed: bool,
}

impl GpuProbe {
    /// Returns `None` when the GPU counter set is unavailable.
    pub fn new() -> Option<Self> {
        let mut query: PDH_HQUERY = std::ptr::null_mut();
        if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
            return None;
        }

        let engine = Counter::add(query, PATH_ENGINE);
        if engine.is_none() {
            // No GPU Engine counter set -> nothing meaningful to show.
            unsafe { PdhCloseQuery(query) };
            return None;
        }
        let dedicated = Counter::add(query, PATH_DEDICATED);
        let shared = Counter::add(query, PATH_SHARED);

        // A rate counter needs two collections before it yields a value.
        unsafe { PdhCollectQueryData(query) };

        let adapters = super::dxgi::enumerate()
            .into_iter()
            .map(|a| (a.luid, a))
            .collect();

        Some(Self {
            query,
            engine,
            dedicated,
            shared,
            items: Vec::new(),
            names: FxHashMap::default(),
            order: Vec::new(),
            adapters,
            primed: false,
        })
    }

    /// Total bytes held by the three counter-array buffers. PDH sizes these from
    /// the instance count, and `GPU Engine(*)` has one instance per process per
    /// engine, so this is the one thing here that tracks machine load.
    #[cfg(feature = "memtrace")]
    pub fn debug_buf_bytes(&self) -> usize {
        [&self.engine, &self.dedicated, &self.shared]
            .iter()
            .filter_map(|c| c.as_ref())
            .map(|c| c.buf.capacity())
            .sum::<usize>()
            + self.items.capacity() * std::mem::size_of::<(String, f64)>()
    }

    pub fn sample(&mut self, out: &mut Vec<GpuStats>) {
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return;
        }
        // The first collection after construction only primes the counters.
        if !self.primed {
            self.primed = true;
            return;
        }

        // luid -> (engtype -> summed utilization)
        let mut per_adapter: FxHashMap<u64, FxHashMap<u32, f64>> = FxHashMap::default();

        if let Some(c) = self.engine.as_mut() {
            c.read(&mut self.items);
            for (name, value) in &self.items {
                let Some(luid) = parse_luid(name) else { continue };
                let engtype = parse_engtype(name).unwrap_or(0);
                *per_adapter
                    .entry(luid)
                    .or_default()
                    .entry(engtype)
                    .or_insert(0.0) += *value;
            }
        }

        let mut dedicated: FxHashMap<u64, f64> = FxHashMap::default();
        if let Some(c) = self.dedicated.as_mut() {
            c.read(&mut self.items);
            for (name, value) in &self.items {
                if let Some(luid) = parse_luid(name) {
                    *dedicated.entry(luid).or_insert(0.0) += *value;
                }
            }
        }
        let mut shared: FxHashMap<u64, f64> = FxHashMap::default();
        if let Some(c) = self.shared.as_mut() {
            c.read(&mut self.items);
            for (name, value) in &self.items {
                if let Some(luid) = parse_luid(name) {
                    *shared.entry(luid).or_insert(0.0) += *value;
                }
            }
        }

        // Adapters with memory reported but no engine activity still exist.
        for luid in dedicated.keys() {
            per_adapter.entry(*luid).or_default();
        }

        // Drop adapters that neither DXGI knows about nor have any memory or
        // activity: the counter set exposes render-only pseudo adapters that
        // Task Manager does not show.
        per_adapter.retain(|luid, engines| {
            let known = self.adapters.contains_key(luid);
            let busy = engines.values().any(|&v| v > 0.0)
                || dedicated.get(luid).copied().unwrap_or(0.0) > 0.0;
            // A software adapter (WARP / Basic Render) is never interesting.
            let software = self.adapters.get(luid).is_some_and(|a| a.is_software);
            !software && (known || busy)
        });

        // Keep first-seen order stable across ticks; order new arrivals by the
        // DXGI enumeration index so the primary GPU tends to come first.
        let mut fresh: Vec<u64> = per_adapter
            .keys()
            .copied()
            .filter(|k| !self.order.contains(k))
            .collect();
        fresh.sort_unstable();
        for k in fresh {
            self.order.push(k);
        }

        for (slot, luid) in self.order.iter().enumerate() {
            let Some(engines) = per_adapter.get(luid) else {
                continue;
            };
            // Task Manager reports the busiest *engine type*, not the sum:
            // 3D and Video Decode running at 50% each is 50% busy, not 100%.
            let utilization = engines
                .values()
                .copied()
                .fold(0.0f64, f64::max)
                .clamp(0.0, 100.0) as f32;

            let adapters = &self.adapters;
            let name = self
                .names
                .entry(*luid)
                .or_insert_with(|| match adapters.get(luid) {
                    Some(a) => format!("GPU {slot} ({})", a.name).into(),
                    None => format!("GPU {slot}").into(),
                })
                .clone();

            out.push(GpuStats {
                name,
                utilization,
                dedicated_used: dedicated.get(luid).copied().unwrap_or(0.0).max(0.0) as u64,
                dedicated_total: self
                    .adapters
                    .get(luid)
                    .map(|a| a.dedicated_video_memory)
                    .unwrap_or(0),
                shared_used: shared.get(luid).copied().unwrap_or(0.0).max(0.0) as u64,
            });
        }
    }
}

impl Drop for GpuProbe {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

/// Instance names look like
/// `pid_1234_luid_0x00000000_0x0000A1B2_phys_0_eng_0_engtype_3D`, where the
/// first hex word is the LUID's high part and the second its low part.
///
/// Returns the same packed `high << 32 | low` key that [`super::dxgi`] produces,
/// so counter instances can be joined against real adapter identities.
fn parse_luid(instance: &str) -> Option<u64> {
    let rest = instance.split("luid_").nth(1)?;
    let mut it = rest.split('_');
    let hi = parse_hex(it.next()?)?;
    let lo = parse_hex(it.next()?)?;
    Some(((hi as u64) << 32) | lo as u64)
}

fn parse_hex(s: &str) -> Option<u32> {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    u32::from_str_radix(s, 16).ok()
}

/// Map the `engtype_*` suffix to a small id so we can group by engine type.
fn parse_engtype(instance: &str) -> Option<u32> {
    let tag = instance.split("engtype_").nth(1)?;
    Some(match tag {
        "3D" => 0,
        "VideoDecode" => 1,
        "VideoEncode" => 2,
        "VideoProcessing" => 3,
        "Copy" => 4,
        "Compute" => 5,
        "Security" => 6,
        // Unknown engine types still need distinct buckets, or they would be
        // summed together and overstate utilization.
        other => 7 + (other.len() as u32 % 8),
    })
}

// Adapter names and VRAM totals come from `super::dxgi`, which can key them by
// LUID. `EnumDisplayDevices` was tried first and rejected: it reports adapters
// in an order unrelated to the counters' LUIDs, so names landed on the wrong
// graphs.
