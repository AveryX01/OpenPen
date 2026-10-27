//! Which app is focused, and which apps have windows open (for the profile app picker).

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible,
};

/// Lowercase exe file name owning a window, e.g. "blender.exe".
fn exe_of(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Some(path.rsplit('\\').next()?.to_lowercase())
    }
}

pub fn foreground_exe() -> Option<String> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return None;
    }
    exe_of(hwnd)
}

unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd).as_bool() && GetWindowTextLengthW(hwnd) > 0 {
        let list = &mut *(list.0 as *mut Vec<HWND>);
        list.push(hwnd);
    }
    true.into()
}

/// Exe names of apps with a visible, titled window, sorted and deduplicated.
pub fn running_apps() -> Vec<String> {
    let mut windows: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut windows as *mut _ as isize));
    }
    let mut exes: Vec<String> = windows.into_iter().filter_map(exe_of).collect();
    exes.sort();
    exes.dedup();
    exes
}
