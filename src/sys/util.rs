//! Small Win32 helpers: wide strings, registry reads, handle RAII, the console.

use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ,
};

/// Borrow the console of whatever launched this, if there is one.
///
/// This is a `windows` subsystem binary in every profile -- see the attribute at the top of
/// [`crate`] -- so it never *creates* a console, which is the point: a window that flashes a
/// black one open behind itself on every launch is the thing that gate was costing. What it
/// costs instead is the standard handles. A `windows` subsystem process starts with none, so
/// `AZT_TRACE_SAMPLE`'s per-phase timings, the `memtrace` figures and a panic message all
/// write into nothing -- and Rust's `write`
/// for stdout treats "no stdio handle" as a successful write of every byte, so they fail
/// *silently*, which is the worst of the available failures.
///
/// `ATTACH_PARENT_PROCESS` is the whole of the fix. If the parent has a console -- a shell,
/// or `cargo claude-run` -- this process joins it, and `println!` reaches it because std asks
/// `GetStdHandle` on every write rather than caching a handle at startup. If the parent has
/// none -- Explorer, a pinned shortcut, the Startup folder -- the call fails and creates
/// nothing, which is exactly what a launch from there wants. Either way there is nothing to
/// undo on the way out: the console belongs to the shell and outlives us.
///
/// What it cannot fix is the ordering. A `windows` subsystem process does not hold the shell
/// that started it, so the prompt comes back first and this program's output lands underneath
/// it. That is a cosmetic oddity of every GUI program that prints, and the alternative -- a
/// console of our own -- is the thing being avoided.
pub fn attach_parent_console() {
    // Failing is the ordinary case rather than the exception: launched from anywhere that is
    // not a console there is no parent console to attach to, and that is how this program is
    // usually started.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// A NUL-terminated UTF-16 buffer suitable for `PCWSTR` parameters.
pub fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Decode a fixed-size UTF-16 field, stopping at the first NUL.
pub fn from_wide_nul(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Owns a Win32 `HANDLE` and closes it on drop.
pub struct Handle(pub HANDLE);

impl Handle {
    /// Wraps a handle, mapping the two "no handle" sentinels to `None`.
    #[inline]
    pub fn new(h: HANDLE) -> Option<Self> {
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            None
        } else {
            Some(Self(h))
        }
    }

    #[inline]
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

// SAFETY: Win32 handles are process-wide kernel objects, not thread-affine.
unsafe impl Send for Handle {}

/// An open registry key that closes on drop.
struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn open_key(root: HKEY, path: &str) -> Option<RegKey> {
    let wpath = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(root, wpath.as_ptr(), 0, KEY_READ, &mut key) };
    (rc == ERROR_SUCCESS).then_some(RegKey(key))
}

/// Read a `REG_SZ` / `REG_EXPAND_SZ` value.
pub fn reg_string(root: HKEY, path: &str, name: &str) -> Option<String> {
    let key = open_key(root, path)?;
    let wname = wide(name);

    let mut ty: u32 = 0;
    let mut size: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            wname.as_ptr(),
            std::ptr::null(),
            &mut ty,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if rc != ERROR_SUCCESS || size == 0 || (ty != REG_SZ && ty != REG_EXPAND_SZ) {
        return None;
    }

    // Round up to whole u16s so the final read cannot land mid-character.
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let mut size2 = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            wname.as_ptr(),
            std::ptr::null(),
            &mut ty,
            buf.as_mut_ptr().cast(),
            &mut size2,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    Some(from_wide_nul(&buf).trim().to_owned())
}

/// Read a `REG_DWORD` value.
pub fn reg_u32(root: HKEY, path: &str, name: &str) -> Option<u32> {
    let key = open_key(root, path)?;
    let wname = wide(name);

    let mut ty: u32 = 0;
    let mut val: u32 = 0;
    let mut size = 4u32;
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            wname.as_ptr(),
            std::ptr::null(),
            &mut ty,
            (&mut val as *mut u32).cast(),
            &mut size,
        )
    };
    (rc == ERROR_SUCCESS && ty == REG_DWORD).then_some(val)
}

/// Enumerate a registry key's values as `(name, string_value)` pairs.
pub fn reg_values(root: HKEY, path: &str) -> Vec<(String, String)> {
    use windows_sys::Win32::System::Registry::RegEnumValueW;

    let Some(key) = open_key(root, path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut name = [0u16; 256];
    let mut data = vec![0u16; 2048];
    for i in 0.. {
        let mut name_len = name.len() as u32;
        let mut data_len = (data.len() * 2) as u32;
        let mut ty = 0u32;
        let rc = unsafe {
            RegEnumValueW(
                key.0,
                i,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                &mut ty,
                data.as_mut_ptr().cast(),
                &mut data_len,
            )
        };
        if rc != ERROR_SUCCESS {
            break;
        }
        if ty == REG_SZ || ty == REG_EXPAND_SZ {
            let chars = (data_len as usize / 2).min(data.len());
            out.push((
                from_wide_nul(&name[..name_len as usize]),
                from_wide_nul(&data[..chars]),
            ));
        }
    }
    out
}

/// Convert a FILETIME (100 ns ticks since 1601-01-01 UTC) to a local
/// `HH:MM:SS` / `YYYY-MM-DD HH:MM:SS` string.
pub fn format_filetime(ft: i64, with_date: bool) -> String {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    if ft <= 0 {
        return "—".into();
    }
    let utc = FILETIME {
        dwLowDateTime: (ft as u64 & 0xFFFF_FFFF) as u32,
        dwHighDateTime: ((ft as u64) >> 32) as u32,
    };
    let mut sys: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { FileTimeToSystemTime(&utc, &mut sys) } == 0 {
        return "—".into();
    }
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &sys, &mut local) } == 0 {
        local = sys;
    }
    if with_date {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
        )
    } else {
        format!(
            "{:02}:{:02}:{:02}",
            local.wHour, local.wMinute, local.wSecond
        )
    }
}

// ---------------------------------------------------------------------------
// Vendor libraries, bound at run time
// ---------------------------------------------------------------------------

/// Load a DLL that may not be installed.
///
/// The GPU sensor interfaces ship with their vendor's display driver: linking
/// against any of them would refuse to start the app on a machine that has a
/// different card. Absent means "no sensors from that vendor", not an error.
pub fn load_dll(name: &str) -> Option<windows_sys::Win32::Foundation::HMODULE> {
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
    let module = unsafe { LoadLibraryW(wide(name).as_ptr()) };
    (!module.is_null()).then_some(module)
}

/// Look up one entry point by name. `name` must be NUL-terminated.
///
/// # Safety
/// The caller states the signature; nothing here can check it. Getting it wrong
/// is undefined behaviour, so every use sits beside the vendor's own declaration.
pub unsafe fn dll_symbol<T: Copy>(
    module: windows_sys::Win32::Foundation::HMODULE,
    name: &[u8],
) -> Option<T> {
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
    debug_assert_eq!(name.last(), Some(&0), "GetProcAddress needs a C string");
    debug_assert_eq!(
        std::mem::size_of::<T>(),
        std::mem::size_of::<*const std::ffi::c_void>(),
        "T must be a function pointer"
    );
    GetProcAddress(module, name.as_ptr()).map(|p| std::mem::transmute_copy(&p))
}
