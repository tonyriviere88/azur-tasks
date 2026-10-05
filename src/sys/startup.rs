//! Startup-app enumeration: the `Run` registry keys plus the Startup folders.
//!
//! Task Manager also reports an "impact" rating, which it derives from ETW boot
//! traces recorded by the OS. That data is not reachable from a normal process,
//! so this tab reports where each entry comes from instead of inventing a rating.

use super::util::{reg_values, wide};
use std::sync::Arc;

use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StartupSource {
    UserRun,
    MachineRun,
    MachineRun32,
    UserFolder,
    CommonFolder,
}

impl StartupSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::UserRun => "Registry (current user)",
            Self::MachineRun => "Registry (all users)",
            Self::MachineRun32 => "Registry (all users, 32-bit)",
            Self::UserFolder => "Startup folder (current user)",
            Self::CommonFolder => "Startup folder (all users)",
        }
    }

    /// Where Windows records whether entries from this source are on or off.
    ///
    /// The `StartupApproved` keys are Explorer's own switchboard: enabling and
    /// disabling never touches the `Run` value or the shortcut itself, which is
    /// why a disabled entry can be re-enabled at all. Each source has its own
    /// subkey, and 32-bit `Run` entries have their own again -- writing a WOW6432
    /// entry into `Run` instead of `Run32` disables nothing.
    fn approved_key(self) -> (HKEY, &'static str) {
        match self {
            Self::UserRun => (HKEY_CURRENT_USER, APPROVED_RUN),
            Self::MachineRun => (HKEY_LOCAL_MACHINE, APPROVED_RUN),
            Self::MachineRun32 => (HKEY_LOCAL_MACHINE, APPROVED_RUN32),
            Self::UserFolder => (HKEY_CURRENT_USER, APPROVED_FOLDER),
            Self::CommonFolder => (HKEY_LOCAL_MACHINE, APPROVED_FOLDER),
        }
    }

    /// True when only an administrator can change entries from this source.
    pub fn needs_admin(self) -> bool {
        matches!(
            self,
            Self::MachineRun | Self::MachineRun32 | Self::CommonFolder
        )
    }
}

const APPROVED_RUN: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const APPROVED_RUN32: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32";
const APPROVED_FOLDER: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder";

#[derive(Clone)]
pub struct StartupEntry {
    pub name: Arc<str>,
    pub command: Arc<str>,
    pub source: StartupSource,
    /// `false` when Windows has this entry disabled.
    pub enabled: bool,
    /// The name Explorer files this entry under in `StartupApproved`: the `Run`
    /// value's name, or a shortcut's file name *with* its extension. Not always
    /// [`Self::name`], which for a shortcut is the stem -- and switching an entry
    /// off under the wrong name silently does nothing.
    pub approval: Arc<str>,
}

pub fn enumerate() -> Vec<StartupEntry> {
    let mut out = Vec::new();

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";

    let disabled_user = disabled_set(HKEY_CURRENT_USER, APPROVED_RUN);
    let disabled_machine = disabled_set(HKEY_LOCAL_MACHINE, APPROVED_RUN);
    let disabled_machine32 = disabled_set(HKEY_LOCAL_MACHINE, APPROVED_RUN32);
    let disabled_folder = disabled_set(HKEY_CURRENT_USER, APPROVED_FOLDER);
    let disabled_common = disabled_set(HKEY_LOCAL_MACHINE, APPROVED_FOLDER);

    for (root, path, source, disabled) in [
        (HKEY_CURRENT_USER, RUN, StartupSource::UserRun, &disabled_user),
        (HKEY_LOCAL_MACHINE, RUN, StartupSource::MachineRun, &disabled_machine),
        (
            HKEY_LOCAL_MACHINE,
            RUN32,
            StartupSource::MachineRun32,
            &disabled_machine32,
        ),
    ] {
        for (name, command) in reg_values(root, path) {
            if name.is_empty() {
                continue;
            }
            let enabled = !disabled.contains(&name.to_ascii_lowercase());
            out.push(StartupEntry {
                name: Arc::from(name.as_str()),
                command: Arc::from(command.as_str()),
                source,
                enabled,
                approval: Arc::from(name.as_str()),
            });
        }
    }

    for (folder, source, disabled_folder) in [
        (
            startup_folder(false),
            StartupSource::UserFolder,
            &disabled_folder,
        ),
        (
            startup_folder(true),
            StartupSource::CommonFolder,
            &disabled_common,
        ),
    ] {
        let Some(dir) = folder else { continue };
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            // desktop.ini is folder metadata, not a startup item.
            if file_name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| file_name.clone());
            let enabled = !disabled_folder.contains(&file_name.to_ascii_lowercase());
            out.push(StartupEntry {
                name: Arc::from(stem.as_str()),
                command: Arc::from(path.to_string_lossy().as_ref()),
                source,
                enabled,
                approval: Arc::from(file_name.as_str()),
            });
        }
    }

    out.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    out
}

/// Turn a startup entry on or off, the way Explorer does.
///
/// Writes a 12-byte record into the entry's `StartupApproved` key: a 4-byte flag
/// word — 2 for enabled, 3 for disabled — followed by the `FILETIME` the switch
/// was thrown. The `Run` value and the shortcut are left exactly as they are, so
/// nothing is lost and the entry can be switched back.
///
/// Explorer reads this at sign-in, so a change takes effect at the next one
/// rather than now. Machine-wide entries live under `HKEY_LOCAL_MACHINE` and need
/// administrator rights; the error says so rather than leaving the caller to
/// guess at `ERROR_ACCESS_DENIED`.
pub fn set_enabled(entry: &StartupEntry, enable: bool) -> Result<String, String> {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, FILETIME};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, KEY_SET_VALUE, REG_BINARY,
        REG_OPTION_NON_VOLATILE,
    };
    use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;

    let (root, path) = entry.source.approved_key();
    let wpath = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    // Created rather than opened: a machine that has never had anything disabled
    // has no `StartupApproved` subkey at all.
    let rc = unsafe {
        RegCreateKeyExW(
            root,
            wpath.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(access_error(rc, entry, ERROR_ACCESS_DENIED));
    }

    let mut data = [0u8; 12];
    data[0] = if enable { 2 } else { 3 };
    if !enable {
        // Explorer stamps the moment an entry was switched off. Nothing reads it
        // back, but Task Manager writes it and a record that matches is one fewer
        // difference to explain.
        let mut ft = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        unsafe { GetSystemTimeAsFileTime(&mut ft) };
        data[4..8].copy_from_slice(&ft.dwLowDateTime.to_le_bytes());
        data[8..12].copy_from_slice(&ft.dwHighDateTime.to_le_bytes());
    }

    let wname = wide(&entry.approval);
    let rc = unsafe {
        RegSetValueExW(
            key,
            wname.as_ptr(),
            0,
            REG_BINARY,
            data.as_ptr(),
            data.len() as u32,
        )
    };
    unsafe { RegCloseKey(key) };
    if rc != ERROR_SUCCESS {
        return Err(access_error(rc, entry, ERROR_ACCESS_DENIED));
    }

    Ok(format!(
        "{} {}. Takes effect at the next sign-in.",
        entry.name,
        if enable { "enabled" } else { "disabled" }
    ))
}

fn access_error(rc: u32, entry: &StartupEntry, denied: u32) -> String {
    if rc == denied || entry.source.needs_admin() {
        format!(
            "Could not change {}: entries under \"{}\" need administrator rights.",
            entry.name,
            entry.source.label()
        )
    } else {
        format!("Could not change {}: Windows error {rc}.", entry.name)
    }
}

/// Read the `StartupApproved` keys, where a value whose first byte has bit 0 set
/// means the entry is disabled.
fn disabled_set(root: HKEY, path: &str) -> std::collections::HashSet<String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, KEY_READ,
    };

    let mut set = std::collections::HashSet::new();
    let wpath = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    if unsafe { RegOpenKeyExW(root, wpath.as_ptr(), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return set;
    }

    let mut name = [0u16; 512];
    let mut data = [0u8; 64];
    for i in 0.. {
        let mut name_len = name.len() as u32;
        let mut data_len = data.len() as u32;
        let mut ty = 0u32;
        let rc = unsafe {
            RegEnumValueW(
                key,
                i,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                &mut ty,
                data.as_mut_ptr(),
                &mut data_len,
            )
        };
        if rc != ERROR_SUCCESS {
            break;
        }
        if data_len >= 1 && data[0] & 1 != 0 {
            set.insert(
                super::util::from_wide_nul(&name[..name_len as usize]).to_ascii_lowercase(),
            );
        }
    }
    unsafe { RegCloseKey(key) };
    set
}

fn startup_folder(common: bool) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::UI::Shell::{
        SHGetKnownFolderPath, FOLDERID_CommonStartup, FOLDERID_Startup,
    };
    use windows_sys::Win32::System::Com::CoTaskMemFree;

    let id = if common {
        &FOLDERID_CommonStartup
    } else {
        &FOLDERID_Startup
    };
    let mut out: *mut u16 = std::ptr::null_mut();
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut out) };
    if hr != 0 || out.is_null() {
        return None;
    }
    let mut len = 0usize;
    while unsafe { *out.add(len) } != 0 {
        len += 1;
    }
    let s = super::util::from_wide_nul(unsafe { std::slice::from_raw_parts(out, len) });
    unsafe { CoTaskMemFree(out.cast()) };
    Some(std::path::PathBuf::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a record through the real `StartupApproved` key and read it
    /// back the way [`enumerate`] does.
    ///
    /// This is the whole risk in enabling and disabling: the write can succeed
    /// against the wrong key, or under the wrong name, and Windows will simply
    /// ignore it -- a silent no-op that looks like a working feature. So it is
    /// checked against the registry itself rather than against a mock.
    ///
    /// Under `HKEY_CURRENT_USER`, so no administrator rights are needed, and under
    /// a name that matches no `Run` value, so Explorer has nothing to act on. The
    /// value is removed again whatever happens: a test that leaves a mark on the
    /// machine it ran on is a bug.
    #[test]
    fn writes_a_record_windows_reads_back() {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegDeleteValueW, RegOpenKeyExW, KEY_SET_VALUE,
        };

        struct Scratch(Vec<u16>);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let wpath = wide(APPROVED_RUN);
                let mut key: HKEY = std::ptr::null_mut();
                unsafe {
                    if RegOpenKeyExW(
                        HKEY_CURRENT_USER,
                        wpath.as_ptr(),
                        0,
                        KEY_SET_VALUE,
                        &mut key,
                    ) == ERROR_SUCCESS
                    {
                        RegDeleteValueW(key, self.0.as_ptr());
                        RegCloseKey(key);
                    }
                }
            }
        }

        let name = format!("aztm.selftest.{}", std::process::id());
        let _scratch = Scratch(wide(&name));
        let entry = StartupEntry {
            name: Arc::from(name.as_str()),
            command: Arc::from(""),
            source: StartupSource::UserRun,
            enabled: true,
            approval: Arc::from(name.as_str()),
        };
        let key = name.to_ascii_lowercase();

        set_enabled(&entry, false).expect("disabling a per-user entry needs no rights");
        assert!(
            disabled_set(HKEY_CURRENT_USER, APPROVED_RUN).contains(&key),
            "a disabled entry must read back as disabled"
        );

        set_enabled(&entry, true).expect("re-enabling it");
        assert!(
            !disabled_set(HKEY_CURRENT_USER, APPROVED_RUN).contains(&key),
            "an enabled entry must not read back as disabled"
        );
    }

    /// 32-bit `Run` entries are approved under their own key. Reading the 64-bit
    /// one for them reports every disabled 32-bit entry as enabled, which is what
    /// this tab used to do.
    #[test]
    fn keeps_the_32_bit_run_key_apart() {
        assert_ne!(
            StartupSource::MachineRun.approved_key().1,
            StartupSource::MachineRun32.approved_key().1
        );
        // The per-user and all-users startup folders share a subkey name but not a
        // root, so the pair has to differ in the root.
        assert_eq!(
            StartupSource::UserFolder.approved_key().1,
            StartupSource::CommonFolder.approved_key().1
        );
        assert_ne!(
            StartupSource::UserFolder.approved_key().0,
            StartupSource::CommonFolder.approved_key().0
        );
    }
}
