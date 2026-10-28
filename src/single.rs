//! Single instance: a named event lets a second launch ask the running OpenPen to show
//! its window, then exit.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui;
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject, INFINITE};

/// The "show yourself" event, owned by the first instance.
pub struct Instance(HANDLE);

/// Returns `None` (after waking the running instance) if OpenPen is already running.
pub fn claim() -> Option<Instance> {
    unsafe {
        let event = CreateEventW(None, false, false, w!(r"Local\OpenPen.Show")).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = SetEvent(event);
            return None;
        }
        Some(Instance(event))
    }
}

impl Instance {
    /// Shows the window (via the tray's show flag) whenever another launch signals.
    pub fn listen(self, flags: Arc<crate::tray::Flags>, ctx: egui::Context) {
        // HANDLE isn't Send; the event lives for the whole process, so pass it as a number.
        let raw = self.0 .0 as usize;
        std::thread::spawn(move || loop {
            unsafe { WaitForSingleObject(HANDLE(raw as *mut _), INFINITE) };
            flags.show.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }
}
