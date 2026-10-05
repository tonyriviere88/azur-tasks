//! Windows service enumeration for the Services tab.

use super::util::from_wide_nul;
use std::sync::Arc;

use windows_sys::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, ENUM_SERVICE_STATUS_PROCESSW,
    SC_ENUM_PROCESS_INFO, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_ACTIVE, SERVICE_INACTIVE,
    SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_WIN32,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Stopped,
    Pending,
}

impl ServiceState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Stopped => "Stopped",
            Self::Pending => "Pending",
        }
    }
}

#[derive(Clone)]
pub struct Service {
    pub name: Arc<str>,
    pub display_name: Arc<str>,
    pub pid: u32,
    pub state: ServiceState,
    /// Service group, read from the registry (Task Manager's "Group" column).
    pub group: Option<Arc<str>>,
}

/// Enumerate all Win32 services. Needs only `SC_MANAGER_ENUMERATE_SERVICE`,
/// which is granted to standard users.
pub fn enumerate() -> Vec<Service> {
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ENUMERATE_SERVICE) };
    if scm.is_null() {
        return Vec::new();
    }
    struct Scm(windows_sys::Win32::System::Services::SC_HANDLE);
    impl Drop for Scm {
        fn drop(&mut self) {
            unsafe { CloseServiceHandle(self.0) };
        }
    }
    let scm = Scm(scm);

    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];

    // Two passes: active and inactive. Asking for both at once is possible but
    // makes the "did the buffer grow" logic harder to get right.
    for state_filter in [SERVICE_ACTIVE, SERVICE_INACTIVE] {
        let mut resume: u32 = 0;
        loop {
            let mut needed = 0u32;
            let mut returned = 0u32;
            let ok = unsafe {
                EnumServicesStatusExW(
                    scm.0,
                    SC_ENUM_PROCESS_INFO,
                    SERVICE_WIN32,
                    state_filter,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut needed,
                    &mut returned,
                    &mut resume,
                    std::ptr::null(),
                )
            };

            // ok == 0 with `needed` set means "buffer too small"; grow and retry
            // the same resume point rather than losing entries.
            if ok == 0 && returned == 0 {
                if needed as usize > buf.len() {
                    buf.resize(needed as usize, 0);
                    continue;
                }
                break;
            }

            let entries = buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>();
            for i in 0..returned as usize {
                let e: ENUM_SERVICE_STATUS_PROCESSW =
                    unsafe { std::ptr::read_unaligned(entries.add(i)) };
                let name = unsafe { read_pwstr(e.lpServiceName) };
                let display = unsafe { read_pwstr(e.lpDisplayName) };
                let st = e.ServiceStatusProcess.dwCurrentState;
                let state = match st {
                    SERVICE_RUNNING => ServiceState::Running,
                    SERVICE_STOPPED => ServiceState::Stopped,
                    SERVICE_START_PENDING | SERVICE_STOP_PENDING => ServiceState::Pending,
                    _ => ServiceState::Pending,
                };
                let group = read_group(&name);
                out.push(Service {
                    display_name: Arc::from(if display.is_empty() {
                        name.as_str()
                    } else {
                        display.as_str()
                    }),
                    name: Arc::from(name.as_str()),
                    pid: e.ServiceStatusProcess.dwProcessId,
                    state,
                    group,
                });
            }

            // `ok != 0` means the enumeration finished.
            if ok != 0 {
                break;
            }
        }
    }

    out.sort_by_key(|s| s.name.to_ascii_lowercase());
    out
}

fn read_group(service: &str) -> Option<Arc<str>> {
    use super::util::reg_string;
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;

    let path = format!(r"SYSTEM\CurrentControlSet\Services\{service}");
    reg_string(HKEY_LOCAL_MACHINE, &path, "Group")
        .filter(|g| !g.is_empty())
        .map(Arc::from)
}

unsafe fn read_pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < 1024 && *p.add(len) != 0 {
        len += 1;
    }
    from_wide_nul(std::slice::from_raw_parts(p, len))
}
