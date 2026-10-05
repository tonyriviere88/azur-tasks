//! Raw `NtQuerySystemInformation` bindings.
//!
//! We declare the structures ourselves rather than leaning on a binding crate:
//! the layouts below are the documented x64 layouts, and owning them means the
//! hot path is a single syscall into a reusable buffer followed by a pointer
//! walk -- no allocation, no COM, no per-process handles.

#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::c_void;

// ---------------------------------------------------------------------------
// ntdll
// ---------------------------------------------------------------------------

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        SystemInformationClass: u32,
        SystemInformation: *mut c_void,
        SystemInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;

    fn NtQueryInformationProcess(
        ProcessHandle: *mut c_void,
        ProcessInformationClass: u32,
        ProcessInformation: *mut c_void,
        ProcessInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;

    fn NtQueryObject(
        Handle: *mut c_void,
        ObjectInformationClass: u32,
        ObjectInformation: *mut c_void,
        ObjectInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;

    fn NtQueryInformationThread(
        ThreadHandle: *mut c_void,
        ThreadInformationClass: u32,
        ThreadInformation: *mut c_void,
        ThreadInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
}

pub const STATUS_SUCCESS: i32 = 0;
pub const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;

// SYSTEM_INFORMATION_CLASS values we use.
pub const SYSTEM_BASIC_INFORMATION: u32 = 0;
pub const SYSTEM_PROCESS_INFORMATION: u32 = 5;
pub const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;
pub const SYSTEM_MEMORY_LIST_INFORMATION: u32 = 80;
/// `PROCESSINFOCLASS::ProcessHandleInformation` (Windows 8+): one process's handle
/// table, and nothing else's.
///
/// The alternative is `SystemExtendedHandleInformation`, which is the whole
/// machine's table filtered down to one pid afterwards. Measured on the machine
/// this was written on, that is **625 MB and 2.1 seconds** for the 144 handles a
/// process actually holds -- per read, because the table is rebuilt every time --
/// and a buffer the worker would then hold for the life of the process. This class
/// answers the same question in a few kilobytes.
///
/// What it does not carry is the kernel address of the object, so two handles onto
/// the same object cannot be told apart by it. It carries the object's handle and
/// pointer counts instead, which is the question that was worth asking anyway.
pub const PROCESS_HANDLE_INFORMATION: u32 = 51;

/// `OBJECT_INFORMATION_CLASS::ObjectNameInformation` -- what a handle refers to,
/// as a `UNICODE_STRING`: a path, a registry key, a named pipe.
pub const OBJECT_NAME_INFORMATION: u32 = 1;
/// `ObjectTypesInformation` -- *all* of them, in one call, each carrying the
/// `TypeIndex` the handle table reports. The alternative is
/// `ObjectTypeInformation` (2) per handle, which needs the handle duplicated
/// first and answers the same question a thousand times.
pub const OBJECT_TYPES_INFORMATION: u32 = 3;

// PROCESSINFOCLASS::ProcessCommandLineInformation (Win 8.1+). Returns a
// UNICODE_STRING and needs only PROCESS_QUERY_LIMITED_INFORMATION, which is far
// cheaper and far more permissive than walking the remote PEB by hand.
pub const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;

// ---------------------------------------------------------------------------
// 8-byte aligned scratch buffer
// ---------------------------------------------------------------------------

/// A growable byte buffer guaranteed to be 8-byte aligned.
///
/// `NtQuerySystemInformation` returns structures containing `u64`/pointer
/// fields, and every `NextEntryOffset` is a multiple of 8. Backing the buffer
/// with `u64` gives us that alignment for free -- a plain `Vec<u8>` is only
/// 1-aligned, which would make every field read a misaligned access.
pub struct AlignedBuf {
    words: Vec<u64>,
}

impl AlignedBuf {
    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            words: vec![0u64; bytes.div_ceil(8)],
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.words.len() * 8
    }

    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut c_void {
        self.words.as_mut_ptr().cast()
    }

    #[inline]
    pub fn as_ptr(&self) -> *const u8 {
        self.words.as_ptr().cast()
    }

    fn grow_to(&mut self, bytes: usize) {
        let want = bytes.div_ceil(8);
        if want > self.words.len() {
            self.words.resize(want, 0);
        }
    }
}

/// Fill `buf` with the requested system information class, growing it as needed.
///
/// Returns the number of valid bytes. Steady state performs zero allocations
/// because the buffer keeps whatever capacity the previous call needed.
pub fn query_system_info(class: u32, buf: &mut AlignedBuf) -> Result<usize, i32> {
    // The process list can change size between the sizing call and the real
    // call, so this has to be a retry loop rather than a query-then-fetch.
    for _ in 0..8 {
        let mut ret_len: u32 = 0;
        let status = unsafe {
            NtQuerySystemInformation(class, buf.as_mut_ptr(), buf.len() as u32, &mut ret_len)
        };

        if status == STATUS_SUCCESS {
            let len = if ret_len as usize <= buf.len() {
                ret_len as usize
            } else {
                buf.len()
            };
            return Ok(len);
        }

        if status != STATUS_INFO_LENGTH_MISMATCH {
            return Err(status);
        }

        // Overshoot so a few processes spawning between calls doesn't force
        // another round trip.
        let want = (ret_len as usize).max(buf.len() + 1);
        buf.grow_to(want + want / 4 + 64 * 1024);
    }
    Err(STATUS_INFO_LENGTH_MISMATCH)
}

// ---------------------------------------------------------------------------
// Structures (x64 layouts)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UNICODE_STRING {
    pub Length: u16,
    pub MaximumLength: u16,
    _pad: u32,
    pub Buffer: *mut u16,
}

/// `SYSTEM_PROCESS_INFORMATION` -- 0x100 bytes on x64, followed inline by
/// `NumberOfThreads` * `SYSTEM_THREAD_INFORMATION`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SystemProcessInformation {
    pub NextEntryOffset: u32,          // 0x00
    pub NumberOfThreads: u32,          // 0x04
    pub WorkingSetPrivateSize: i64,    // 0x08
    pub HardFaultCount: u32,           // 0x10
    pub NumberOfThreadsHighWatermark: u32, // 0x14
    pub CycleTime: u64,                // 0x18
    pub CreateTime: i64,               // 0x20
    pub UserTime: i64,                 // 0x28
    pub KernelTime: i64,               // 0x30
    pub ImageName: UNICODE_STRING,     // 0x38
    pub BasePriority: i32,             // 0x48
    _pad0: u32,                        // 0x4C
    pub UniqueProcessId: isize,        // 0x50
    pub InheritedFromUniqueProcessId: isize, // 0x58
    pub HandleCount: u32,              // 0x60
    pub SessionId: u32,                // 0x64
    pub UniqueProcessKey: usize,       // 0x68
    pub PeakVirtualSize: usize,        // 0x70
    pub VirtualSize: usize,            // 0x78
    pub PageFaultCount: u32,           // 0x80
    _pad1: u32,                        // 0x84
    pub PeakWorkingSetSize: usize,     // 0x88
    pub WorkingSetSize: usize,         // 0x90
    pub QuotaPeakPagedPoolUsage: usize,    // 0x98
    pub QuotaPagedPoolUsage: usize,        // 0xA0
    pub QuotaPeakNonPagedPoolUsage: usize, // 0xA8
    pub QuotaNonPagedPoolUsage: usize,     // 0xB0
    pub PagefileUsage: usize,          // 0xB8  (== private bytes / commit)
    pub PeakPagefileUsage: usize,      // 0xC0
    pub PrivatePageCount: usize,       // 0xC8
    pub ReadOperationCount: i64,       // 0xD0
    pub WriteOperationCount: i64,      // 0xD8
    pub OtherOperationCount: i64,      // 0xE0
    pub ReadTransferCount: i64,        // 0xE8
    pub WriteTransferCount: i64,       // 0xF0
    pub OtherTransferCount: i64,       // 0xF8
}

/// `SYSTEM_THREAD_INFORMATION` -- 0x50 bytes on x64.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SystemThreadInformation {
    pub KernelTime: i64,      // 0x00
    pub UserTime: i64,        // 0x08
    pub CreateTime: i64,      // 0x10
    pub WaitTime: u32,        // 0x18
    _pad0: u32,               // 0x1C
    pub StartAddress: *mut c_void, // 0x20
    pub UniqueProcess: isize, // 0x28
    pub UniqueThread: isize,  // 0x30
    pub Priority: i32,        // 0x38
    pub BasePriority: i32,    // 0x3C
    pub ContextSwitches: u32, // 0x40
    pub ThreadState: u32,     // 0x44
    pub WaitReason: u32,      // 0x48
    _pad1: u32,               // 0x4C
}

/// `PROCESS_HANDLE_SNAPSHOT_INFORMATION`: a count, a reserved word, then the
/// entries inline. Sized for one entry here; the array is walked from `Handles` on.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessHandleSnapshotInformation {
    pub NumberOfHandles: usize,
    pub Reserved: usize,
    pub Handles: [ProcessHandleTableEntryInfo; 1],
}

/// `PROCESS_HANDLE_TABLE_ENTRY_INFO` -- 0x28 bytes on x64.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessHandleTableEntryInfo {
    pub HandleValue: *mut c_void, // 0x00
    /// How many handles onto this object exist, machine-wide.
    pub HandleCount: usize, // 0x08
    /// References including the kernel's own, which is always at least one more.
    pub PointerCount: usize, // 0x10
    pub GrantedAccess: u32,  // 0x18
    /// The same index space the object type table uses -- a `ULONG` here where the
    /// system-wide table spells it `USHORT`.
    pub ObjectTypeIndex: u32, // 0x1C
    pub HandleAttributes: u32, // 0x20
    _reserved: u32,          // 0x24
}

/// Read a process information class into `buf`, growing it as needed.
///
/// The retry loop is [`query_system_info`]'s, for the same reason: a process can
/// open a handle between the sizing call and the real one.
///
/// # Safety
/// `handle` must be an open process handle with the rights the class requires.
pub unsafe fn query_process_info(
    handle: *mut c_void,
    class: u32,
    buf: &mut AlignedBuf,
) -> Result<usize, i32> {
    for _ in 0..8 {
        let mut ret_len: u32 = 0;
        let status = NtQueryInformationProcess(
            handle,
            class,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut ret_len,
        );
        if status == STATUS_SUCCESS {
            return Ok((ret_len as usize).min(buf.len()));
        }
        const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023u32 as i32;
        if !matches!(status, STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_TOO_SMALL) {
            return Err(status);
        }
        let want = (ret_len as usize).max(buf.len() + 1);
        buf.grow_to(want + want / 4 + 4 * 1024);
    }
    Err(STATUS_INFO_LENGTH_MISMATCH)
}

/// `OBJECT_TYPE_INFORMATION`, as `ObjectTypesInformation` lays them out: a
/// `u32` count, then the entries, each one padded to an 8-byte boundary *after*
/// its name's buffer, which follows the struct inline.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ObjectTypeInformation {
    pub TypeName: UNICODE_STRING,
    pub TotalNumberOfObjects: u32,
    pub TotalNumberOfHandles: u32,
    pub TotalPagedPoolUsage: u32,
    pub TotalNonPagedPoolUsage: u32,
    pub TotalNamePoolUsage: u32,
    pub TotalHandleTableUsage: u32,
    pub HighWaterNumberOfObjects: u32,
    pub HighWaterNumberOfHandles: u32,
    pub HighWaterPagedPoolUsage: u32,
    pub HighWaterNonPagedPoolUsage: u32,
    pub HighWaterNamePoolUsage: u32,
    pub HighWaterHandleTableUsage: u32,
    pub InvalidAttributes: u32,
    pub GenericMapping: [u32; 4],
    pub ValidAccessMask: u32,
    pub SecurityRequired: u8,
    pub MaintainHandleCount: u8,
    pub TypeIndex: u8,
    _reserved: u8,
    pub PoolType: u32,
    pub DefaultPagedPoolCharge: u32,
    pub DefaultNonPagedPoolCharge: u32,
}

/// Ask an object what it is called. The buffer must be 8-byte aligned, since the
/// answer starts with a `UNICODE_STRING`.
///
/// # Safety
/// `handle` must be a handle this process owns and may query.
pub unsafe fn query_object(
    handle: *mut c_void,
    class: u32,
    buf: &mut AlignedBuf,
) -> Result<usize, i32> {
    let mut ret_len: u32 = 0;
    let status = NtQueryObject(handle, class, buf.as_mut_ptr(), buf.len() as u32, &mut ret_len);
    if status == STATUS_SUCCESS {
        return Ok((ret_len as usize).min(buf.len()));
    }
    // Two spellings of "your buffer is too small", plus the info-length one.
    const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005u32 as i32;
    const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023u32 as i32;
    if !matches!(
        status,
        STATUS_BUFFER_OVERFLOW | STATUS_BUFFER_TOO_SMALL | STATUS_INFO_LENGTH_MISMATCH
    ) {
        return Err(status);
    }
    buf.grow_to((ret_len as usize).max(buf.len() * 2) + 256);
    let mut ret_len: u32 = 0;
    let status = NtQueryObject(handle, class, buf.as_mut_ptr(), buf.len() as u32, &mut ret_len);
    if status == STATUS_SUCCESS {
        Ok((ret_len as usize).min(buf.len()))
    } else {
        Err(status)
    }
}

/// Where a thread was told to begin, from the thread itself.
///
/// `SYSTEM_THREAD_INFORMATION::StartAddress` is zero for an ordinary caller --
/// Windows masks kernel and user pointers out of `NtQuerySystemInformation` unless
/// the process holds `SeDebugPrivilege`, which a task manager should not need in
/// order to list threads. `ThreadQuerySetWin32StartAddress` asks per thread with
/// `THREAD_QUERY_LIMITED_INFORMATION` and answers at medium integrity, and it is
/// the *Win32* entry point -- `CreateThread`'s argument rather than the loader
/// stub every thread in the machine shares.
///
/// # Safety
/// `thread` must be an open thread handle with query rights.
pub unsafe fn query_thread_start(thread: *mut c_void) -> Option<usize> {
    /// `THREADINFOCLASS::ThreadQuerySetWin32StartAddress`
    const CLASS: u32 = 9;
    let mut addr: usize = 0;
    let mut ret = 0u32;
    let status = NtQueryInformationThread(
        thread,
        CLASS,
        (&mut addr as *mut usize).cast(),
        std::mem::size_of::<usize>() as u32,
        &mut ret,
    );
    (status == STATUS_SUCCESS && addr != 0).then_some(addr)
}

/// `KTHREAD_STATE::Waiting`
pub const THREAD_STATE_WAIT: u32 = 5;
/// `KWAIT_REASON::Suspended`
pub const WAIT_REASON_SUSPENDED: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SystemProcessorPerformanceInformation {
    pub IdleTime: i64,
    /// Includes `IdleTime`.
    pub KernelTime: i64,
    pub UserTime: i64,
    pub DpcTime: i64,
    pub InterruptTime: i64,
    pub InterruptCount: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SystemBasicInformation {
    pub Reserved: u32,
    pub TimerResolution: u32,
    pub PageSize: u32,
    pub NumberOfPhysicalPages: u32,
    pub LowestPhysicalPageNumber: u32,
    pub HighestPhysicalPageNumber: u32,
    pub AllocationGranularity: u32,
    pub MinimumUserModeAddress: usize,
    pub MaximumUserModeAddress: usize,
    pub ActiveProcessorsAffinityMask: usize,
    pub NumberOfProcessors: i8,
}

/// `SYSTEM_MEMORY_LIST_INFORMATION` -- drives the memory-composition bar
/// (free / modified / standby), which `GlobalMemoryStatusEx` cannot provide.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SystemMemoryListInformation {
    pub ZeroPageCount: usize,
    pub FreePageCount: usize,
    pub ModifiedPageCount: usize,
    pub ModifiedNoWritePageCount: usize,
    pub BadPageCount: usize,
    pub PageCountByPriority: [usize; 8],
    pub RepurposedPagesByPriority: [usize; 8],
    pub ModifiedPageCountPageFile: usize,
}

// ---------------------------------------------------------------------------
// Layout guards
//
// Every number the app displays is read through these offsets. A silent
// mismatch would not crash -- it would just show plausible garbage -- so the
// documented x64 layout is pinned at compile time.
// ---------------------------------------------------------------------------

const _: () = {
    use std::mem::{align_of, offset_of, size_of};

    assert!(size_of::<UNICODE_STRING>() == 0x10);

    assert!(size_of::<SystemProcessInformation>() == 0x100);
    assert!(align_of::<SystemProcessInformation>() == 8);
    assert!(offset_of!(SystemProcessInformation, WorkingSetPrivateSize) == 0x08);
    assert!(offset_of!(SystemProcessInformation, CycleTime) == 0x18);
    assert!(offset_of!(SystemProcessInformation, CreateTime) == 0x20);
    assert!(offset_of!(SystemProcessInformation, UserTime) == 0x28);
    assert!(offset_of!(SystemProcessInformation, KernelTime) == 0x30);
    assert!(offset_of!(SystemProcessInformation, ImageName) == 0x38);
    assert!(offset_of!(SystemProcessInformation, BasePriority) == 0x48);
    assert!(offset_of!(SystemProcessInformation, UniqueProcessId) == 0x50);
    assert!(offset_of!(SystemProcessInformation, InheritedFromUniqueProcessId) == 0x58);
    assert!(offset_of!(SystemProcessInformation, HandleCount) == 0x60);
    assert!(offset_of!(SystemProcessInformation, SessionId) == 0x64);
    assert!(offset_of!(SystemProcessInformation, PeakVirtualSize) == 0x70);
    assert!(offset_of!(SystemProcessInformation, VirtualSize) == 0x78);
    assert!(offset_of!(SystemProcessInformation, PageFaultCount) == 0x80);
    assert!(offset_of!(SystemProcessInformation, PeakWorkingSetSize) == 0x88);
    assert!(offset_of!(SystemProcessInformation, WorkingSetSize) == 0x90);
    assert!(offset_of!(SystemProcessInformation, PagefileUsage) == 0xB8);
    assert!(offset_of!(SystemProcessInformation, PrivatePageCount) == 0xC8);
    assert!(offset_of!(SystemProcessInformation, ReadOperationCount) == 0xD0);
    assert!(offset_of!(SystemProcessInformation, ReadTransferCount) == 0xE8);
    assert!(offset_of!(SystemProcessInformation, WriteTransferCount) == 0xF0);
    assert!(offset_of!(SystemProcessInformation, OtherTransferCount) == 0xF8);

    assert!(size_of::<SystemThreadInformation>() == 0x50);
    assert!(offset_of!(SystemThreadInformation, StartAddress) == 0x20);
    assert!(offset_of!(SystemThreadInformation, UniqueProcess) == 0x28);
    assert!(offset_of!(SystemThreadInformation, UniqueThread) == 0x30);
    assert!(offset_of!(SystemThreadInformation, ThreadState) == 0x44);
    assert!(offset_of!(SystemThreadInformation, WaitReason) == 0x48);

    assert!(size_of::<SystemProcessorPerformanceInformation>() == 0x30);

    assert!(size_of::<ProcessHandleTableEntryInfo>() == 0x28);
    assert!(align_of::<ProcessHandleTableEntryInfo>() == 8);
    assert!(offset_of!(ProcessHandleTableEntryInfo, HandleCount) == 0x08);
    assert!(offset_of!(ProcessHandleTableEntryInfo, PointerCount) == 0x10);
    assert!(offset_of!(ProcessHandleTableEntryInfo, GrantedAccess) == 0x18);
    assert!(offset_of!(ProcessHandleTableEntryInfo, ObjectTypeIndex) == 0x1C);
    assert!(offset_of!(ProcessHandleTableEntryInfo, HandleAttributes) == 0x20);
    // The entries start after the count and its padding.
    assert!(offset_of!(ProcessHandleSnapshotInformation, Handles) == 0x10);

    assert!(offset_of!(ObjectTypeInformation, TotalNumberOfObjects) == 0x10);
    assert!(offset_of!(ObjectTypeInformation, GenericMapping) == 0x44);
    // The field the whole type table is read for, and the one added in 8.1: a
    // build that shifted it would silently label every handle as the wrong kind.
    assert!(offset_of!(ObjectTypeInformation, TypeIndex) == 0x5A);
    assert!(size_of::<ObjectTypeInformation>() == 0x68);
};

// ---------------------------------------------------------------------------
// Typed helpers
// ---------------------------------------------------------------------------

/// Iterator over the process entries packed inside a filled buffer.
pub struct ProcessIter<'a> {
    base: *const u8,
    offset: usize,
    valid: usize,
    done: bool,
    _life: std::marker::PhantomData<&'a AlignedBuf>,
}

/// One process entry plus a borrowed view of its inline thread array.
pub struct ProcessEntry<'a> {
    pub info: SystemProcessInformation,
    pub threads: &'a [SystemThreadInformation],
    /// UTF-16 image name, borrowed from the query buffer.
    pub image_name: &'a [u16],
}

impl<'a> Iterator for ProcessIter<'a> {
    type Item = ProcessEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        const PROC_SIZE: usize = std::mem::size_of::<SystemProcessInformation>();
        const THREAD_SIZE: usize = std::mem::size_of::<SystemThreadInformation>();

        if self.done || self.offset + PROC_SIZE > self.valid {
            return None;
        }

        // SAFETY: `offset + PROC_SIZE <= valid`, and `valid` bytes were written
        // by the kernel into an 8-byte aligned buffer.
        let entry_ptr = unsafe { self.base.add(self.offset) };
        let info: SystemProcessInformation =
            unsafe { std::ptr::read_unaligned(entry_ptr.cast()) };

        // Trust the reported thread count only as far as the buffer allows; a
        // truncated read must not turn into an out-of-bounds slice.
        let max_threads = (self.valid - self.offset - PROC_SIZE) / THREAD_SIZE;
        let n_threads = (info.NumberOfThreads as usize).min(max_threads);
        let threads = unsafe {
            std::slice::from_raw_parts(
                entry_ptr.add(PROC_SIZE).cast::<SystemThreadInformation>(),
                n_threads,
            )
        };

        // ImageName.Buffer points into this same buffer for live entries.
        let image_name: &[u16] = if info.ImageName.Buffer.is_null() || info.ImageName.Length == 0 {
            &[]
        } else {
            let chars = info.ImageName.Length as usize / 2;
            // Bound the string to the buffer we own.
            let start = info.ImageName.Buffer as usize;
            let buf_start = self.base as usize;
            let buf_end = buf_start + self.valid;
            if start >= buf_start && start + chars * 2 <= buf_end {
                unsafe { std::slice::from_raw_parts(info.ImageName.Buffer, chars) }
            } else {
                &[]
            }
        };

        if info.NextEntryOffset == 0 {
            self.done = true;
        } else {
            self.offset += info.NextEntryOffset as usize;
        }

        Some(ProcessEntry {
            info,
            threads,
            image_name,
        })
    }
}

/// Walk the process entries in a buffer previously filled by
/// [`query_system_info`] with [`SYSTEM_PROCESS_INFORMATION`].
pub fn iter_processes(buf: &AlignedBuf, valid: usize) -> ProcessIter<'_> {
    ProcessIter {
        base: buf.as_ptr(),
        offset: 0,
        valid: valid.min(buf.len()),
        done: false,
        _life: std::marker::PhantomData,
    }
}

/// Read the per-logical-CPU performance counters into `out`.
pub fn query_cpu_times(
    buf: &mut AlignedBuf,
    out: &mut Vec<SystemProcessorPerformanceInformation>,
) -> bool {
    let Ok(valid) = query_system_info(SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION, buf) else {
        return false;
    };
    let n = valid / std::mem::size_of::<SystemProcessorPerformanceInformation>();
    out.clear();
    out.reserve(n);
    let base = buf.as_ptr().cast::<SystemProcessorPerformanceInformation>();
    for i in 0..n {
        out.push(unsafe { std::ptr::read_unaligned(base.add(i)) });
    }
    true
}

pub fn query_basic_info() -> Option<SystemBasicInformation> {
    let mut info = SystemBasicInformation::default();
    let mut ret = 0u32;
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_BASIC_INFORMATION,
            (&mut info as *mut SystemBasicInformation).cast(),
            std::mem::size_of::<SystemBasicInformation>() as u32,
            &mut ret,
        )
    };
    (status == STATUS_SUCCESS).then_some(info)
}

pub fn query_memory_list() -> Option<SystemMemoryListInformation> {
    let mut info = SystemMemoryListInformation::default();
    let mut ret = 0u32;
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_MEMORY_LIST_INFORMATION,
            (&mut info as *mut SystemMemoryListInformation).cast(),
            std::mem::size_of::<SystemMemoryListInformation>() as u32,
            &mut ret,
        )
    };
    (status == STATUS_SUCCESS).then_some(info)
}

/// Read a process's command line. `handle` needs
/// `PROCESS_QUERY_LIMITED_INFORMATION`.
pub fn query_command_line(handle: *mut c_void) -> Option<String> {
    let mut len = 0u32;
    // Size probe.
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut len,
        )
    };
    if status != STATUS_INFO_LENGTH_MISMATCH && status != STATUS_SUCCESS {
        return None;
    }
    if len == 0 || len > 1 << 20 {
        return None;
    }

    let mut buf = AlignedBuf::with_capacity(len as usize);
    let mut ret = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut ret,
        )
    };
    if status != STATUS_SUCCESS {
        return None;
    }

    let us: UNICODE_STRING = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
    if us.Buffer.is_null() || us.Length == 0 {
        return None;
    }
    let chars = us.Length as usize / 2;
    let slice = unsafe { std::slice::from_raw_parts(us.Buffer, chars) };
    Some(String::from_utf16_lossy(slice))
}
