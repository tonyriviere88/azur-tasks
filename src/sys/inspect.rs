//! What one process is made of: its loaded modules, its open handles, its threads.
//!
//! Everything here is *on demand* and off the UI thread, which is the whole design
//! of the module. The sampler's job is the machine, once a second, in a few
//! milliseconds; none of these three lists belongs in it:
//!
//! - **Handles** have no per-process query at all. `SystemExtendedHandleInformation`
//!   returns the entire machine's handle table -- tens of megabytes on a busy box,
//!   tens of milliseconds -- and the answer is then filtered down to one pid.
//! - **Modules** need the process opened for `PROCESS_VM_READ`, one open per look.
//! - **Names** for handles need each one duplicated into this process and asked,
//!   and one of those calls can block for as long as the kernel feels like it.
//!
//! So a worker thread owns the buffers, answers one question at a time, and
//! publishes the result through an [`ArcSwapOption`] the way the sampler publishes
//! a [`Snapshot`](super::snapshot::Snapshot). A request that arrives while another
//! is being answered replaces it -- the reader only ever wants the newest answer,
//! and a queue would make a held-down arrow key into a backlog.

use super::nt::{self, AlignedBuf};
use super::util::{from_wide_nul, Handle};
use arc_swap::ArcSwapOption;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{
    DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, HMODULE, MAX_PATH,
};
use windows_sys::Win32::System::ProcessStatus::{
    EnumProcessModulesEx, GetModuleFileNameExW, GetModuleInformation, LIST_MODULES_ALL, MODULEINFO,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenThread, PROCESS_DUP_HANDLE, PROCESS_QUERY_INFORMATION,
    PROCESS_VM_READ, THREAD_QUERY_INFORMATION,
};

/// Which of the three lists a request is for.
///
/// One at a time on purpose: only one is on screen, and the handle table costs
/// more than the other two together.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Modules,
    Handles,
    Threads,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Modules => "Modules",
            Self::Handles => "Handles",
            Self::Threads => "Threads",
        }
    }
}

/// A DLL (or the image itself) mapped into the process.
#[derive(Clone)]
pub struct Module {
    pub base: usize,
    pub size: u32,
    /// File name only, for the Name column.
    pub name: Arc<str>,
    /// Full path, for the Path column and the context menu.
    pub path: Arc<str>,
}

/// One entry of the process's handle table.
#[derive(Clone)]
pub struct OpenHandle {
    /// The handle value as the owning process sees it.
    pub value: u32,
    /// `File`, `Key`, `Event`, `Mutant`, ... from the kernel's own type table.
    pub kind: Arc<str>,
    pub access: u32,
    /// How many handles onto this object exist machine-wide. A hundred rows all
    /// reading 1 is a leak of distinct objects; one row reading 4 000 is a leak of
    /// handles onto one.
    pub refs: usize,
    /// What it refers to, when that could be asked safely. See [`name_is_safe`].
    pub name: Option<Arc<str>>,
}

/// One thread of the process.
#[derive(Clone)]
pub struct ThreadInfo {
    pub tid: u32,
    /// `ntdll.dll+0x1a2b3`, or the bare address when no module owns it. The
    /// address itself is not kept: unresolved, this *is* the address, and resolved,
    /// the module and the offset are what a reader can act on.
    pub start_label: Arc<str>,
    pub priority: i32,
    pub base_priority: i32,
    /// `Running`, `Waiting: UserRequest`, ...
    pub state: Arc<str>,
    /// Kernel + user time, in 100 ns ticks.
    pub cpu_100ns: i64,
    pub created: i64,
    pub switches: u32,
}

/// The rows of whichever list was asked for.
#[derive(Clone)]
pub enum Rows {
    Modules(Vec<Module>),
    Handles(Vec<OpenHandle>),
    Threads(Vec<ThreadInfo>),
}

impl Rows {
    pub fn len(&self) -> usize {
        match self {
            Self::Modules(v) => v.len(),
            Self::Handles(v) => v.len(),
            Self::Threads(v) => v.len(),
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Self::Modules(_) => Kind::Modules,
            Self::Handles(_) => Kind::Handles,
            Self::Threads(_) => Kind::Threads,
        }
    }
}

/// One answer: the rows, and what stopped them from being complete.
pub struct Inspection {
    pub pid: u32,
    pub rows: Rows,
    /// The process refused to open, so the list is empty for a reason worth saying
    /// out loud rather than showing as "no modules".
    pub denied: bool,
    /// Handles only: names were not readable because `PROCESS_DUP_HANDLE` was not
    /// granted. The types and values are still the kernel's own.
    pub names_denied: bool,
}

impl Inspection {
    pub fn kind(&self) -> Kind {
        self.rows.kind()
    }
}

/// The worker's side of one request.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Request {
    pid: u32,
    kind: Kind,
}

/// Ask about a process; read the newest answer.
///
/// Cheap to clone-free: the UI holds one and calls [`Inspector::request`] whenever
/// the question changes -- a different process, a different tab, a new sample.
pub struct Inspector {
    tx: Sender<Request>,
    latest: Arc<ArcSwapOption<Inspection>>,
}

impl Inspector {
    /// Spawn the worker. `notify` is called on the worker thread every time an
    /// answer is published, so the UI can ask for a repaint.
    pub fn start(notify: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let latest: Arc<ArcSwapOption<Inspection>> = Arc::new(ArcSwapOption::empty());
        let published = Arc::clone(&latest);
        std::thread::Builder::new()
            .name("aztm-inspect".into())
            .spawn(move || worker(rx, published, notify))
            .expect("spawn inspector worker");
        Self { tx, latest }
    }

    /// Queue a look at `pid`'s `kind` list. Superseded silently if another arrives
    /// before the worker gets to it.
    pub fn request(&self, pid: u32, kind: Kind) {
        // The worker outlives the UI, so a send error means shutdown is under way
        // and there is nothing useful to do about it.
        let _ = self.tx.send(Request { pid, kind });
    }

    /// The newest answer, whatever question it was for -- the caller checks the pid
    /// and the kind against what it is drawing.
    pub fn latest(&self) -> Option<Arc<Inspection>> {
        self.latest.load_full()
    }
}

fn worker(
    rx: Receiver<Request>,
    latest: Arc<ArcSwapOption<Inspection>>,
    notify: impl Fn() + Send + 'static,
) {
    // Reused across every answer: the handle table is the reason this module has a
    // thread of its own, and re-allocating megabytes per look would undo it.
    let mut buf = AlignedBuf::with_capacity(256 * 1024);
    let mut names = AlignedBuf::with_capacity(2048);
    // The kernel's type table. Fixed for the life of the boot, so it is read once.
    let mut types: Option<Vec<Arc<str>>> = None;

    while let Ok(first) = rx.recv() {
        // Only the newest question matters: an arrow key held down on the process
        // list would otherwise queue one full handle-table read per row it passed.
        let mut req = first;
        while let Ok(next) = rx.try_recv() {
            req = next;
        }

        let answer = match req.kind {
            Kind::Modules => {
                let (mods, denied) = modules(req.pid);
                Inspection {
                    pid: req.pid,
                    rows: Rows::Modules(mods),
                    denied,
                    names_denied: false,
                }
            }
            Kind::Threads => {
                let rows = threads(req.pid, &mut buf);
                Inspection {
                    pid: req.pid,
                    // A process with no threads is a process that has exited, which
                    // is the same story as one that would not open.
                    denied: rows.is_empty(),
                    rows: Rows::Threads(rows),
                    names_denied: false,
                }
            }
            Kind::Handles => {
                let table = types.get_or_insert_with(|| object_types(&mut names));
                let (rows, denied, names_denied) =
                    handles(req.pid, table, &mut buf, &mut names);
                Inspection {
                    pid: req.pid,
                    rows: Rows::Handles(rows),
                    denied,
                    names_denied,
                }
            }
        };

        latest.store(Some(Arc::new(answer)));
        notify();
    }
}

// ---------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------

/// Every image mapped into the process, largest concern first: the executable
/// itself is module zero, and the rest follow in load order.
///
/// Returns `(modules, denied)`. `PROCESS_VM_READ` is the price of the module list
/// -- `EnumProcessModulesEx` reads the loader data out of the target's own address
/// space -- and it is exactly the right a protected process denies, so an empty
/// list with `denied` set is the ordinary answer for anything Antimalware owns.
fn modules(pid: u32) -> (Vec<Module>, bool) {
    let Some(h) = Handle::new(unsafe {
        OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid)
    }) else {
        return (Vec::new(), true);
    };

    // Two passes: ask how many, then take them. A process can load a DLL between
    // the two, so the second pass is bounded by what was actually written.
    let mut needed: u32 = 0;
    let mut list: Vec<HMODULE> = vec![std::ptr::null_mut(); 512];
    loop {
        let bytes = (list.len() * std::mem::size_of::<HMODULE>()) as u32;
        let ok = unsafe {
            EnumProcessModulesEx(h.raw(), list.as_mut_ptr(), bytes, &mut needed, LIST_MODULES_ALL)
        };
        if ok == 0 {
            return (Vec::new(), true);
        }
        if needed <= bytes {
            list.truncate(needed as usize / std::mem::size_of::<HMODULE>());
            break;
        }
        // Grown by what the kernel asked for plus room for a few more arrivals.
        list.resize(needed as usize / std::mem::size_of::<HMODULE>() + 32, std::ptr::null_mut());
    }

    let mut out = Vec::with_capacity(list.len());
    let mut wide = [0u16; MAX_PATH as usize];
    for m in list {
        let mut info = MODULEINFO {
            lpBaseOfDll: std::ptr::null_mut(),
            SizeOfImage: 0,
            EntryPoint: std::ptr::null_mut(),
        };
        let ok = unsafe {
            GetModuleInformation(
                h.raw(),
                m,
                &mut info,
                std::mem::size_of::<MODULEINFO>() as u32,
            )
        };
        // A module unloaded between the enumeration and here: skip it rather than
        // reporting a row of zeroes.
        if ok == 0 {
            continue;
        }
        let n = unsafe { GetModuleFileNameExW(h.raw(), m, wide.as_mut_ptr(), wide.len() as u32) };
        let path = if n == 0 {
            String::new()
        } else {
            from_wide_nul(&wide[..n as usize])
        };
        let name = path
            .rsplit(['\\', '/'])
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("0x{:x}", info.lpBaseOfDll as usize));
        out.push(Module {
            base: info.lpBaseOfDll as usize,
            size: info.SizeOfImage,
            name: Arc::from(name.as_str()),
            path: Arc::from(path.as_str()),
        });
    }
    (out, false)
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

/// The process's threads, from the same system call the sampler uses.
///
/// Not from `CreateToolhelp32Snapshot`, which walks every process in the machine
/// to hand back thread ids and nothing else: `SYSTEM_PROCESS_INFORMATION` carries
/// the thread array inline after each process, with the state, the wait reason,
/// the priorities, the switch count and the start address already in it.
///
/// The start address is then labelled against the module list, so a row says
/// `ntdll.dll+0x1a2b3` rather than a bare pointer -- which is the difference
/// between "a thread" and "a thread pool worker". No symbols are involved; the
/// module is as far as an address alone can be resolved.
fn threads(pid: u32, buf: &mut AlignedBuf) -> Vec<ThreadInfo> {
    let Ok(valid) = nt::query_system_info(nt::SYSTEM_PROCESS_INFORMATION, buf) else {
        return Vec::new();
    };
    let Some(entry) = nt::iter_processes(buf, valid).find(|e| e.info.UniqueProcessId as u32 == pid)
    else {
        return Vec::new();
    };

    // Sorted by base so the containing module is a binary search per thread rather
    // than a scan. Denied is fine here: the labels fall back to the address.
    let mut mods = modules(pid).0;
    mods.sort_unstable_by_key(|m| m.base);

    entry
        .threads
        .iter()
        .map(|t| {
            let tid = t.UniqueThread as u32;
            // `StartAddress` from the system call is zero unless we hold
            // `SeDebugPrivilege`, so it is only the fallback -- see
            // `nt::query_thread_start`, which is where the address comes from.
            let start = thread_start(tid).unwrap_or(t.StartAddress as usize);
            ThreadInfo {
                tid,
                start_label: Arc::from(label_address(start, &mods).as_str()),
                priority: t.Priority,
                base_priority: t.BasePriority,
                state: Arc::from(thread_state(t.ThreadState, t.WaitReason)),
                cpu_100ns: t.KernelTime + t.UserTime,
                created: t.CreateTime,
                switches: t.ContextSwitches,
            }
        })
        .collect()
}

/// A thread's Win32 entry point, or `None` when it would not open.
///
/// One open and one query per thread. That is the only way to get this at medium
/// integrity, and it is cheap next to the module enumeration the label needs
/// anyway; a thread that exits between the enumeration and here simply has no
/// address, which the label renders as the bare zero.
fn thread_start(tid: u32) -> Option<usize> {
    // `THREAD_QUERY_INFORMATION`, not the `_LIMITED_` one: measured, the limited
    // right opens the thread perfectly happily and then answers
    // `STATUS_ACCESS_DENIED` to this class, which is a failure mode that looks
    // like every start address in the machine being zero.
    let h = Handle::new(unsafe { OpenThread(THREAD_QUERY_INFORMATION, 0, tid) })?;
    unsafe { nt::query_thread_start(h.raw()) }
}

/// `module.dll+0xoffset` for an address inside a mapped image, else the address.
///
/// `mods` must be sorted by `base`.
fn label_address(addr: usize, mods: &[Module]) -> String {
    if addr == 0 {
        return "—".to_owned();
    }
    // The last module starting at or below the address is the only candidate.
    let idx = mods.partition_point(|m| m.base <= addr);
    if let Some(m) = idx.checked_sub(1).map(|i| &mods[i]) {
        if addr < m.base + m.size as usize {
            let off = addr - m.base;
            return if off == 0 {
                m.name.to_string()
            } else {
                format!("{}+0x{off:x}", m.name)
            };
        }
    }
    format!("0x{addr:x}")
}

/// `KTHREAD_STATE`, and for a waiting thread the `KWAIT_REASON` under it.
///
/// The reason is the useful half: every idle thread in the machine is `Waiting`,
/// and what separates a thread pool worker parked on its queue from one blocked on
/// a lock is `WrQueue` against `WrPushLock`.
fn thread_state(state: u32, reason: u32) -> String {
    let name = match state {
        0 => "Initialized",
        1 => "Ready",
        2 => "Running",
        3 => "Standby",
        4 => "Terminated",
        nt::THREAD_STATE_WAIT => "Waiting",
        6 => "Transition",
        7 => "Deferred ready",
        _ => "Unknown",
    };
    if state != nt::THREAD_STATE_WAIT {
        return name.to_owned();
    }
    // The reasons a UI thread and a worker thread actually park on. The rest are
    // rare enough that the number is more honest than a guessed label.
    let why = match reason {
        0 => "Executive",
        1 => "FreePage",
        2 => "PageIn",
        3 => "PoolAllocation",
        4 => "DelayExecution",
        nt::WAIT_REASON_SUSPENDED => "Suspended",
        6 => "UserRequest",
        7 => "WrExecutive",
        13 => "WrUserRequest",
        15 => "WrQueue",
        16 => "WrLpcReceive",
        17 => "WrLpcReply",
        21 => "WrKeyedEvent",
        22 => "WrTerminated",
        27 => "WrResource",
        28 => "WrPushLock",
        29 => "WrMutex",
        30 => "WrQuantumEnd",
        32 => "WrPreempted",
        34 => "WrFastMutex",
        35 => "WrGuardedMutex",
        37 => "WrAlertByThreadId",
        _ => return format!("Waiting: {reason}"),
    };
    format!("Waiting: {why}")
}

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

/// The kernel's object type table, indexed by the `ObjectTypeIndex` the handle
/// table reports.
///
/// One call instead of one duplication-and-query per handle: `ObjectTypesInformation`
/// hands back every type there is, each carrying its own index, and the answer is
/// the same for the life of the boot.
///
/// The walk is the fiddly part. Entries are not an array -- each type's name
/// buffer follows its struct inline, and the next entry starts at the next
/// pointer-aligned address after it.
fn object_types(buf: &mut AlignedBuf) -> Vec<Arc<str>> {
    let mut out: Vec<Arc<str>> = Vec::new();
    // Any handle this process owns works as the subject; the class is about the
    // machine, not about the object.
    let me = unsafe { GetCurrentProcess() };
    let Ok(valid) = (unsafe { nt::query_object(me, nt::OBJECT_TYPES_INFORMATION, buf) }) else {
        return out;
    };
    if valid < 8 {
        return out;
    }
    let base = buf.as_ptr();
    let count = unsafe { std::ptr::read_unaligned(base.cast::<u32>()) } as usize;
    // `OBJECT_TYPES_INFORMATION` is one `ULONG` followed by the first entry at the
    // next pointer boundary.
    let mut off = align_up(std::mem::size_of::<u32>(), std::mem::size_of::<usize>());
    for _ in 0..count.min(4096) {
        if off + std::mem::size_of::<nt::ObjectTypeInformation>() > valid {
            break;
        }
        let info: nt::ObjectTypeInformation =
            unsafe { std::ptr::read_unaligned(base.add(off).cast()) };
        let chars = info.TypeName.Length as usize / 2;
        let name = if info.TypeName.Buffer.is_null() || chars == 0 {
            String::new()
        } else {
            // The buffer must lie inside what the kernel just wrote.
            let start = info.TypeName.Buffer as usize;
            let lo = base as usize;
            if start >= lo && start + chars * 2 <= lo + valid {
                String::from_utf16_lossy(unsafe {
                    std::slice::from_raw_parts(info.TypeName.Buffer, chars)
                })
            } else {
                String::new()
            }
        };
        let idx = info.TypeIndex as usize;
        if out.len() <= idx {
            out.resize(idx + 1, Arc::from(""));
        }
        out[idx] = Arc::from(name.as_str());

        // Struct, then its name bytes, then round up to a pointer.
        off += std::mem::size_of::<nt::ObjectTypeInformation>();
        off = align_up(
            off + info.TypeName.MaximumLength as usize,
            std::mem::size_of::<usize>(),
        );
    }
    out
}

fn align_up(v: usize, to: usize) -> usize {
    (v + to - 1) & !(to - 1)
}

/// How many handles one process may have named in a single look.
///
/// A handle leak is exactly the thing this list exists to show, and a process with
/// 300 000 handles would otherwise mean 300 000 duplications before the panel drew
/// anything. The rows above the cap still appear -- they just keep the type and the
/// value the handle table gave them, with no name.
const NAME_BUDGET: usize = 4096;

/// One process's handle table: `(rows, denied, names_denied)`.
///
/// One query against the process, not a filtered read of the machine's table --
/// see [`nt::PROCESS_HANDLE_INFORMATION`] for the 625 MB that buys.
fn handles(
    pid: u32,
    types: &[Arc<str>],
    buf: &mut AlignedBuf,
    names: &mut AlignedBuf,
) -> (Vec<OpenHandle>, bool, bool) {
    // Duplication rights are asked for up front, so one open serves the list and
    // the names. Refused -- a protected process, or one running as another user --
    // the same open is retried for the list alone, which is the more useful half.
    let mut names_denied = false;
    let h = Handle::new(unsafe {
        OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_DUP_HANDLE, 0, pid)
    })
    .or_else(|| {
        names_denied = true;
        Handle::new(unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid) })
    });
    let Some(h) = h else {
        return (Vec::new(), true, false);
    };

    let Ok(valid) = (unsafe { nt::query_process_info(h.raw(), nt::PROCESS_HANDLE_INFORMATION, buf) })
    else {
        return (Vec::new(), true, false);
    };
    let head = std::mem::size_of::<usize>() * 2;
    if valid < head {
        return (Vec::new(), true, false);
    }
    let base = buf.as_ptr();
    let reported = unsafe { std::ptr::read_unaligned(base.cast::<usize>()) };
    // Trust the count only as far as the bytes the kernel wrote.
    let entry_size = std::mem::size_of::<nt::ProcessHandleTableEntryInfo>();
    let n = reported.min((valid - head) / entry_size);

    let mut named = 0usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e: nt::ProcessHandleTableEntryInfo =
            unsafe { std::ptr::read_unaligned(base.add(head + i * entry_size).cast()) };
        let kind = types
            .get(e.ObjectTypeIndex as usize)
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| Arc::from(format!("Type {}", e.ObjectTypeIndex).as_str()));

        let mut name = None;
        if !names_denied && named < NAME_BUDGET && name_is_safe(&kind, e.GrantedAccess) {
            name = handle_name(h.raw(), e.HandleValue as HANDLE, names);
            named += 1;
        }

        out.push(OpenHandle {
            value: e.HandleValue as u32,
            kind,
            access: e.GrantedAccess,
            refs: e.HandleCount,
            name,
        });
    }
    (out, false, names_denied)
}

/// Whether asking this handle its name is safe to do on a thread we need back.
///
/// `NtQueryObject(ObjectNameInformation)` on a synchronous named pipe blocks until
/// the pipe's other end does something -- which may be never. There is no timeout
/// parameter and no way to cancel it; the only defences are a thread you are
/// willing to abandon, or not asking. This is not asking.
///
/// The masks are the ones a pipe is opened with, and they are the check Process
/// Explorer has used since the problem was first written up: `FILE_READ_DATA` plus
/// synchronize on a `File` object is overwhelmingly a pipe, and a real file named
/// through this path is one row's worth of information to give up for a UI that
/// cannot wedge.
fn name_is_safe(kind: &str, access: u32) -> bool {
    if kind != "File" {
        return true;
    }
    !matches!(access, 0x0012_019f | 0x0012_0189 | 0x0010_0000)
}

/// Duplicate one handle out of the target process and ask what it refers to.
fn handle_name(process: HANDLE, value: HANDLE, buf: &mut AlignedBuf) -> Option<Arc<str>> {
    let mut mine: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        DuplicateHandle(
            process,
            value,
            GetCurrentProcess(),
            &mut mine,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return None;
    }
    // Closed by the guard however this returns.
    let mine = Handle::new(mine)?;
    let valid = unsafe { nt::query_object(mine.raw(), nt::OBJECT_NAME_INFORMATION, buf) }.ok()?;
    if valid < std::mem::size_of::<nt::UNICODE_STRING>() {
        return None;
    }
    let s: nt::UNICODE_STRING = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
    let chars = s.Length as usize / 2;
    if s.Buffer.is_null() || chars == 0 {
        return None;
    }
    // The string sits in the tail of our own buffer; bound it to what was written.
    let start = s.Buffer as usize;
    let lo = buf.as_ptr() as usize;
    if start < lo || start + chars * 2 > lo + valid {
        return None;
    }
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(s.Buffer, chars) });
    (!text.is_empty()).then(|| Arc::from(text.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address labeller, which is the one piece of arithmetic here that a
    /// mistake would show as a plausible wrong answer rather than as an empty list.
    #[test]
    fn labels_an_address_against_the_module_that_contains_it() {
        let m = |base: usize, size: u32, name: &str| Module {
            base,
            size,
            name: Arc::from(name),
            path: Arc::from(""),
        };
        // Sorted by base, as `threads` guarantees.
        let mods = vec![
            m(0x1000, 0x100, "a.exe"),
            m(0x2000, 0x200, "b.dll"),
            m(0x8000, 0x10, "c.dll"),
        ];

        // Inside the first, at its base, and inside the last.
        assert_eq!(label_address(0x1010, &mods), "a.exe+0x10");
        assert_eq!(label_address(0x2000, &mods), "b.dll");
        assert_eq!(label_address(0x8008, &mods), "c.dll+0x8");

        // In the gaps: below everything, between two modules, and past the end of
        // the one that starts below it. A module's *size* has to be what ends it,
        // or every stray address would be labelled with the last module in the list.
        assert_eq!(label_address(0x900, &mods), "0x900");
        assert_eq!(label_address(0x1500, &mods), "0x1500");
        assert_eq!(label_address(0x9000, &mods), "0x9000");

        // A thread whose start address the kernel would not report.
        assert_eq!(label_address(0, &mods), "—");
    }

    /// The pipe guard. Getting this backwards is a UI that hangs on a process with
    /// a pipe open, which is most of them.
    #[test]
    fn declines_to_name_the_handles_that_can_block() {
        // The three access masks a synchronous pipe wears.
        for access in [0x0012_019f, 0x0012_0189, 0x0010_0000] {
            assert!(!name_is_safe("File", access), "{access:#x}");
        }
        // An ordinary file handle, and every other type whatever its access.
        assert!(name_is_safe("File", 0x0012_0089));
        assert!(name_is_safe("Key", 0x0012_019f));
        assert!(name_is_safe("Event", 0x0010_0000));
    }

    #[test]
    fn rounds_up_to_the_next_boundary() {
        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(4, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
    }

    /// This machine's own type table, read the way the handle list reads it. The
    /// walk is offset arithmetic over a variable-length structure, so it is worth
    /// checking against a kernel rather than only against itself.
    #[test]
    fn reads_the_object_type_table_this_machine_publishes() {
        let mut buf = AlignedBuf::with_capacity(8 * 1024);
        let types = object_types(&mut buf);
        assert!(
            types.len() > 20,
            "the machine reported {} object types",
            types.len()
        );
        // Types every Windows kernel has. Found by name rather than by index,
        // because the indices move between builds -- which is the whole reason the
        // table is read instead of hardcoded.
        for want in ["Process", "Thread", "File", "Key", "Event"] {
            assert!(
                types.iter().any(|t| &**t == want),
                "no `{want}` in the type table: {types:?}"
            );
        }
    }

    /// Our own process, end to end: it has threads, it has modules, and one of the
    /// modules is the test binary itself.
    #[test]
    fn inspects_the_process_running_the_test() {
        let pid = std::process::id();

        let (mods, denied) = modules(pid);
        assert!(!denied, "our own process would not open for a module list");
        assert!(
            mods.iter().any(|m| m.name.ends_with(".exe")),
            "no image among {} modules",
            mods.len()
        );
        assert!(mods.iter().any(|m| &*m.name == "ntdll.dll"));

        let mut buf = AlignedBuf::with_capacity(256 * 1024);
        let threads = threads(pid, &mut buf);
        assert!(!threads.is_empty(), "a running process with no threads");
        // Every thread starts somewhere a module owns -- Windows starts them in
        // `ntdll.dll` -- so the labeller has to have resolved at least one.
        assert!(
            threads.iter().any(|t| t.start_label.contains(".dll")),
            "no thread start resolved to a module: {:?}",
            threads.iter().map(|t| &t.start_label).collect::<Vec<_>>()
        );
    }

    /// The handle list of our own process, which is the one process we are
    /// guaranteed `PROCESS_DUP_HANDLE` on.
    #[test]
    fn lists_and_names_our_own_handles() {
        let pid = std::process::id();
        let mut buf = AlignedBuf::with_capacity(512 * 1024);
        let mut names = AlignedBuf::with_capacity(2048);
        let types = object_types(&mut names);
        let (rows, denied, names_denied) = handles(pid, &types, &mut buf, &mut names);

        assert!(!denied, "the handle table would not read");
        assert!(!names_denied, "no duplication right on our own process");
        assert!(!rows.is_empty(), "a process with no handles");
        // A process always holds these two.
        assert!(rows.iter().any(|h| &*h.kind == "Key" || &*h.kind == "File"));
        assert!(
            rows.iter().any(|h| h.name.is_some()),
            "not one of {} handles could be named",
            rows.len()
        );
    }
}


