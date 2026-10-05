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
}

impl Category {
    /// Group-header text.
    pub fn label(self) -> &'static str {
        match self {
            Self::App => "Apps",
            Self::Background => "Background processes",
        }
    }

    /// Short form, for the Type column.
    pub fn type_label(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Background => "Background",
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
    /// True when this process heads its own entry in `group` rather than
    /// nesting under a parent: it either owns a window, or has no visible parent.
    pub group_head: bool,
    /// Title of this process's main window, when it owns a visible one.
    pub window_title: Option<Arc<str>>,

    // --- subtree rollups (sums over self + all descendants) --------------
    // Used for the collapsed-group rows, exactly like Task Manager: a folded
    // parent shows what its whole tree is doing.
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

#[derive(Clone, Default)]
pub struct GpuStats {
    pub name: Arc<str>,
    /// Busiest engine, 0..=100.
    pub utilization: f32,
    pub dedicated_used: u64,
    pub dedicated_total: u64,
    pub shared_used: u64,
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
}

/// Every graph's history, carried alongside the live values.
#[derive(Clone, Default)]
pub struct Histories {
    pub cpu: History,
    pub per_core: Vec<History>,
    pub mem: History,
    /// Per-disk active time, 0..=100.
    pub disk: Vec<History>,
    /// Per-disk combined read+write throughput in bytes/sec.
    pub disk_rate: Vec<History>,
    pub net_recv: Vec<History>,
    pub net_send: Vec<History>,
    pub gpu: Vec<History>,
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
    /// Indices of processes with no visible parent.
    pub roots: Vec<u32>,

    pub cpu: CpuStats,
    pub mem: MemStats,
    pub disks: Vec<DiskStats>,
    pub nets: Vec<NetStats>,
    pub gpus: Vec<GpuStats>,

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
