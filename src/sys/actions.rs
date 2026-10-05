//! Actions taken against a process: end task, change priority, open its folder.
//!
//! Every one of these can legitimately fail (protected processes, insufficient
//! rights), so they all return a message the UI can surface rather than
//! silently doing nothing.

use super::util::{wide, Handle};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Threading::{
    OpenProcess, SetPriorityClass, TerminateProcess, ABOVE_NORMAL_PRIORITY_CLASS,
    BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    PROCESS_SET_INFORMATION, PROCESS_TERMINATE, REALTIME_PRIORITY_CLASS,
};

pub type ActionResult = Result<String, String>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Realtime,
    High,
    AboveNormal,
    Normal,
    BelowNormal,
    Low,
}

impl Priority {
    pub const ALL: [Priority; 6] = [
        Priority::Realtime,
        Priority::High,
        Priority::AboveNormal,
        Priority::Normal,
        Priority::BelowNormal,
        Priority::Low,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Realtime => "Realtime",
            Self::High => "High",
            Self::AboveNormal => "Above normal",
            Self::Normal => "Normal",
            Self::BelowNormal => "Below normal",
            Self::Low => "Low",
        }
    }

    fn class(self) -> u32 {
        match self {
            Self::Realtime => REALTIME_PRIORITY_CLASS,
            Self::High => HIGH_PRIORITY_CLASS,
            Self::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
            Self::Normal => NORMAL_PRIORITY_CLASS,
            Self::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
            Self::Low => IDLE_PRIORITY_CLASS,
        }
    }

}

/// Terminate a process.
pub fn end_task(pid: u32, name: &str) -> ActionResult {
    if pid == 0 || pid == 4 {
        return Err("The system process cannot be ended.".into());
    }

    let Some(h) = Handle::new(unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) }) else {
        return Err(format!(
            "Cannot open {name} (PID {pid}): access denied. Try running elevated."
        ));
    };
    // Exit code 1 matches what Task Manager reports for a killed process.
    if unsafe { TerminateProcess(h.raw(), 1) } == 0 {
        return Err(format!("Failed to end {name} (PID {pid})."));
    }
    Ok(format!("Ended {name} (PID {pid})."))
}

/// Terminate a process and everything below it, deepest first.
pub fn end_tree(targets: &[(u32, String)]) -> ActionResult {
    let mut ok = 0usize;
    let mut failed = 0usize;
    for (pid, _) in targets {
        let Some(h) = Handle::new(unsafe { OpenProcess(PROCESS_TERMINATE, 0, *pid) }) else {
            failed += 1;
            continue;
        };
        if unsafe { TerminateProcess(h.raw(), 1) } == 0 {
            failed += 1;
        } else {
            ok += 1;
        }
    }
    if failed == 0 {
        Ok(format!("Ended {ok} process(es)."))
    } else if ok == 0 {
        Err(format!("Could not end {failed} process(es): access denied."))
    } else {
        Ok(format!("Ended {ok}; {failed} could not be ended."))
    }
}

pub fn set_priority(pid: u32, name: &str, prio: Priority) -> ActionResult {
    let Some(h) = Handle::new(unsafe { OpenProcess(PROCESS_SET_INFORMATION, 0, pid) }) else {
        return Err(format!("Cannot open {name} (PID {pid}): access denied."));
    };
    if unsafe { SetPriorityClass(h.raw(), prio.class()) } == 0 {
        return Err(format!(
            "Failed to set priority on {name}: the process may be protected."
        ));
    }
    Ok(format!("{name} priority set to {}.", prio.label()))
}

/// Open Explorer with the executable selected.
pub fn open_file_location(path: &str) -> ActionResult {
    if path.is_empty() {
        return Err("Image path is unknown for this process.".into());
    }
    // Quote the path so spaces do not split the argument.
    let args = wide(&format!("/select,\"{path}\""));
    let file = wide("explorer.exe");
    shell_exec(&file, Some(&args), None)?;
    Ok(String::new())
}

/// Open the standard file properties dialog.
pub fn open_properties(path: &str) -> ActionResult {
    if path.is_empty() {
        return Err("Image path is unknown for this process.".into());
    }
    use windows_sys::Win32::UI::Shell::{SHObjectProperties, SHOP_FILEPATH};
    let wpath = wide(path);
    let ok = unsafe {
        SHObjectProperties(
            std::ptr::null_mut(),
            SHOP_FILEPATH as u32, // SHOP_TYPE is i32; the API parameter is u32
            wpath.as_ptr(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err("Could not open properties.".into());
    }
    Ok(String::new())
}

/// Relaunch this app elevated. Returns `Ok` once the UAC prompt is accepted; the
/// caller is expected to exit.
pub fn relaunch_elevated() -> ActionResult {
    let exe = std::env::current_exe().map_err(|e| format!("Cannot locate own image: {e}"))?;
    let file = wide(&exe.to_string_lossy());
    let verb = wide("runas");
    shell_exec(&file, None, Some(&verb))?;
    Ok(String::new())
}

fn shell_exec(file: &[u16], args: Option<&[u16]>, verb: Option<&[u16]>) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let h = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.map_or(std::ptr::null(), |v| v.as_ptr()),
            file.as_ptr(),
            args.map_or(std::ptr::null(), |a| a.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a pseudo-HINSTANCE; anything <= 32 is an error code.
    if (h as isize) <= 32 {
        // 1223 == ERROR_CANCELLED, i.e. the user dismissed the UAC prompt.
        if h as isize == 1223 {
            return Err("Cancelled.".into());
        }
        return Err("The shell refused the request.".into());
    }
    Ok(())
}

/// Is this process running with an elevated token?
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
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
