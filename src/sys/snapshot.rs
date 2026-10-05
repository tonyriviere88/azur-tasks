//! The immutable data model handed from the sampler thread to the UI thread.
//!
//! A `Snapshot` is built off-thread and published through an `ArcSwap`. The UI
//! never locks and never copies: it loads the pointer once per frame and reads
//! straight out of it.

use std::sync::Arc;

/// How many samples of history each graph keeps. At the default 1 s cadence
/// this is 60 seconds, matching Task Manager's window.
pub const HISTORY_LEN: usize = 60;

// ---------------------------------------------------------------------------
// Process
// ---------------------------------------------------------------------------

/// Top-level grouping.
///
/// Task Manager splits non-app processes into "Background processes" and
/// "Windows processes". The line between them is a guess either way -- it comes
/// down to whether the image sits under `C:\Windows` -- and it pushes a third
/// of the list into a section nobody looks at. One background section is more
/// useful.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Category {
    App,
    Background,
    /// Frozen by the shell, or suspended by a debugger: running in name only.
    ///
    /// Its own section rather than a status on a row in the others, because a
    /// suspended process consumes nothing and can do nothing, and a reader scanning
    /// for what is using the machine wants them all out of the way at once. The
    /// section is last for the same reason.
    Suspended,
}

impl Category {
    /// In the order they are shown.
    pub const ALL: [Category; 3] = [Self::App, Self::Background, Self::Suspended];

    /// Group-header text.
    pub fn label(self) -> &'static str {
        match self {
            Self::App => "Apps",
            Self::Background => "Background processes",
            Self::Suspended => "Suspended processes",
        }
    }

    /// Short form, for the Type column.
    pub fn type_label(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Background => "Background",
            Self::Suspended => "Suspended",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunState {
    Running,
    Suspended,
}

/// One process, as displayed. Field order groups hot (per-frame sorted/painted)
/// data first so a table pass touches fewer cache lines.
#[derive(Clone)]
pub struct Proc {
    // --- identity -------------------------------------------------------
    pub pid: u32,
    pub ppid: u32,
    /// FILETIME. Also disambiguates recycled PIDs.
    pub create_time: i64,
    /// Image file name, e.g. `chrome.exe`.
    pub name: Arc<str>,

    // --- rates (recomputed every tick) ----------------------------------
    /// Percent of total machine CPU capacity, 0..=100.
    pub cpu: f32,
    /// The part of [`Self::cpu`] this process spent in kernel mode -- drivers, page
    /// faults, the file and network stacks -- on the same scale and never more than
    /// it. The per-process counterpart of [`CpuStats::kernel`].
    pub cpu_kernel: f32,
    /// Disk bytes/sec (read + write + other transfers).
    pub disk_bps: f32,
    pub io_read_bps: f32,
    pub io_write_bps: f32,
    pub hard_faults_per_s: f32,

    // --- memory ---------------------------------------------------------
    /// Private working set -- Task Manager's "Memory" column.
    pub private_ws: u64,
    pub working_set: u64,
    pub peak_ws: u64,
    /// `PagefileUsage`; Task Manager calls this "Commit size".
    pub commit: u64,
    pub virtual_size: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,

    // --- counts ---------------------------------------------------------
    pub threads: u32,
    pub handles: u32,
    pub session: u32,
    pub base_priority: i32,
    pub page_faults: u32,

    // --- cumulative totals ----------------------------------------------
    pub cpu_time_100ns: u64,
    pub cycle_time: u64,
    pub io_read_total: u64,
    pub io_write_total: u64,
    pub io_other_total: u64,
    pub read_ops: u64,
    pub write_ops: u64,
    pub other_ops: u64,

    // --- classification -------------------------------------------------
    pub state: RunState,
    /// What this process is on its own merits.
    pub category: Category,
    /// Which top-level group this process is *displayed* under.
    ///
    /// Distinct from `category`: a browser's renderer children are background
    /// processes in their own right, but they belong under the browser in Apps.
    /// Every process appears exactly once, under the group of the nearest
    /// ancestor-or-self that heads a group.
    pub group: Category,
    /// True when this process heads its own entry in `group` rather than nesting
    /// under a parent: it owns a window, has no visible parent, or was merely
    /// *launched* by its parent -- the shell, the service manager, a COM surrogate
    /// host. Task Manager's rule; see `sampler::is_launcher` for why the parent
    /// field alone cannot express it.
    pub group_head: bool,
    /// Title of this process's main window, when it owns a visible one.
    pub window_title: Option<Arc<str>>,

    // --- subtree rollups -------------------------------------------------
    // Sums over self and every descendant the list nests *under* this row --
    // which stops at a descendant that heads its own entry, because that one is
    // a row beside this one rather than inside it. Used for the collapsed rows
    // and for the CPU / Memory / Disk sort keys, exactly like Task Manager: a
    // folded parent shows, and is ranked by, what its whole group is doing.
    pub sub_cpu: f32,
    pub sub_private_ws: u64,
    pub sub_disk_bps: f32,
    pub sub_count: u32,

    // --- tree ------------------------------------------------------------
    /// Index of the parent in `Snapshot::procs`, or `u32::MAX` for a root.
    pub parent: u32,
    /// Slice into `Snapshot::child_links` (CSR adjacency layout).
    pub child_start: u32,
    pub child_len: u32,
    pub depth: u16,
}

impl Proc {
    #[inline]
    pub fn has_children(&self) -> bool {
        self.child_len > 0
    }

    /// Stable identity across ticks, immune to PID recycling.
    #[inline]
    pub fn key(&self) -> ProcKey {
        ProcKey {
            pid: self.pid,
            create_time: self.create_time,
        }
    }
}

/// An empty process, for a test that builds a machine by hand.
///
/// Test-only on purpose: the sampler fills every field from a real
/// `SYSTEM_PROCESS_INFORMATION`, and a `Default` it could reach for would let a
/// forgotten field ship as a zero. A fixture wants the opposite -- to name the
/// three or four fields the rule under test reads, and say nothing about the rest.
#[cfg(test)]
impl Default for Proc {
    fn default() -> Self {
        Self {
            pid: 0,
            ppid: 0,
            create_time: 0,
            name: Arc::from(""),
            cpu: 0.0,
            cpu_kernel: 0.0,
            disk_bps: 0.0,
            io_read_bps: 0.0,
            io_write_bps: 0.0,
            hard_faults_per_s: 0.0,
            private_ws: 0,
            working_set: 0,
            peak_ws: 0,
            commit: 0,
            virtual_size: 0,
            paged_pool: 0,
            nonpaged_pool: 0,
            threads: 0,
            handles: 0,
            session: 0,
            base_priority: 0,
            page_faults: 0,
            cpu_time_100ns: 0,
            cycle_time: 0,
            io_read_total: 0,
            io_write_total: 0,
            io_other_total: 0,
            read_ops: 0,
            write_ops: 0,
            other_ops: 0,
            state: RunState::Running,
            category: Category::Background,
            group: Category::Background,
            group_head: false,
            window_title: None,
            sub_cpu: 0.0,
            sub_private_ws: 0,
            sub_disk_bps: 0.0,
            sub_count: 1,
            parent: u32::MAX,
            child_start: 0,
            child_len: 0,
            depth: 0,
        }
    }
}

/// A PID paired with its creation time. Windows recycles PIDs aggressively, so
/// a bare PID is not a stable key -- selection and expansion state would jump
/// to an unrelated process.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ProcKey {
    pub pid: u32,
    pub create_time: i64,
}

// ---------------------------------------------------------------------------
// System-wide counters
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct CpuStats {
    /// Total utilization across all logical processors, 0..=100.
    pub total: f32,
    /// Per-logical-processor utilization, 0..=100.
    pub per_core: Vec<f32>,
    /// Fraction of total time spent in kernel mode, 0..=100.
    pub kernel: f32,
    pub processes: u32,
    pub threads: u32,
    pub handles: u32,
    /// Seconds since boot.
    pub uptime_secs: u64,
}

#[derive(Clone, Default)]
pub struct MemStats {
    pub total: u64,
    pub available: u64,
    pub in_use: u64,
    pub committed: u64,
    pub commit_limit: u64,
    pub cached: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    // Memory composition bar.
    pub free: u64,
    pub modified: u64,
    pub standby: u64,
}

impl MemStats {
    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.in_use as f32 / self.total as f32 * 100.0
        }
    }
}

#[derive(Clone, Default)]
pub struct DiskStats {
    pub name: Arc<str>,
    /// Percent of time the disk had a request outstanding, 0..=100.
    pub active_pct: f32,
    pub read_bps: f32,
    pub write_bps: f32,
    /// Average response time in milliseconds.
    pub avg_response_ms: f32,
    pub capacity: u64,
}

#[derive(Clone, Default)]
pub struct NetStats {
    pub name: Arc<str>,
    pub recv_bps: f32,
    pub send_bps: f32,
    /// Link speed in bits/sec.
    pub link_speed: u64,
    pub up: bool,
}

#[derive(Clone)]
pub struct GpuStats {
    pub name: Arc<str>,
    /// PCI vendor id from DXGI, so a sensor backend can be matched to the adapter
    /// it speaks for. Zero when DXGI could not describe the adapter.
    pub vendor: u32,
    /// Index into [`Sensors::gpus`] of this adapter's readings, or `u32::MAX` when
    /// no vendor interface speaks for it. Resolved once by the sampler, so the UI
    /// and the history buffers agree on the pairing.
    pub sensors: u32,
    /// Busiest engine, 0..=100.
    pub utilization: f32,
    pub dedicated_used: u64,
    pub dedicated_total: u64,
    pub shared_used: u64,
}

// ---------------------------------------------------------------------------
// Sensors
// ---------------------------------------------------------------------------

/// One ACPI thermal zone.
#[derive(Clone)]
pub struct ThermalZone {
    /// The firmware's name for the zone, translated where the tag is
    /// conventional. See [`crate::sys::sensors`].
    pub label: Arc<str>,
    pub celsius: f32,
}

/// PCI vendor ids, as DXGI reports them. Which sensor interface can speak for an
/// adapter is decided by these and nothing else — a name match would turn on how
/// a driver spells its own product.
pub mod vendor {
    pub const NVIDIA: u32 = 0x10DE;
    pub const INTEL: u32 = 0x8086;
    /// AMD and the ATI ids it inherited are the same number.
    pub const AMD: u32 = 0x1002;
}

/// One GPU's sensors, whichever vendor interface produced them.
///
/// `None` means "this device has no such sensor", which is the common case: an
/// integrated GPU has no temperature of its own (it is part of the CPU package),
/// and a laptop board's fan belongs to the embedded controller rather than to the
/// GPU.
#[derive(Clone, Default)]
pub struct GpuSensors {
    /// PCI vendor of the device these came from, and which device of that vendor —
    /// each backend enumerates only its own, in PCI order, so the pair is what
    /// pins a reading to an adapter.
    pub vendor: u32,
    pub ordinal: u32,
    /// The interface that produced them, for the reader: "NVML", "Level Zero",
    /// "ADL". A page of vendor-specific numbers should say whose they are.
    pub source: &'static str,
    /// The device's name *as its own driver gives it*, which is not always what
    /// DXGI calls the same board.
    pub name: Arc<str>,
    pub temp_c: Option<f32>,
    /// The hottest point on the die — AMD's "junction" temperature.
    ///
    /// Separate from [`Self::temp_c`], which is the die *edge*, and always the
    /// higher of the two: the edge is what the board is dissipating, the hotspot is
    /// what the card throttles against. Only AMD publishes it; the field is `None`
    /// elsewhere and no card is drawn for it.
    pub hotspot_c: Option<f32>,
    pub fan_pct: Option<f32>,
    pub fan_rpm: Option<u32>,
    pub core_mhz: Option<u32>,
    pub mem_mhz: Option<u32>,
    pub power_w: Option<f32>,
    pub util_pct: Option<f32>,
    /// Device-wide video memory, in bytes: every process on the board.
    pub vram_used: u64,
    pub vram_total: u64,
    /// What this adapter reports about the *CPU* it shares a package with.
    ///
    /// An oddity of AMD's interface rather than a modelling mistake: on an APU the
    /// graphics and the processor are one die, and ADL's sensor set carries both. The
    /// sampler lifts these into [`Sensors::cpu_temp_c`] and [`Sensors::cpu_power_w`],
    /// which is where anything reading them should look.
    pub cpu_temp_c: Option<f32>,
    pub cpu_power_w: Option<f32>,
}

impl GpuSensors {
    /// The setters below refuse a reading that cannot be one.
    ///
    /// Not defensive habit: two of the three interfaces here identify a sensor by
    /// an *index* into a vendor enum, and an index that shifted between driver
    /// generations returns a real number for the wrong quantity. A reading outside
    /// physical possibility is the one sign of that available at run time, and a
    /// dropped reading shows as "—" while a wrong one shows as fact.
    pub fn set_temp(&mut self, celsius: f32) {
        if (-40.0..=150.0).contains(&celsius) {
            self.temp_c = Some(celsius);
        }
    }

    pub fn set_hotspot(&mut self, celsius: f32) {
        if (-40.0..=150.0).contains(&celsius) {
            self.hotspot_c = Some(celsius);
        }
    }

    pub fn set_fan_pct(&mut self, pct: f32) {
        if (0.0..=100.0).contains(&pct) {
            self.fan_pct = Some(pct);
        }
    }

    pub fn set_fan_rpm(&mut self, rpm: i64) {
        if (1..=30_000).contains(&rpm) {
            self.fan_rpm = Some(rpm as u32);
        }
    }

    pub fn set_core_mhz(&mut self, mhz: f64) {
        if (1.0..=8_000.0).contains(&mhz) {
            self.core_mhz = Some(mhz as u32);
        }
    }

    pub fn set_mem_mhz(&mut self, mhz: f64) {
        if (1.0..=20_000.0).contains(&mhz) {
            self.mem_mhz = Some(mhz as u32);
        }
    }

    pub fn set_power_w(&mut self, watts: f32) {
        if (0.0..=1_000.0).contains(&watts) {
            self.power_w = Some(watts);
        }
    }

    pub fn set_util_pct(&mut self, pct: f32) {
        if (0.0..=100.0).contains(&pct) {
            self.util_pct = Some(pct);
        }
    }
}

/// What the machine's own sensors report, as far as an unprivileged process can
/// see. Every part is empty on hardware that has no such sensor: nothing here is
/// estimated or inferred from anything else.
#[derive(Clone, Default)]
pub struct Sensors {
    pub zones: Vec<ThermalZone>,
    /// Every GPU any backend could speak for, in no particular order — matched to
    /// an adapter by `(vendor, ordinal)`.
    pub gpus: Vec<GpuSensors>,
    /// Mean current clock across the logical processors, MHz.
    pub cpu_mhz: u32,
    /// Nominal maximum, MHz. The current clock exceeds it when the part boosts.
    pub cpu_max_mhz: u32,
    /// Package temperature, from whichever source could give one: the vendor's own
    /// sensor for preference, the firmware's thermal zone otherwise.
    pub cpu_temp_c: Option<f32>,
    /// Package power in watts — the whole processor, not just its cores.
    pub cpu_power_w: Option<f32>,
    /// One line per vendor library that is installed and yet answered nothing,
    /// tagged with the PCI vendor it speaks for.
    ///
    /// An adapter with no readings has three quite different causes: there is no
    /// such library on the machine (ordinary — most machines have one vendor's, not
    /// three), or it is there and refused to start, or it started and enumerated no
    /// adapter. Silence cannot tell them apart, and the last two are bugs someone
    /// can act on, so they are said out loud.
    pub notes: Vec<(u32, Arc<str>)>,
}

impl Default for GpuStats {
    fn default() -> Self {
        Self {
            name: Arc::from(""),
            vendor: 0,
            sensors: NO_SENSORS,
            utilization: 0.0,
            dedicated_used: 0,
            dedicated_total: 0,
            shared_used: 0,
        }
    }
}

/// [`GpuStats::sensors`] when nothing speaks for the adapter.
pub const NO_SENSORS: u32 = u32::MAX;

/// What the frame tracer knows about the foreground application.
#[derive(Clone)]
pub struct Frames {
    /// Why there are no numbers, when there are none.
    pub status: crate::sys::frames::Status,
    /// The process the numbers are about, and its name.
    pub pid: u32,
    pub name: Arc<str>,
    pub stats: Option<crate::sys::frames::FrameStats>,
}

impl Default for Frames {
    fn default() -> Self {
        Self {
            status: crate::sys::frames::Status::NeedsElevation,
            pid: 0,
            name: Arc::from(""),
            stats: None,
        }
    }
}

impl Sensors {
    /// The reading of a named zone, if the firmware publishes one.
    pub fn zone(&self, label: &str) -> Option<f32> {
        self.zones
            .iter()
            .find(|z| &*z.label == label)
            .map(|z| z.celsius)
    }
}

// ---------------------------------------------------------------------------
// History ring buffer
// ---------------------------------------------------------------------------

/// Fixed-capacity ring of `f32` samples. Never allocates after construction,
/// so graph history costs a known, constant amount of memory.
#[derive(Clone)]
pub struct History {
    buf: [f32; HISTORY_LEN],
    /// Where the next sample goes.
    head: usize,
    len: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            buf: [0.0; HISTORY_LEN],
            head: 0,
            len: 0,
        }
    }
}

impl History {
    /// A series with nothing in it, usable in a `static`: the UI needs one to
    /// stand in for a device that appeared between ticks and has no slot yet.
    pub const EMPTY: Self = Self {
        buf: [0.0; HISTORY_LEN],
        head: 0,
        len: 0,
    };

    #[inline]
    pub fn push(&mut self, v: f32) {
        self.buf[self.head] = v;
        self.head = (self.head + 1) % HISTORY_LEN;
        if self.len < HISTORY_LEN {
            self.len += 1;
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Samples oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        let start = (self.head + HISTORY_LEN - self.len) % HISTORY_LEN;
        (0..self.len).map(move |i| self.buf[(start + i) % HISTORY_LEN])
    }

    pub fn max(&self) -> f32 {
        self.iter().fold(0.0f32, f32::max)
    }

    /// The newest sample, or zero before there is one. For a series the UI graphs
    /// but does not otherwise carry a live value for.
    pub fn last(&self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        self.buf[(self.head + HISTORY_LEN - 1) % HISTORY_LEN]
    }
}

/// Every graph's history, carried alongside the live values.
#[derive(Clone, Default)]
pub struct Histories {
    pub cpu: History,
    /// The share of the same wall clock that went to kernel mode, 0..=100.
    ///
    /// A *part of* [`Self::cpu`] rather than a series beside it: it is never the
    /// larger of the two, which is why the graph draws it as a band inside the
    /// total instead of a second line that could cross it.
    pub cpu_kernel: History,
    pub per_core: Vec<History>,
    pub mem: History,
    /// Per-disk active time, 0..=100.
    pub disk: Vec<History>,
    /// Per-disk combined read+write throughput in bytes/sec.
    pub disk_rate: Vec<History>,
    pub net_recv: Vec<History>,
    pub net_send: Vec<History>,
    pub gpu: Vec<History>,

    // --- sensors -------------------------------------------------------
    // A sensor reading is only worth a graph because of where it has been: a
    // package at 60 C means one thing after a minute of idling and another
    // thirty seconds into a build.
    pub cpu_temp: History,
    /// Mean CPU clock in MHz.
    pub cpu_clock: History,
    /// Package power in watts.
    pub cpu_power: History,
    /// The foreground application's frame rate, frame time and stutter rate.
    pub fps: History,
    pub frametime: History,
    pub stutter: History,
    /// Commit charge as a percentage of the commit limit.
    pub mem_commit: History,
    /// One per ACPI thermal zone, indexed like `Sensors::zones`.
    pub zone_temp: Vec<History>,
    /// Per *adapter*, not per sensor device: these are indexed like
    /// `Snapshot::gpus`, so an adapter with no sensor interface keeps an empty
    /// slot rather than shifting its neighbours.
    pub gpu_temp: Vec<History>,
    /// Junction temperature, where the device reports one.
    pub gpu_hotspot: Vec<History>,
    pub gpu_clock: Vec<History>,
    pub gpu_power: Vec<History>,
    pub gpu_fan: Vec<History>,
    /// Video memory in use, in bytes.
    pub gpu_mem: Vec<History>,
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// One complete, self-consistent view of the machine.
#[derive(Clone, Default)]
pub struct Snapshot {
    /// Monotonically increasing; lets the UI detect new data without comparing
    /// contents.
    pub generation: u64,
    /// Seconds covered by the rates in this snapshot.
    pub interval: f32,

    pub procs: Vec<Proc>,
    /// CSR child adjacency: `procs[i]`'s children are
    /// `child_links[child_start .. child_start + child_len]`.
    pub child_links: Vec<u32>,
    /// Indices of the processes nothing in this snapshot parents.
    ///
    /// The roots of the *parent-pid* tree, which seeds the traversal that fills
    /// `depth` and the rollups. Not the top level of the list: the shell parents
    /// every task the reader launched, so a list rooted here would file them all
    /// under Explorer. That is `group_head`.
    pub roots: Vec<u32>,

    pub cpu: CpuStats,
    pub mem: MemStats,
    pub disks: Vec<DiskStats>,
    pub nets: Vec<NetStats>,
    pub gpus: Vec<GpuStats>,
    pub sensors: Sensors,
    /// The foreground application's frame pacing, when it can be traced at all.
    pub frames: Frames,

    pub hist: Histories,

    /// Logical processor count.
    pub cpu_count: usize,
    /// Static CPU description, filled once at startup.
    pub cpu_name: Arc<str>,
    pub cpu_cores: u32,
    pub cpu_sockets: u32,
    pub cpu_base_mhz: u32,
    pub cpu_virtualization: bool,
}

impl Snapshot {
    /// Recycle an existing snapshot's allocations instead of freeing them.
    ///
    /// The sampler pushes a new snapshot every tick forever; without this the
    /// app would allocate and free the whole process table once a second.
    pub fn reset_for_reuse(&mut self) {
        self.procs.clear();
        self.child_links.clear();
        self.roots.clear();
        self.disks.clear();
        self.nets.clear();
        self.gpus.clear();
        self.cpu.per_core.clear();
        // The sensor probe clears these itself, but a snapshot handed back for
        // reuse should not carry a stale reading into the tick that fills it.
        self.sensors.zones.clear();
        self.sensors.gpus.clear();
    }

    #[inline]
    pub fn find_by_key(&self, key: ProcKey) -> Option<usize> {
        self.procs
            .iter()
            .position(|p| p.pid == key.pid && p.create_time == key.create_time)
    }

    #[inline]
    pub fn children_of(&self, idx: usize) -> &[u32] {
        let p = &self.procs[idx];
        let s = p.child_start as usize;
        &self.child_links[s..s + p.child_len as usize]
    }
}
