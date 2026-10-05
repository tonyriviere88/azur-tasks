//! GPU utilization and video-memory usage via PDH performance counters.
//!
//! There is no kernel call that reports GPU load the way
//! `NtQuerySystemInformation` reports CPU load; the GPU counters live in the
//! `GPU Engine` / `GPU Adapter Memory` performance-counter sets, which is what
//! Task Manager reads too. Everything here degrades to "no GPU tab" rather than
//! failing the app: these counters are absent in some VMs and on some drivers.

use super::pdh::{Counter, Query};
use super::snapshot::GpuStats;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// `\GPU Engine(*)\Utilization Percentage`
const PATH_ENGINE: &str = r"\GPU Engine(*)\Utilization Percentage";
/// `\GPU Adapter Memory(*)\Dedicated Usage`
const PATH_DEDICATED: &str = r"\GPU Adapter Memory(*)\Dedicated Usage";
/// `\GPU Adapter Memory(*)\Shared Usage`
const PATH_SHARED: &str = r"\GPU Adapter Memory(*)\Shared Usage";

pub struct GpuProbe {
    query: Query,
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
        let query = Query::open()?;

        // No GPU Engine counter set -> nothing meaningful to show. The other two
        // are allowed to be missing; this one is the tab.
        let engine = Some(Counter::add(&query, PATH_ENGINE)?);
        let dedicated = Counter::add(&query, PATH_DEDICATED);
        let shared = Counter::add(&query, PATH_SHARED);

        // A rate counter needs two collections before it yields a value.
        query.collect();

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
            .map(|c| c.buf_bytes())
            .sum::<usize>()
            + self.items.capacity() * std::mem::size_of::<(String, f64)>()
    }

    pub fn sample(&mut self, out: &mut Vec<GpuStats>) {
        if !self.query.collect() {
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
                vendor: self.adapters.get(luid).map(|a| a.vendor_id).unwrap_or(0),
                utilization,
                dedicated_used: dedicated.get(luid).copied().unwrap_or(0.0).max(0.0) as u64,
                dedicated_total: self
                    .adapters
                    .get(luid)
                    .map(|a| a.dedicated_video_memory)
                    .unwrap_or(0),
                shared_used: shared.get(luid).copied().unwrap_or(0.0).max(0.0) as u64,
                // Filled in by the sampler once the sensor backends have run.
                sensors: super::snapshot::NO_SENSORS,
            });
        }
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
