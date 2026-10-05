//! The sampler thread: reads the machine, builds a [`Snapshot`], publishes it.
//!
//! Everything expensive happens here, off the UI thread. Publication is a
//! single atomic pointer store via `ArcSwap`, so the UI never blocks and never
//! sees a half-built snapshot.

use super::counters::{self, CpuProbe};
use super::disk::DiskProbe;
use super::gpu::GpuProbe;
use super::meta::MetaStore;
use super::net::NetProbe;
use super::nt;
use super::snapshot::{Category, History, MemStats, Proc, ProcKey, RunState, Snapshot};
use super::windows_list::WindowList;

use arc_swap::ArcSwap;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Per-process state carried between ticks so we can report rates.
#[derive(Clone, Copy)]
struct Prev {
    create_time: i64,
    cpu_time: u64,
    kernel_time: u64,
    io_read: u64,
    io_write: u64,
    io_other: u64,
    hard_faults: u32,
    generation: u64,
}

/// Live handle the UI holds onto.
pub struct Monitor {
    current: Arc<ArcSwap<Snapshot>>,
    pub meta: Arc<MetaStore>,
    /// Sampling period in milliseconds; the sampler picks up changes on its
    /// next wake.
    interval_ms: Arc<AtomicU32>,
    /// Set to request the sampler take a sample immediately.
    wake: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Monitor {
    /// Load the most recent snapshot. Wait-free: one atomic load.
    #[inline]
    pub fn snapshot(&self) -> arc_swap::Guard<Arc<Snapshot>> {
        self.current.load()
    }

    pub fn set_interval(&self, ms: u32) {
        self.interval_ms.store(ms.clamp(250, 10_000), Ordering::Relaxed);
    }

    pub fn interval(&self) -> u32 {
        self.interval_ms.load(Ordering::Relaxed)
    }

    /// Ask for a fresh sample now instead of waiting out the period.
    ///
    /// Also takes one sample while paused, which is what a manual refresh
    /// should do.
    pub fn refresh_now(&self) {
        self.wake.store(true, Ordering::Release);
    }

    /// Stop sampling entirely. The last snapshot stays on display.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        if !paused {
            // Resuming should show fresh numbers immediately.
            self.refresh_now();
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Start sampling and invoke `notify` immediately after each publish.
///
/// The UI passes `egui::Context::request_repaint` here. That is what makes the
/// app event-driven: with no user input and no new sample there is nothing to
/// wake the UI thread, so it stays parked instead of polling for changes.
pub fn start_with_notify(
    interval_ms: u32,
    notify: impl Fn() + Send + 'static,
) -> Monitor {
    let current = Arc::new(ArcSwap::from_pointee(Snapshot::default()));
    let meta = MetaStore::new();
    let interval = Arc::new(AtomicU32::new(interval_ms));
    let wake = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));

    let monitor = Monitor {
        current: Arc::clone(&current),
        meta: Arc::clone(&meta),
        interval_ms: Arc::clone(&interval),
        wake: Arc::clone(&wake),
        paused: Arc::clone(&paused),
        stop: Arc::clone(&stop),
    };

    std::thread::Builder::new()
        .name("azt-sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::new(meta);
            let mut pool: Vec<Arc<Snapshot>> = Vec::with_capacity(4);

            while !stop.load(Ordering::Acquire) {
                // A manual refresh takes one sample even while paused; the flag
                // is cleared here rather than in the sleep loop so that request
                // survives long enough to be honoured.
                let manual = wake.swap(false, Ordering::AcqRel);
                if manual || !paused.load(Ordering::Relaxed) {
                    // Reuse a snapshot the UI has finished reading rather than
                    // reallocating the whole process table every tick.
                    let mut snap = take_reusable(&mut pool);
                    {
                        let s = Arc::get_mut(&mut snap).expect("exclusive by construction");
                        sampler.sample_into(s);
                    }
                    current.store(Arc::clone(&snap));
                    if pool.len() < 4 {
                        pool.push(snap);
                    }
                    #[cfg(feature = "memtrace")]
                    sampler.trace_mem(pool.len());
                    notify();
                }

                // Sleep in slices so an interval change or a manual refresh is
                // picked up promptly instead of after the full period.
                let period = Duration::from_millis(interval.load(Ordering::Relaxed) as u64);
                let deadline = Instant::now() + period;
                loop {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    // Left set for the top of the loop to consume.
                    if wake.load(Ordering::Acquire) {
                        break;
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    std::thread::sleep((deadline - now).min(Duration::from_millis(50)));
                }
            }
        })
        .expect("spawn sampler thread");

    monitor
}

/// Find a snapshot in the pool that no reader still references.
fn take_reusable(pool: &mut Vec<Arc<Snapshot>>) -> Arc<Snapshot> {
    if let Some(pos) = pool.iter().position(|a| Arc::strong_count(a) == 1) {
        return pool.swap_remove(pos);
    }
    Arc::new(Snapshot::default())
}

// ---------------------------------------------------------------------------
// Sampler
// ---------------------------------------------------------------------------

struct Sampler {
    meta: Arc<MetaStore>,
    buf: nt::AlignedBuf,
    prev: FxHashMap<u32, Prev>,
    live: FxHashSet<ProcKey>,
    cpu: CpuProbe,
    disk: DiskProbe,
    net: NetProbe,
    gpu: Option<GpuProbe>,
    sensors: super::sensors::SensorProbe,
    /// Absent when the trace session could not be started, which unelevated it never
    /// can; the status inside it says why.
    frames: super::frames::FrameMonitor,
    windows: WindowList,

    /// Carried across ticks; copied into each published snapshot.
    hist: super::snapshot::Histories,
    mem: MemStats,

    generation: u64,
    last_tick: Option<Instant>,
    cpu_info: counters::CpuInfo,
    page_size: u64,

    /// Scratch reused by the tree builder.
    pid_index: FxHashMap<u32, u32>,
    child_count: Vec<u32>,
    order: Vec<u32>,
    stack: Vec<u32>,
}

impl Sampler {
    fn new(meta: Arc<MetaStore>) -> Self {
        let cpu_info = counters::cpu_info();
        let page_size = nt::query_basic_info()
            .map(|b| b.PageSize as u64)
            .unwrap_or(4096);

        Self {
            meta,
            // Pre-size for a busy machine so the first tick does not resize.
            buf: nt::AlignedBuf::with_capacity(1 << 20),
            prev: FxHashMap::default(),
            live: FxHashSet::default(),
            frames: super::frames::FrameMonitor::start(),
            cpu: CpuProbe::new(),
            disk: DiskProbe::new(),
            net: NetProbe::new(),
            gpu: GpuProbe::new(),
            sensors: super::sensors::SensorProbe::new(),
            windows: WindowList::new(),
            hist: Default::default(),
            mem: MemStats::default(),
            generation: 0,
            last_tick: None,
            cpu_info,
            page_size,
            pid_index: FxHashMap::default(),
            child_count: Vec::new(),
            order: Vec::new(),
            stack: Vec::new(),
        }
    }

    /// Dump live heap bytes alongside the size of everything long-lived on this
    /// thread, so a growing container gives itself away.
    #[cfg(feature = "memtrace")]
    fn trace_mem(&self, pool_len: usize) {
        let (live, allocs) = crate::memtrace::live();
        let (frames, textures) = crate::memtrace::counters();
        let (meta, sids, files) = self.meta.debug_lens();
        let hists = self.hist.per_core.len()
            + self.hist.disk.len()
            + self.hist.disk_rate.len()
            + self.hist.net_recv.len()
            + self.hist.net_send.len()
            + self.hist.gpu.len();
        eprintln!(
            "[mem] live={:>8} KB allocs={:>11} frames={frames:>7} tex={textures:>6} \
             pool={pool_len} prev={} pid_index={} \
             order={} live_set={} meta={meta}/{sids}/{files} hists={hists} gpu_buf={} KB",
            live / 1024,
            allocs,
            self.prev.len(),
            self.pid_index.len(),
            self.order.len(),
            self.live.len(),
            self.gpu
                .as_ref()
                .map(|g| g.debug_buf_bytes() / 1024)
                .unwrap_or(0),
        );
    }

    fn sample_into(&mut self, out: &mut Snapshot) {
        out.reset_for_reuse();
        self.generation += 1;

        let now = Instant::now();
        let dt = self
            .last_tick
            .map(|t| (now - t).as_secs_f32())
            .unwrap_or(0.0);
        self.last_tick = Some(now);
        out.generation = self.generation;
        out.interval = dt;

        let trace = std::env::var_os("AZT_TRACE_SAMPLE").is_some();
        let mut mark = Instant::now();
        let mut timings: Vec<(&str, f32)> = Vec::new();
        macro_rules! phase {
            ($name:literal) => {
                if trace {
                    timings.push(($name, mark.elapsed().as_secs_f32() * 1000.0));
                    mark = Instant::now();
                }
            };
        }

        // --- system counters ---------------------------------------------
        self.cpu.sample(&mut out.cpu);
        phase!("cpu");
        counters::sample_memory(&mut self.mem);
        out.mem = self.mem.clone();
        phase!("mem");
        self.disk.sample(&mut out.disks);
        phase!("disk");
        self.net.sample(dt, &mut out.nets);
        phase!("net");
        if let Some(g) = self.gpu.as_mut() {
            g.sample(&mut out.gpus);
        }
        phase!("gpu");
        self.sensors.sample(&mut out.sensors);
        // Read before the process table is built, so the pid can be named from it.
        out.frames.status = self.frames.status();
        out.frames.pid = super::frames::foreground_pid();
        out.frames.stats = self.frames.stats(out.frames.pid);
        phase!("frames");
        // Pair each adapter with its vendor's readings once, here, so the UI and
        // the history buffers cannot disagree about which GPU a temperature
        // belongs to. Every backend enumerates only its own vendor's devices, in
        // the same PCI order DXGI does, so the nth adapter of a vendor is that
        // vendor's nth device.
        let mut nth: FxHashMap<u32, u32> = FxHashMap::default();
        for g in out.gpus.iter_mut() {
            let ordinal = nth.entry(g.vendor).or_insert(0);
            g.sensors = out
                .sensors
                .gpus
                .iter()
                .position(|s| s.vendor == g.vendor && s.ordinal == *ordinal)
                .map(|i| i as u32)
                .unwrap_or(super::snapshot::NO_SENSORS);
            *ordinal += 1;
        }
        phase!("sensors");

        out.cpu_count = out.cpu.per_core.len().max(self.cpu_info.logical as usize);
        out.cpu_name = Arc::from(self.cpu_info.name.as_str());
        out.cpu_cores = self.cpu_info.cores;
        out.cpu_sockets = self.cpu_info.sockets;
        out.cpu_base_mhz = self.cpu_info.base_mhz;
        out.cpu_virtualization = self.cpu_info.virtualization;
        out.cpu.uptime_secs = counters::uptime_secs();

        // --- window titles (for the Apps grouping) ------------------------
        self.windows.refresh();
        phase!("windows");

        // --- processes -----------------------------------------------------
        self.collect_processes(out, dt);
        phase!("procs");
        self.build_tree(out);
        self.assign_groups(out);
        self.rollup(out);
        phase!("tree");

        // Named from the table that was just built, rather than by opening the process
        // again: the pid is already in there with its window title resolved.
        out.frames.name = out
            .procs
            .iter()
            .find(|p| p.pid == out.frames.pid)
            .map(|p| Arc::clone(&p.name))
            .unwrap_or_else(|| Arc::from(""));

        // --- history --------------------------------------------------------
        self.push_history(out);
        out.hist = self.hist.clone();
        phase!("hist");

        if trace {
            // `mark` is intentionally left dangling after the last phase.
            let _ = mark;
            let total: f32 = timings.iter().map(|(_, ms)| *ms).sum();
            let detail: Vec<String> = timings
                .iter()
                .filter(|(_, ms)| *ms >= 0.05)
                .map(|(n, ms)| format!("{n}={ms:.2}"))
                .collect();
            eprintln!("[sample] total={total:.2}ms  {}", detail.join(" "));
        }
    }

    fn collect_processes(&mut self, out: &mut Snapshot, dt: f32) {
        let Ok(valid) = nt::query_system_info(nt::SYSTEM_PROCESS_INFORMATION, &mut self.buf) else {
            return;
        };

        let cpu_count = self.cpu_info.logical.max(1) as f64;
        // 100 ns CPU-time ticks available across all processors this interval.
        let cpu_capacity_100ns = (dt as f64) * 10_000_000.0 * cpu_count;

        self.live.clear();
        let mut total_threads = 0u32;
        let mut total_handles = 0u32;

        // Budget the cheap-metadata lookups so a startup burst (or a fork bomb)
        // cannot stall a tick. Anything left over is picked up next tick.
        let meta_deadline = Instant::now() + Duration::from_millis(15);
        let mut meta_budget_hit = false;

        for entry in nt::iter_processes(&self.buf, valid) {
            let info = &entry.info;
            let pid = info.UniqueProcessId as u32;

            // Skip the idle process. Its "CPU time" is the machine's *idle*
            // time, so including it would make the per-process CPU column sum
            // to 100% at all times and bury every real consumer. Task Manager
            // omits it for the same reason.
            if pid == 0 {
                continue;
            }

            let name: Arc<str> = if entry.image_name.is_empty() {
                // PID 4 has no image name; it is the kernel.
                if pid == 4 {
                    Arc::from("System")
                } else {
                    Arc::from("—")
                }
            } else {
                Arc::from(String::from_utf16_lossy(entry.image_name).as_str())
            };

            let create_time = info.CreateTime;
            let key = ProcKey { pid, create_time };
            self.live.insert(key);

            let kernel_time = info.KernelTime as u64;
            let cpu_time = kernel_time.wrapping_add(info.UserTime as u64);
            let io_read = info.ReadTransferCount as u64;
            let io_write = info.WriteTransferCount as u64;
            let io_other = info.OtherTransferCount as u64;

            // Rates, but only against a matching previous sample: a recycled PID
            // must not inherit the old process's counters.
            let mut cpu = 0.0f32;
            let mut cpu_kernel = 0.0f32;
            let mut io_read_bps = 0.0f32;
            let mut io_write_bps = 0.0f32;
            let mut disk_bps = 0.0f32;
            let mut hard_faults_per_s = 0.0f32;

            if let Some(p) = self.prev.get(&pid).copied() {
                if p.create_time == create_time && dt > 0.0 {
                    let d_cpu = cpu_time.saturating_sub(p.cpu_time) as f64;
                    let d_kernel = kernel_time.saturating_sub(p.kernel_time) as f64;
                    if cpu_capacity_100ns > 0.0 {
                        cpu = ((d_cpu / cpu_capacity_100ns) * 100.0).clamp(0.0, 100.0) as f32;
                        // Against the same capacity, so that the two are one figure
                        // and its part rather than two percentages of two things.
                        cpu_kernel =
                            ((d_kernel / cpu_capacity_100ns) * 100.0).clamp(0.0, 100.0) as f32;
                    }
                    let dr = io_read.saturating_sub(p.io_read) as f64;
                    let dw = io_write.saturating_sub(p.io_write) as f64;
                    let dro = io_other.saturating_sub(p.io_other) as f64;
                    io_read_bps = (dr / dt as f64) as f32;
                    io_write_bps = (dw / dt as f64) as f32;
                    disk_bps = ((dr + dw + dro) / dt as f64) as f32;
                    hard_faults_per_s =
                        info.HardFaultCount.saturating_sub(p.hard_faults) as f32 / dt;
                }
            }

            self.prev.insert(
                pid,
                Prev {
                    create_time,
                    cpu_time,
                    kernel_time,
                    io_read,
                    io_write,
                    io_other,
                    hard_faults: info.HardFaultCount,
                    generation: self.generation,
                },
            );

            // A process counts as suspended when it has threads and every one of
            // them is in a suspended wait -- this is how UWP apps look when the
            // shell freezes them.
            let state = if !entry.threads.is_empty()
                && entry.threads.iter().all(|t| {
                    t.ThreadState == nt::THREAD_STATE_WAIT
                        && t.WaitReason == nt::WAIT_REASON_SUSPENDED
                })
            {
                RunState::Suspended
            } else {
                RunState::Running
            };

            // Resolve the cheap metadata for processes seen for the first time,
            // which also queues the expensive stage. Nothing here consumes the
            // path any more -- categorisation stopped needing it when the
            // "Windows processes" group went away -- but the call has to happen
            // on this thread, since it is what discovers new processes at all.
            if !meta_budget_hit {
                if Instant::now() > meta_deadline {
                    meta_budget_hit = true;
                }
                self.meta.resolve_basic(key);
            }

            let window_title = self.windows.title_for(pid);
            let category = classify(
                pid,
                info.SessionId,
                window_title.is_some(),
                state == RunState::Suspended,
            );

            total_threads = total_threads.saturating_add(info.NumberOfThreads);
            total_handles = total_handles.saturating_add(info.HandleCount);

            out.procs.push(Proc {
                pid,
                ppid: info.InheritedFromUniqueProcessId as u32,
                create_time,
                name,
                cpu,
                cpu_kernel,
                disk_bps,
                io_read_bps,
                io_write_bps,
                hard_faults_per_s,
                private_ws: info.WorkingSetPrivateSize.max(0) as u64,
                working_set: info.WorkingSetSize as u64,
                peak_ws: info.PeakWorkingSetSize as u64,
                commit: info.PagefileUsage as u64,
                virtual_size: info.VirtualSize as u64,
                paged_pool: info.QuotaPagedPoolUsage as u64,
                nonpaged_pool: info.QuotaNonPagedPoolUsage as u64,
                threads: info.NumberOfThreads,
                handles: info.HandleCount,
                session: info.SessionId,
                base_priority: info.BasePriority,
                page_faults: info.PageFaultCount,
                cpu_time_100ns: cpu_time,
                cycle_time: info.CycleTime,
                io_read_total: io_read,
                io_write_total: io_write,
                io_other_total: io_other,
                read_ops: info.ReadOperationCount as u64,
                write_ops: info.WriteOperationCount as u64,
                other_ops: info.OtherOperationCount as u64,
                state,
                category,
                // Filled by `assign_groups` once the tree exists.
                group: category,
                group_head: true,
                window_title,
                sub_cpu: cpu,
                sub_private_ws: info.WorkingSetPrivateSize.max(0) as u64,
                sub_disk_bps: disk_bps,
                sub_count: 1,
                parent: u32::MAX,
                child_start: 0,
                child_len: 0,
                depth: 0,
            });
        }

        out.cpu.processes = out.procs.len() as u32;
        out.cpu.threads = total_threads;
        out.cpu.handles = total_handles;

        // Drop rate state for processes that have exited.
        let gen = self.generation;
        self.prev.retain(|_, p| p.generation == gen);
        self.meta.retain_live(&self.live);
        let _ = self.page_size;
    }

    /// Link parents to children and lay the adjacency out in CSR form.
    fn build_tree(&mut self, out: &mut Snapshot) {
        let n = out.procs.len();
        if n == 0 {
            return;
        }

        self.pid_index.clear();
        self.pid_index.reserve(n);
        for (i, p) in out.procs.iter().enumerate() {
            self.pid_index.insert(p.pid, i as u32);
        }

        // Resolve each process's parent.
        for i in 0..n {
            let (ppid, create_time) = (out.procs[i].ppid, out.procs[i].create_time);
            let mut parent = u32::MAX;
            if ppid != 0 || out.procs[i].pid != 0 {
                if let Some(&pi) = self.pid_index.get(&ppid) {
                    // Reject a "parent" that started after its child: that PID
                    // has been recycled and the real parent is gone.
                    if pi as usize != i && out.procs[pi as usize].create_time <= create_time {
                        parent = pi;
                    }
                }
            }
            out.procs[i].parent = parent;
        }

        // Break any cycle a create-time tie could still allow, so the DFS below
        // is guaranteed to terminate.
        self.order.clear();
        self.child_count.clear();
        self.child_count.resize(n, 0);
        let mut visited = vec![false; n];
        for i in 0..n {
            // Walk up at most n steps; if we are still moving we are in a cycle.
            let mut steps = 0usize;
            let mut cur = out.procs[i].parent;
            while cur != u32::MAX && steps <= n {
                if cur as usize == i {
                    out.procs[i].parent = u32::MAX;
                    break;
                }
                cur = out.procs[cur as usize].parent;
                steps += 1;
            }
            if steps > n {
                out.procs[i].parent = u32::MAX;
            }
        }

        for i in 0..n {
            let p = out.procs[i].parent;
            if p == u32::MAX {
                out.roots.push(i as u32);
            } else {
                self.child_count[p as usize] += 1;
            }
        }

        // Prefix sum -> child_start, then fill.
        let mut acc = 0u32;
        for i in 0..n {
            out.procs[i].child_start = acc;
            out.procs[i].child_len = 0;
            acc += self.child_count[i];
        }
        out.child_links.resize(acc as usize, 0);
        for i in 0..n {
            let p = out.procs[i].parent;
            if p != u32::MAX {
                let parent = &mut out.procs[p as usize];
                let slot = (parent.child_start + parent.child_len) as usize;
                parent.child_len += 1;
                out.child_links[slot] = i as u32;
            }
        }

        // Assign depth and record a pre-order traversal for the rollup pass.
        self.stack.clear();
        // Push roots in reverse so the traversal comes out in root order.
        for &r in out.roots.iter().rev() {
            self.stack.push(r);
        }
        while let Some(i) = self.stack.pop() {
            let idx = i as usize;
            if visited[idx] {
                continue;
            }
            visited[idx] = true;
            self.order.push(i);

            let p = out.procs[idx].parent;
            out.procs[idx].depth = if p == u32::MAX {
                0
            } else {
                out.procs[p as usize].depth.saturating_add(1)
            };

            let (s, l) = (
                out.procs[idx].child_start as usize,
                out.procs[idx].child_len as usize,
            );
            for k in (0..l).rev() {
                self.stack.push(out.child_links[s + k]);
            }
        }
    }

    /// Decide which top-level group each process is displayed under.
    ///
    /// Task Manager assigns a whole process tree to one group rather than
    /// scattering it: a browser's renderer children are background processes on
    /// their own merits, yet they belong *under the browser* in Apps. Grouping
    /// each process by its own category instead left a windowed app looking
    /// childless while its helpers turned up separately under Background.
    ///
    /// A process heads its own group entry when it owns a window (so an app
    /// launched from a shell still appears in Apps), when it has no visible
    /// parent, or when the process that started it is a [launcher](is_launcher)
    /// rather than an owner. Everything else inherits its parent's group.
    fn assign_groups(&mut self, out: &mut Snapshot) {
        let shell = self.windows.shell();

        // `order` is pre-order, so a parent's group is already final here.
        for &i in &self.order {
            let idx = i as usize;
            let parent = out.procs[idx].parent;
            let own = out.procs[idx].category;

            // Apps and suspended processes head their own entries; a background
            // process nests under whichever of those started it, unless that was a
            // launcher. A suspended child of a running app moves to the Suspended
            // section with its own subtree, which is the point of the section.
            let heads = if parent == u32::MAX {
                true
            } else {
                let p = &out.procs[parent as usize];
                own != Category::Background || is_launcher(p.pid, p.session, shell)
            };
            out.procs[idx].group_head = heads;
            out.procs[idx].group = if heads {
                own
            } else {
                out.procs[parent as usize].group
            };
        }
    }

    /// Sum each subtree into its root so a collapsed row shows the whole group.
    ///
    /// Along the edges the *list* draws, not every edge in the tree: a process that
    /// heads its own entry is a row in its own right beside its launcher, so folding
    /// the launcher must not claim its load. Otherwise the shell would carry the CPU
    /// of everything the reader ever started -- and carry it into the sort key, which
    /// is these totals, putting a row that displays 0.2% at the top of a list ordered
    /// by CPU. Each total is complete before its edge is skipped, so a detached head
    /// still reports its own subtree.
    fn rollup(&mut self, out: &mut Snapshot) {
        // `order` is pre-order, so walking it backwards visits every child
        // before its parent.
        for &i in self.order.iter().rev() {
            let idx = i as usize;
            let parent = out.procs[idx].parent;
            if parent == u32::MAX || out.procs[idx].group_head {
                continue;
            }
            let (c, m, d, n) = {
                let p = &out.procs[idx];
                (p.sub_cpu, p.sub_private_ws, p.sub_disk_bps, p.sub_count)
            };
            let pp = &mut out.procs[parent as usize];
            pp.sub_cpu += c;
            pp.sub_private_ws += m;
            pp.sub_disk_bps += d;
            pp.sub_count += n;
        }
    }

    fn push_history(&mut self, out: &Snapshot) {
        self.hist.cpu.push(out.cpu.total);
        self.hist.cpu_kernel.push(out.cpu.kernel);
        self.hist.mem.push(out.mem.percent());

        resize_hist(&mut self.hist.per_core, out.cpu.per_core.len());
        for (h, &v) in self.hist.per_core.iter_mut().zip(out.cpu.per_core.iter()) {
            h.push(v);
        }

        resize_hist(&mut self.hist.disk, out.disks.len());
        resize_hist(&mut self.hist.disk_rate, out.disks.len());
        for (h, d) in self.hist.disk.iter_mut().zip(out.disks.iter()) {
            h.push(d.active_pct);
        }
        for (h, d) in self.hist.disk_rate.iter_mut().zip(out.disks.iter()) {
            h.push(d.read_bps + d.write_bps);
        }

        resize_hist(&mut self.hist.net_recv, out.nets.len());
        resize_hist(&mut self.hist.net_send, out.nets.len());
        for (h, nt) in self.hist.net_recv.iter_mut().zip(out.nets.iter()) {
            h.push(nt.recv_bps);
        }
        for (h, nt) in self.hist.net_send.iter_mut().zip(out.nets.iter()) {
            h.push(nt.send_bps);
        }

        resize_hist(&mut self.hist.gpu, out.gpus.len());
        for (h, g) in self.hist.gpu.iter_mut().zip(out.gpus.iter()) {
            h.push(g.utilization);
        }

        // Sensors. A reading the hardware does not have pushes a zero: the graph
        // has to advance for every other series to stay aligned in time, and the
        // tile above it shows "—" rather than the flat line.
        self.hist
            .cpu_temp
            .push(out.sensors.cpu_temp_c.unwrap_or(0.0));
        self.hist.cpu_clock.push(out.sensors.cpu_mhz as f32);
        self.hist
            .cpu_power
            .push(out.sensors.cpu_power_w.unwrap_or(0.0));
        // Frame pacing follows whatever is in front, so the process it is about can
        // change between one tick and the next. The histories are of *the foreground*
        // rather than of one application, which is what a reader watching a game while
        // alt-tabbing to look at this expects.
        self.hist.fps.push(out.frames.stats.map_or(0.0, |s| s.fps));
        self.hist
            .frametime
            .push(out.frames.stats.map_or(0.0, |s| s.avg_ms));
        self.hist
            .stutter
            .push(out.frames.stats.map_or(0.0, |s| s.stutter_pct));
        self.hist.mem_commit.push(if out.mem.commit_limit > 0 {
            out.mem.committed as f32 / out.mem.commit_limit as f32 * 100.0
        } else {
            0.0
        });

        resize_hist(&mut self.hist.zone_temp, out.sensors.zones.len());
        for (h, z) in self.hist.zone_temp.iter_mut().zip(out.sensors.zones.iter()) {
            h.push(z.celsius);
        }

        for h in [
            &mut self.hist.gpu_temp,
            &mut self.hist.gpu_hotspot,
            &mut self.hist.gpu_clock,
            &mut self.hist.gpu_power,
            &mut self.hist.gpu_fan,
            &mut self.hist.gpu_mem,
        ] {
            resize_hist(h, out.gpus.len());
        }
        for (i, g) in out.gpus.iter().enumerate() {
            let s = out.sensors.gpus.get(g.sensors as usize);
            let take = |v: Option<f32>| v.unwrap_or(0.0);
            self.hist.gpu_temp[i].push(take(s.and_then(|s| s.temp_c)));
            self.hist.gpu_hotspot[i].push(take(s.and_then(|s| s.hotspot_c)));
            self.hist.gpu_clock[i].push(take(s.and_then(|s| s.core_mhz.map(|m| m as f32))));
            self.hist.gpu_power[i].push(take(s.and_then(|s| s.power_w)));
            self.hist.gpu_fan[i].push(take(s.and_then(|s| s.fan_pct)));
            // Whichever pool this adapter actually uses -- an integrated one has
            // no dedicated memory, and its allocation is in system RAM.
            let vram = match s {
                Some(s) if s.vram_used > 0 && g.shared_used <= g.dedicated_used => s.vram_used,
                _ if g.shared_used > g.dedicated_used => g.shared_used,
                _ => g.dedicated_used,
            };
            self.hist.gpu_mem[i].push(vram as f32);
        }
    }
}

fn resize_hist(v: &mut Vec<History>, n: usize) {
    if v.len() != n {
        v.resize_with(n, History::default);
    }
}

/// App, background, or suspended.
///
/// Owning a visible top-level window is what makes a process an app; everything
/// else is background. The image's location used to matter here, to split
/// "Windows processes" out of "Background processes", and no longer does.
///
/// Being suspended outranks both, including for a process that owns a window: a
/// frozen app is not one of the things using the machine, and leaving it in Apps
/// means a reader looking for what is busy has to read past it.
fn classify(pid: u32, session: u32, has_window: bool, suspended: bool) -> Category {
    // PID 0 and 4 are the idle process and the kernel. Session 0 is the
    // services session, so never a user-facing app however it looks.
    if pid == 0 || pid == 4 || session == 0 {
        // ...but a suspended service host is still worth setting aside.
        return if suspended {
            Category::Suspended
        } else {
            Category::Background
        };
    }
    if suspended {
        Category::Suspended
    } else if has_window {
        Category::App
    } else {
        Category::Background
    }
}

/// Did this process merely *start* the ones below it, rather than own them?
///
/// `InheritedFromUniqueProcessId` records who called `CreateProcess`, and on Windows
/// that is usually a launcher acting on someone else's behalf: the shell starts
/// everything the reader double-clicks, the service manager starts every service,
/// `wininit` starts the service manager, and the DCOM launcher starts every COM and
/// UWP activation -- `RuntimeBroker`, `dllhost`, `WmiPrvSE`. It is not even reliably
/// "who started me": a caller can name any process as the parent of its child with
/// `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`.
///
/// So a tree drawn straight from the parent field files most of the machine under
/// two or three nodes that say nothing about it -- here, 170 processes under the
/// service manager and 41 under one `svchost`. Task Manager does not: its rows come
/// from *task* identity (a window, a package, a service the SCM reports), which is
/// why the children of `explorer.exe`, `wininit.exe` and `services.exe` sit at the
/// same level as their launcher rather than under it. This is that rule, expressed
/// as the two tests that make it hold without a list of image names:
///
/// - **Session 0 has no user in it.** Everything there is infrastructure, so a
///   session-0 parent is a launcher whatever its image -- and that covers the
///   service manager, `wininit`, the COM surrogate hosts, and the kernel.
/// - **The shell** is identified by pid, from `GetShellWindow` -- see
///   [`shell`](super::windows_list::WindowList::shell).
///
/// Ownership survives both: a browser, an editor or a terminal in a user session is
/// none of these, so its helpers still nest under it, which is what the grouped list
/// is for.
fn is_launcher(pid: u32, session: u32, shell: u32) -> bool {
    session == 0 || (shell != 0 && pid == shell)
}

#[cfg(test)]
mod tests {
    use super::is_launcher;

    /// The two tests that stand in for Task Manager's list of "Windows processes".
    #[test]
    fn a_launcher_is_the_shell_or_anything_in_session_0() {
        const SHELL: u32 = 4321;

        // The service manager, `wininit`, a COM surrogate host: session 0 is
        // infrastructure whatever pid it wears.
        assert!(is_launcher(2468, 0, SHELL));
        assert!(is_launcher(4, 0, SHELL), "the kernel parents smss");

        // The shell, by pid rather than by image name.
        assert!(is_launcher(SHELL, 1, SHELL));

        // An ordinary process in a user session owns what it starts: a browser's
        // renderers and an editor's language servers belong under them.
        assert!(!is_launcher(1234, 1, SHELL));

        // No shell reported -- a session that has none, or one whose shell has just
        // died. Nothing matches, rather than everything matching pid 0.
        assert!(!is_launcher(0, 1, 0));
        assert!(!is_launcher(SHELL, 1, 0));
    }
}
