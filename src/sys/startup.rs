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
}

#[derive(Clone)]
pub struct StartupEntry {
    pub name: Arc<str>,
    pub command: Arc<str>,
    pub source: StartupSource,
    /// `false` when Windows has this entry disabled.
    pub enabled: bool,
}

pub fn enumerate() -> Vec<StartupEntry> {
    let mut out = Vec::new();

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";

    let disabled_user = disabled_set(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
    let disabled_machine = disabled_set(HKEY_LOCAL_MACHINE, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
    let disabled_folder = disabled_set(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder");

    for (root, path, source, disabled) in [
        (HKEY_CURRENT_USER, RUN, StartupSource::UserRun, &disabled_user),
        (HKEY_LOCAL_MACHINE, RUN, StartupSource::MachineRun, &disabled_machine),
        (
            HKEY_LOCAL_MACHINE,
            RUN32,
            StartupSource::MachineRun32,
            &disabled_machine,
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
            });
        }
    }

    for (folder, source) in [
        (startup_folder(false), StartupSource::UserFolder),
        (startup_folder(true), StartupSource::CommonFolder),
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
