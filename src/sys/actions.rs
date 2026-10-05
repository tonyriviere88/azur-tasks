//! Actions taken against a process: end task, change priority, open its folder.
//!
//! Every one of these can legitimately fail (protected processes, insufficient
//! rights), so they all return a message the UI can surface rather than
//! silently doing nothing.

use super::util::{from_wide_nul, wide, Handle};

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

/// Open the folder holding a file, with the file selected.
pub fn open_file_location(path: &str) -> ActionResult {
    if path.is_empty() {
        return Err("There is no path to open for this item.".into());
    }
    // Quote the path so spaces do not split the argument.
    let args = wide(&format!("/select,\"{path}\""));
    let file = wide("explorer.exe");
    shell_exec(&file, Some(&args), None)?;
    Ok(String::new())
}

/// The executable a command line runs, as a path on disk.
///
/// A process knows its own image path, but the other tables do not: a startup
/// entry and a service each carry a *command line*, and those are not paths.
/// `"C:\Program Files\Foo\foo.exe" /background`, `%SystemRoot%\system32\svchost.exe
/// -k netsvcs` and `C:\Users\me\Start Menu\My App.lnk` are all things this has to
/// take apart before there is a folder to open.
///
/// Quoted is unambiguous. Unquoted is genuinely ambiguous — a space might separate
/// the program from its arguments or might just be a space in a directory name —
/// and Windows resolves it by trying `C:\Program`, then `C:\Program Files\Foo`,
/// then the whole thing, until one names a file it can run. The same order is used
/// here, shortest first, so this agrees with whatever the entry actually starts
/// rather than with a guess about it.
///
/// `None` when nothing along the way exists: a disabled menu item is better than an
/// Explorer window on the wrong folder.
pub fn executable_of(command: &str) -> Option<String> {
    let expanded = expand_env(command.trim());
    let command = expanded.trim();
    if command.is_empty() {
        return None;
    }

    if let Some(rest) = command.strip_prefix('"') {
        return on_disk(rest[..rest.find('"')?].trim());
    }

    // Every space is a candidate end of the path, and so is the end of the string
    // -- a command with no arguments at all is the whole of it.
    let ends = command
        .char_indices()
        .filter(|(_, c)| *c == ' ')
        .map(|(i, _)| i)
        .chain(std::iter::once(command.len()));
    ends.filter_map(|end| on_disk(command[..end].trim_end()))
        .next()
}

/// The path back again if it names a file, with `.exe` supplied when the command
/// line left it off -- `Run` values sometimes do.
fn on_disk(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let p = std::path::Path::new(path);
    if p.is_file() {
        return Some(path.to_owned());
    }
    if p.extension().is_none() {
        let exe = format!("{path}.exe");
        if std::path::Path::new(&exe).is_file() {
            return Some(exe);
        }
    }
    None
}

/// Expand `%VAR%` the way the shell would. Registry startup entries and service
/// image paths are both written with these in them.
fn expand_env(s: &str) -> String {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;

    if !s.contains('%') {
        return s.to_owned();
    }
    let src = wide(s);
    // MAX_PATH is 260, but a long path plus arguments can exceed it and a command
    // line is capped near 32k either way.
    let mut buf = vec![0u16; 2048];
    // Returns the character count including the terminator, or more than the
    // buffer holds when it did not fit.
    let n = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize > buf.len() {
        return s.to_owned();
    }
    from_wide_nul(&buf)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shape a startup entry or a service image path actually comes in.
    /// Checked against files that really exist, because existence on disk is the
    /// whole of how the unquoted case is resolved.
    #[test]
    fn takes_the_executable_out_of_a_command_line() {
        let me = std::env::current_exe().unwrap();
        let me = me.to_string_lossy().to_string();

        // Quoted, which is what an installer writes.
        assert_eq!(
            executable_of(&format!("\"{me}\" --background -x 3")).as_deref(),
            Some(me.as_str())
        );
        // Unquoted with arguments after it.
        assert_eq!(
            executable_of(&format!("{me} --background")).as_deref(),
            Some(me.as_str())
        );
        // Nothing but the path.
        assert_eq!(executable_of(&me).as_deref(), Some(me.as_str()));
        assert_eq!(
            executable_of(&format!("  {me}  ")).as_deref(),
            Some(me.as_str())
        );

        // Environment variables, expanded before anything else is decided.
        let expanded = executable_of(r"%SystemRoot%\system32\notepad.exe /A");
        assert_eq!(
            expanded.as_deref(),
            Some(
                std::path::Path::new(&std::env::var("SystemRoot").unwrap())
                    .join(r"system32\notepad.exe")
                    .to_string_lossy()
                    .as_ref()
            )
        );

        // Nowhere to go rather than somewhere wrong.
        assert_eq!(executable_of(""), None);
        assert_eq!(executable_of("   "), None);
        assert_eq!(executable_of(r"C:\nope\nothing\here.exe --x"), None);
        // An opening quote with no closing one is malformed; not a path.
        assert_eq!(executable_of(&format!("\"{me}")), None);
    }

    /// The case the shortest-first order exists for: a path with spaces in it and
    /// no quotes around it, which is indistinguishable from a program with
    /// arguments until something is looked up on disk.
    #[test]
    fn resolves_an_unquoted_path_that_has_spaces_in_it() {
        let dir = std::env::temp_dir().join("aztm exec test");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("my app.exe");
        std::fs::write(&exe, b"not really an executable").unwrap();
        let path = exe.to_string_lossy().to_string();

        assert_eq!(executable_of(&path).as_deref(), Some(path.as_str()));
        assert_eq!(
            executable_of(&format!("{path} -silent")).as_deref(),
            Some(path.as_str()),
            "the first space is inside the directory name, not before an argument"
        );

        // A command line whose *shorter* prefix also exists must resolve to the
        // shorter one, which is what Windows itself would run.
        let shorter = dir.join("my.exe");
        std::fs::write(&shorter, b"x").unwrap();
        let ambiguous = format!("{} app.exe", dir.join("my").to_string_lossy());
        assert_eq!(
            executable_of(&ambiguous).as_deref(),
            Some(shorter.to_string_lossy().as_ref()),
            "shortest match first, the way CreateProcess resolves it"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Against every startup entry and every service on the machine this runs on.
    ///
    /// The rule cannot be checked against fixed expectations -- no two machines
    /// have the same list -- but it has one invariant that holds everywhere: the
    /// resolution *is* a search of the disk, so anything it returns must be a file
    /// that is really there. Returning a plausible path that is not is the failure
    /// that would send Explorer somewhere wrong. Run with `--nocapture` for the
    /// tally and the ones it could not place.
    #[test]
    fn never_returns_a_path_that_is_not_there() {
        let mut checked = 0;
        let mut placed = 0;

        for e in crate::sys::startup::enumerate() {
            checked += 1;
            match executable_of(&e.command) {
                Some(p) => {
                    placed += 1;
                    assert!(
                        std::path::Path::new(&p).is_file(),
                        "startup {}: resolved {p:?}, which is not a file",
                        e.name
                    );
                }
                None => println!("startup  unplaced  {:<28} {}", &*e.name, &*e.command),
            }
        }
        let startup = (placed, checked);

        for s in crate::sys::services::enumerate() {
            let Some(cmd) = crate::sys::services::image_path(&s.name) else {
                continue;
            };
            checked += 1;
            match executable_of(&cmd) {
                Some(p) => {
                    placed += 1;
                    assert!(
                        std::path::Path::new(&p).is_file(),
                        "service {}: resolved {p:?}, which is not a file",
                        s.name
                    );
                }
                None => println!("service  unplaced  {:<28} {cmd}", &*s.name),
            }
        }

        println!(
            "startup {}/{} placed, services {}/{} placed",
            startup.0,
            startup.1,
            placed - startup.0,
            checked - startup.1
        );
        assert!(placed > 0, "nothing at all resolved, which cannot be right");
    }

    /// A shortcut is the item, so its own folder is the one to open.
    #[test]
    fn treats_a_shortcut_as_the_thing_it_is() {
        let dir = std::env::temp_dir().join("aztm lnk test");
        std::fs::create_dir_all(&dir).unwrap();
        let lnk = dir.join("Some App.lnk");
        std::fs::write(&lnk, b"x").unwrap();
        let path = lnk.to_string_lossy().to_string();

        assert_eq!(executable_of(&path).as_deref(), Some(path.as_str()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
