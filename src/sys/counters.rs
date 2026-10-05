//! System-wide CPU and memory counters, plus the static machine description
//! shown on the Performance tab.

use super::nt;
use super::snapshot::{CpuStats, MemStats};
use super::util::{reg_string, reg_u32};

use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;

// ---------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------

/// Tracks per-CPU tick counters across ticks so we can report rates.
pub struct CpuProbe {
    buf: nt::AlignedBuf,
    cur: Vec<nt::SystemProcessorPerformanceInformation>,
    prev: Vec<nt::SystemProcessorPerformanceInformation>,
    have_prev: bool,
}

impl CpuProbe {
    pub fn new() -> Self {
        Self {
            buf: nt::AlignedBuf::with_capacity(64 * std::mem::size_of::<
                nt::SystemProcessorPerformanceInformation,
            >()),
            cur: Vec::new(),
            prev: Vec::new(),
            have_prev: false,
        }
    }

    /// Sample the CPUs and write utilization into `out`.
    pub fn sample(&mut self, out: &mut CpuStats) {
        if !nt::query_cpu_times(&mut self.buf, &mut self.cur) {
            return;
        }

        out.per_core.clear();
        out.per_core.reserve(self.cur.len());

        if !self.have_prev || self.prev.len() != self.cur.len() {
            // First tick (or the CPU count changed under us): no delta to
            // report yet. Showing 0 is better than showing since-boot average.
            out.per_core.resize(self.cur.len(), 0.0);
            out.total = 0.0;
            out.kernel = 0.0;
        } else {
            let mut sum_busy = 0.0f64;
            let mut sum_total = 0.0f64;
            let mut sum_kernel = 0.0f64;

            for (c, p) in self.cur.iter().zip(self.prev.iter()) {
                // KernelTime includes IdleTime, so kernel+user is all time.
                let idle = (c.IdleTime - p.IdleTime) as f64;
                let kernel = (c.KernelTime - p.KernelTime) as f64;
                let user = (c.UserTime - p.UserTime) as f64;
                let total = kernel + user;

                let busy = (total - idle).max(0.0);
                let pct = if total > 0.0 {
                    (busy / total * 100.0) as f32
                } else {
                    0.0
                };
                out.per_core.push(pct.clamp(0.0, 100.0));

                sum_busy += busy;
                sum_total += total;
                sum_kernel += (kernel - idle).max(0.0);
            }

            out.total = if sum_total > 0.0 {
                ((sum_busy / sum_total * 100.0) as f32).clamp(0.0, 100.0)
            } else {
                0.0
            };
            out.kernel = if sum_total > 0.0 {
                ((sum_kernel / sum_total * 100.0) as f32).clamp(0.0, 100.0)
            } else {
                0.0
            };
        }

        std::mem::swap(&mut self.prev, &mut self.cur);
        self.have_prev = true;
    }
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// Sample system memory. Combines `GetPerformanceInfo` (totals, commit, pools)
/// with `SystemMemoryListInformation` (the free/modified/standby breakdown that
/// drives the composition bar).
pub fn sample_memory(out: &mut MemStats) {
    use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};

    let mut pi: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
    pi.cb = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
    if unsafe { GetPerformanceInfo(&mut pi, pi.cb) } == 0 {
        return;
    }

    let page = pi.PageSize as u64;
    out.total = pi.PhysicalTotal as u64 * page;
    out.available = pi.PhysicalAvailable as u64 * page;
    out.in_use = out.total.saturating_sub(out.available);
    out.committed = pi.CommitTotal as u64 * page;
    out.commit_limit = pi.CommitLimit as u64 * page;
    out.cached = pi.SystemCache as u64 * page;
    out.paged_pool = pi.KernelPaged as u64 * page;
    out.nonpaged_pool = pi.KernelNonpaged as u64 * page;

    // Optional: needs no privilege on current Windows, but degrade quietly if
    // the class is ever refused rather than showing a broken bar.
    if let Some(ml) = nt::query_memory_list() {
        out.free = (ml.FreePageCount as u64 + ml.ZeroPageCount as u64) * page;
        out.modified = ml.ModifiedPageCount as u64 * page;
        out.standby = ml.PageCountByPriority.iter().map(|&p| p as u64).sum::<u64>() * page;
    } else {
        // `cached` is close enough to standby+modified for the bar to be
        // meaningful when the detailed lists are unavailable.
        out.free = out.available.saturating_sub(out.cached);
        out.modified = 0;
        out.standby = out.cached;
    }
}

// ---------------------------------------------------------------------------
// Static machine description
// ---------------------------------------------------------------------------

pub struct CpuInfo {
    pub name: String,
    pub logical: u32,
    pub cores: u32,
    pub sockets: u32,
    pub base_mhz: u32,
    pub virtualization: bool,
    pub l1: u64,
    pub l2: u64,
    pub l3: u64,
}

/// Query the static CPU topology once at startup.
pub fn cpu_info() -> CpuInfo {
    use windows_sys::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationAll, CACHE_RELATIONSHIP, PROCESSOR_RELATIONSHIP,
        SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    use windows_sys::Win32::System::Threading::IsProcessorFeaturePresent;

    const KEY: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    let name = reg_string(HKEY_LOCAL_MACHINE, KEY, "ProcessorNameString")
        .unwrap_or_else(|| "Unknown processor".into());
    let base_mhz = reg_u32(HKEY_LOCAL_MACHINE, KEY, "~MHz").unwrap_or(0);

    // PF_VIRT_FIRMWARE_ENABLED reports whether VT-x/AMD-V is available *to us*,
    // so it goes false once Hyper-V (or WSL2, or Credential Guard) has claimed
    // it -- exactly when virtualization is most definitely enabled. Treating a
    // present hypervisor as equivalent matches what Task Manager displays.
    let virtualization = unsafe { IsProcessorFeaturePresent(21) } != 0 || hypervisor_present();

    let mut info = CpuInfo {
        name,
        logical: 0,
        cores: 0,
        sockets: 0,
        base_mhz,
        virtualization,
        l1: 0,
        l2: 0,
        l3: 0,
    };

    // Size probe, then a single walk of the variable-length record list.
    let mut len: u32 = 0;
    unsafe {
        GetLogicalProcessorInformationEx(RelationAll, std::ptr::null_mut(), &mut len);
    }
    if len == 0 {
        return info;
    }
    let mut buf = vec![0u8; len as usize];
    if unsafe { GetLogicalProcessorInformationEx(RelationAll, buf.as_mut_ptr().cast(), &mut len) }
        == 0
    {
        return info;
    }

    // Relation tags (SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX::Relationship).
    const REL_PROCESSOR_CORE: i32 = 0;
    const REL_CACHE: i32 = 2;
    const REL_PROCESSOR_PACKAGE: i32 = 3;

    let mut offset = 0usize;
    let valid = len as usize;
    while offset + 8 <= valid {
        let base = unsafe { buf.as_ptr().add(offset) };
        let rec: SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX =
            unsafe { std::ptr::read_unaligned(base.cast()) };
        let size = rec.Size as usize;
        if size == 0 || offset + size > valid {
            break;
        }

        match rec.Relationship {
            REL_PROCESSOR_CORE => {
                info.cores += 1;
                // Count set affinity bits to get logical processors per core.
                let pr: PROCESSOR_RELATIONSHIP =
                    unsafe { std::ptr::read_unaligned(base.add(8).cast()) };
                let groups = (pr.GroupCount as usize).max(1);
                for g in 0..groups {
                    let mask = unsafe {
                        std::ptr::read_unaligned(
                            base.add(8)
                                .add(std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupMask))
                                .add(g * std::mem::size_of::<
                                    windows_sys::Win32::System::SystemInformation::GROUP_AFFINITY,
                                >())
                                .cast::<windows_sys::Win32::System::SystemInformation::GROUP_AFFINITY>(),
                        )
                    };
                    info.logical += mask.Mask.count_ones();
                }
            }
            REL_PROCESSOR_PACKAGE => info.sockets += 1,
            REL_CACHE => {
                let cr: CACHE_RELATIONSHIP = unsafe { std::ptr::read_unaligned(base.add(8).cast()) };
                let bytes = cr.CacheSize as u64;
                match cr.Level {
                    1 => info.l1 += bytes,
                    2 => info.l2 += bytes,
                    3 => info.l3 = info.l3.max(bytes),
                    _ => {}
                }
            }
            _ => {}
        }
        offset += size;
    }

    if info.logical == 0 {
        info.logical = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);
    }
    info.sockets = info.sockets.max(1);
    info.cores = info.cores.max(1);
    info
}

/// Is a hypervisor running beneath us? CPUID leaf 1, ECX bit 31 is the
/// architecturally reserved "hypervisor present" hint.
fn hypervisor_present() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        // CPUID leaf 1 is available on every x86-64 processor.
        let r = std::arch::x86_64::__cpuid(1);
        r.ecx & (1 << 31) != 0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Seconds since boot.
pub fn uptime_secs() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    let ms = unsafe { GetTickCount64() };
    ms / 1000
}
