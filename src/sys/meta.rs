//! Per-process metadata that `NtQuerySystemInformation` does not provide:
//! image path, publisher, description, owning user, command line, architecture.
//!
//! This is resolved in two stages because the costs differ by orders of
//! magnitude:
//!
//! * **Basic** -- an `OpenProcess` + `QueryFullProcessImageNameW`, tens of
//!   microseconds. Runs on the sampler thread because process *categorisation*
//!   needs the path.
//! * **Full** -- version-resource parsing (touches the file on disk) and
//!   `LookupAccountSidW` (can round-trip to a domain controller). Both are
//!   unbounded-latency operations and run on a dedicated worker thread.
//!
//! Results are keyed by [`ProcKey`] and cached for the lifetime of the process.

use super::snapshot::ProcKey;
use super::util::{from_wide_nul, wide, Handle};
use parking_lot::RwLock;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Arch {
    #[default]
    Unknown,
    X86,
    X64,
    Arm32,
    Arm64,
}

impl Arch {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "—",
            Self::X86 => "x86",
            Self::X64 => "x64",
            Self::Arm32 => "ARM32",
            Self::Arm64 => "ARM64",
        }
    }
}

/// Everything we know about a process beyond its kernel counters.
#[derive(Clone, Default)]
pub struct Meta {
    pub path: Option<Arc<str>>,
    /// `FileDescription` from the version resource, e.g. "Google Chrome".
    pub description: Option<Arc<str>>,
    pub company: Option<Arc<str>>,
    pub user: Option<Arc<str>>,
    pub cmdline: Option<Arc<str>>,
    pub elevated: bool,
    pub arch: Arch,
    /// True once the worker has finished the expensive stage.
    pub full: bool,
}

/// `FileDescription` and `CompanyName` from an executable's version resource.
type VersionStrings = (Option<Arc<str>>, Option<Arc<str>>);

/// Shared metadata cache. Reads are `RwLock` reads of a hash map, done only for
/// the handful of rows actually on screen, so contention is a non-issue.
pub struct MetaStore {
    map: RwLock<FxHashMap<ProcKey, Arc<Meta>>>,
    /// SID bytes -> account name. `LookupAccountSidW` is the single slowest
    /// call in this file; most processes share a few SIDs.
    sid_names: RwLock<FxHashMap<Box<[u8]>, Arc<str>>>,
    /// Image path -> version-resource strings, shared across all instances of
    /// the same executable (100 chrome.exe processes parse it once).
    file_info: RwLock<FxHashMap<Arc<str>, VersionStrings>>,
    tx: Sender<Job>,
    /// Set when the worker has drained its queue; drives the "resolving…" hint.
    idle: AtomicBool,
}

struct Job {
    key: ProcKey,
    path: Option<Arc<str>>,
}

impl MetaStore {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let store = Arc::new(Self {
            map: RwLock::new(FxHashMap::default()),
            sid_names: RwLock::new(FxHashMap::default()),
            file_info: RwLock::new(FxHashMap::default()),
            tx,
            idle: AtomicBool::new(true),
        });

        let worker_store = Arc::clone(&store);
        std::thread::Builder::new()
            .name("azt-meta".into())
            .spawn(move || worker(worker_store, rx))
            .expect("spawn metadata worker");

        store
    }

    pub fn get(&self, key: ProcKey) -> Option<Arc<Meta>> {
        self.map.read().get(&key).cloned()
    }

    /// Resolve the cheap fields for a newly seen process and queue the rest.
    ///
    /// Returns the image path so the caller can categorise the process without
    /// a second lookup.
    pub fn resolve_basic(&self, key: ProcKey) -> Option<Arc<str>> {
        // Kept as a standalone `let` so the read guard is released before the
        // `write()` further down (see the note in `enrich`).
        let existing = self.map.read().get(&key).cloned();
        if let Some(m) = existing {
            return m.path.clone();
        }

        let mut meta = Meta::default();

        // PROCESS_QUERY_LIMITED_INFORMATION is the least we can ask for and is
        // granted for most processes without elevation -- unlike
        // PROCESS_QUERY_INFORMATION, which fails on protected processes.
        let handle = Handle::new(unsafe {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, key.pid)
        });

        if let Some(h) = &handle {
            meta.path = query_image_path(h.raw()).map(Arc::from);
            meta.arch = query_arch(h.raw());
            meta.elevated = query_elevated(h.raw());
        }

        let path = meta.path.clone();
        self.map.write().insert(key, Arc::new(meta));

        // Hand the slow work off; a full queue is not worth blocking for.
        self.idle.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Job {
            key,
            path: path.clone(),
        });

        path
    }

    /// Entry counts for the three caches, for `--features memtrace`.
    #[cfg(feature = "memtrace")]
    pub fn debug_lens(&self) -> (usize, usize, usize) {
        (
            self.map.read().len(),
            self.sid_names.read().len(),
            self.file_info.read().len(),
        )
    }

    /// Drop cache entries for processes that no longer exist.
    pub fn retain_live(&self, live: &FxHashSet<ProcKey>) {
        let mut map = self.map.write();
        if map.len() > live.len() * 2 + 64 {
            map.retain(|k, _| live.contains(k));
        }
    }
}

fn worker(store: Arc<MetaStore>, rx: Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        enrich(&store, &job);
        // Only claim idle once the queue is actually drained.
        if rx.try_recv().is_err() {
            store.idle.store(true, Ordering::Relaxed);
        }
    }
}

fn enrich(store: &MetaStore, job: &Job) {
    let mut desc = None;
    let mut company = None;

    if let Some(path) = &job.path {
        // One parse per distinct executable, not per process.
        //
        // NOTE: the lookup must be its own `let` statement. Writing this as
        // `if let Some(..) = store.file_info.read().get(..) { } else { ... }`
        // keeps the read guard alive across the else branch under edition 2021,
        // and the `write()` below then deadlocks against our own read.
        let cached = store.file_info.read().get(path).cloned();
        match cached {
            Some((d, c)) => {
                desc = d;
                company = c;
            }
            None => {
                let info = read_version_info(path);
                store.file_info.write().insert(path.clone(), info.clone());
                desc = info.0;
                company = info.1;
            }
        }
    }

    let handle = Handle::new(unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, job.key.pid)
    });
    let mut user = None;
    let mut cmdline = None;
    if let Some(h) = &handle {
        user = query_user(store, h.raw());
        cmdline = super::nt::query_command_line(h.raw()).map(Arc::from);
    }

    // Merge rather than overwrite: the basic stage owns path/arch/elevated.
    let mut map = store.map.write();
    let entry = map.entry(job.key).or_default();
    let mut m = (**entry).clone();
    m.description = desc;
    m.company = company;
    m.user = user;
    m.cmdline = cmdline;
    m.full = true;
    *entry = Arc::new(m);
}

// ---------------------------------------------------------------------------
// Individual queries
// ---------------------------------------------------------------------------

fn query_image_path(h: HANDLE) -> Option<String> {
    let mut buf = [0u16; 32768 / 4];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
    };
    if ok == 0 || len == 0 {
        return None;
    }
    Some(from_wide_nul(&buf[..len as usize]))
}

fn query_arch(h: HANDLE) -> Arch {
    use windows_sys::Win32::System::SystemInformation::{
        IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM, IMAGE_FILE_MACHINE_ARM64,
        IMAGE_FILE_MACHINE_ARMNT, IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_UNKNOWN,
    };
    use windows_sys::Win32::System::Threading::IsWow64Process2;

    let mut process_machine: u16 = 0;
    let mut native_machine: u16 = 0;
    if unsafe { IsWow64Process2(h, &mut process_machine, &mut native_machine) } == 0 {
        return Arch::Unknown;
    }

    let map = |m: u16| match m {
        x if x == IMAGE_FILE_MACHINE_I386 => Arch::X86,
        x if x == IMAGE_FILE_MACHINE_AMD64 => Arch::X64,
        x if x == IMAGE_FILE_MACHINE_ARM64 => Arch::Arm64,
        x if x == IMAGE_FILE_MACHINE_ARM || x == IMAGE_FILE_MACHINE_ARMNT => Arch::Arm32,
        _ => Arch::Unknown,
    };

    // UNKNOWN means "not running under WOW64", i.e. native.
    if process_machine == IMAGE_FILE_MACHINE_UNKNOWN {
        map(native_machine)
    } else {
        map(process_machine)
    }
}

fn query_elevated(h: HANDLE) -> bool {
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_QUERY};

    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(h, TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let Some(token) = Handle::new(token) else {
        return false;
    };

    let mut elevated: u32 = 0;
    let mut ret = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token.raw(),
            TokenElevation,
            (&mut elevated as *mut u32).cast(),
            4,
            &mut ret,
        )
    };
    ok != 0 && elevated != 0
}

fn query_user(store: &MetaStore, h: HANDLE) -> Option<Arc<str>> {
    use windows_sys::Win32::Security::{
        GetTokenInformation, LookupAccountSidW, TokenUser, TOKEN_QUERY, TOKEN_USER,
    };

    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(h, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = Handle::new(token)?;

    // Size probe, then fetch: TOKEN_USER is followed by the variable-length SID.
    let mut need = 0u32;
    unsafe { GetTokenInformation(token.raw(), TokenUser, std::ptr::null_mut(), 0, &mut need) };
    if need == 0 || need > 4096 {
        return None;
    }
    let mut buf = vec![0u8; need as usize];
    let mut ret = 0u32;
    if unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            need,
            &mut ret,
        )
    } == 0
    {
        return None;
    }

    let tu: TOKEN_USER = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
    let sid = tu.User.Sid;
    if sid.is_null() {
        return None;
    }

    let sid_len = unsafe { windows_sys::Win32::Security::GetLengthSid(sid) } as usize;
    if sid_len == 0 || sid_len > 256 {
        return None;
    }
    let sid_bytes: Box<[u8]> =
        unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), sid_len) }.into();

    // Standalone `let`: releases the read guard before the `write()` below.
    let cached = store.sid_names.read().get(&sid_bytes).cloned();
    if let Some(hit) = cached {
        return Some(hit);
    }

    let mut name = [0u16; 256];
    let mut name_len = name.len() as u32;
    let mut domain = [0u16; 256];
    let mut domain_len = domain.len() as u32;
    let mut use_ty = 0i32;
    let resolved = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut use_ty,
        )
    };

    let label: Arc<str> = if resolved != 0 {
        let n = from_wide_nul(&name[..name_len as usize]);
        let d = from_wide_nul(&domain[..domain_len as usize]);
        if d.is_empty() {
            n.into()
        } else {
            format!("{d}\\{n}").into()
        }
    } else {
        // An unresolvable SID is normal for some sandboxed/AppContainer tokens.
        "—".into()
    };

    store
        .sid_names
        .write()
        .insert(sid_bytes, label.clone());
    Some(label)
}

/// Pull `FileDescription` and `CompanyName` out of an executable's version
/// resource.
fn read_version_info(path: &str) -> VersionStrings {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };

    let wpath = wide(path);
    let mut dummy = 0u32;
    let size = unsafe { GetFileVersionInfoSizeW(wpath.as_ptr(), &mut dummy) };
    if size == 0 || size > 1 << 22 {
        return (None, None);
    }

    let mut data = vec![0u8; size as usize];
    if unsafe { GetFileVersionInfoW(wpath.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return (None, None);
    }

    // Find which language/codepage the strings are stored under; assuming
    // 040904B0 (US English) misses localised binaries entirely.
    let mut tr_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut tr_len = 0u32;
    let tr_key = wide(r"\VarFileInfo\Translation");
    let have_tr = unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            tr_key.as_ptr(),
            &mut tr_ptr,
            &mut tr_len,
        )
    };

    let mut langs: Vec<(u16, u16)> = Vec::new();
    if have_tr != 0 && !tr_ptr.is_null() && tr_len >= 4 {
        let n = (tr_len / 4) as usize;
        for i in 0..n {
            let p = unsafe { tr_ptr.cast::<u16>().add(i * 2) };
            let lang = unsafe { std::ptr::read_unaligned(p) };
            let cp = unsafe { std::ptr::read_unaligned(p.add(1)) };
            langs.push((lang, cp));
        }
    }
    // Fall back to the two most common encodings.
    langs.push((0x0409, 0x04B0));
    langs.push((0x0409, 0x04E4));

    let query = |lang: u16, cp: u16, field: &str| -> Option<Arc<str>> {
        let key = wide(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{field}"));
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let ok = unsafe {
            VerQueryValueW(data.as_ptr().cast(), key.as_ptr(), &mut ptr, &mut len)
        };
        if ok == 0 || ptr.is_null() || len == 0 {
            return None;
        }
        // `len` counts characters including the NUL.
        let s = from_wide_nul(unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), len as usize) });
        let s = s.trim();
        (!s.is_empty()).then(|| Arc::from(s))
    };

    for (lang, cp) in langs {
        let desc = query(lang, cp, "FileDescription");
        let company = query(lang, cp, "CompanyName");
        if desc.is_some() || company.is_some() {
            return (desc, company);
        }
    }
    (None, None)
}
