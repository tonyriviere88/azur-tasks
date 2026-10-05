//! Enumerates visible top-level windows so processes can be grouped into
//! "Apps" the way Task Manager does, and so app rows can show a window title.

use super::util::from_wide_nul;
use rustc_hash::FxHashMap;
use std::sync::Arc;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HWND, LPARAM, TRUE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW,
};

pub struct WindowList {
    /// pid -> title of its most relevant visible window.
    titles: FxHashMap<u32, Arc<str>>,
}

impl WindowList {
    pub fn new() -> Self {
        Self {
            titles: FxHashMap::default(),
        }
    }

    pub fn refresh(&mut self) {
        self.titles.clear();
        // Pass the map by pointer; EnumWindows is synchronous so the borrow
        // cannot outlive this call.
        let ptr = &mut self.titles as *mut FxHashMap<u32, Arc<str>>;
        unsafe {
            EnumWindows(Some(enum_proc), ptr as LPARAM);
        }
    }

    #[inline]
    pub fn title_for(&self, pid: u32) -> Option<Arc<str>> {
        self.titles.get(&pid).cloned()
    }
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let map = &mut *(lparam as *mut FxHashMap<u32, Arc<str>>);

    // Task Manager's notion of an "app window": visible, unowned, not a tool
    // window, and actually titled.
    if IsWindowVisible(hwnd) == 0 {
        return TRUE;
    }
    if !GetWindow(hwnd, GW_OWNER).is_null() {
        return TRUE;
    }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW != 0 {
        return TRUE;
    }

    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return TRUE;
    }

    // UWP apps park a cloaked ApplicationFrameWindow per suspended app; without
    // this filter the Apps list fills with entries that are not on screen.
    if is_cloaked(hwnd) {
        return TRUE;
    }

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == 0 {
        return TRUE;
    }

    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    if n <= 0 {
        return TRUE;
    }
    let title = from_wide_nul(&buf[..n as usize]);
    if title.is_empty() {
        return TRUE;
    }

    // First visible window wins; good enough for a label.
    map.entry(pid).or_insert_with(|| Arc::from(title.as_str()));
    TRUE
}

fn is_cloaked(hwnd: HWND) -> bool {
    use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};

    let mut cloaked: u32 = 0;
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32, // DWMWINDOWATTRIBUTE is i32; the API takes u32
            (&mut cloaked as *mut u32).cast(),
            4,
        )
    };
    hr == 0 && cloaked != 0
}
