//! penprobe: step 1 of the Wacom replacement app.
//!
//! Logs what the pen actually reports so we know which signals we can build on:
//!   * RAW  - HID digitizer reports (Raw Input), parsed into named button usages.
//!   * MOUSE - button clicks seen by a global low-level mouse hook, tagged when
//!             they come from the pen (Wacom driver) rather than a real mouse.
//! Nothing is swallowed or changed; this is read-only.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ffi::c_void;
use std::mem::size_of;
use std::time::Instant;

use windows::core::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const PAGE_DIGITIZER: u16 = 0x0D;
const PAGE_BUTTON: u16 = 0x09;


/// dwExtraInfo signature Windows/Wacom put on pen-generated mouse events.
const MI_WP_SIGNATURE: usize = 0xFF51_5700;
const SIGNATURE_MASK: usize = 0xFFFF_FF00;

struct State {
    start: Instant,
    preparsed: HashMap<isize, Vec<u8>>,
    last: HashMap<isize, BTreeSet<(u16, u16)>>,
    last_status: HashMap<isize, Option<u8>>,
    mouse_is_wacom: HashMap<isize, bool>,
}
thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        start: Instant::now(),
        preparsed: HashMap::new(),
        last: HashMap::new(),
        last_status: HashMap::new(),
        mouse_is_wacom: HashMap::new(),
    });
}

fn stamp() -> u128 {
    STATE.with(|s| s.borrow().start.elapsed().as_millis())
}

fn usage_name(page: u16, usage: u16) -> String {
    match (page, usage) {
        (PAGE_DIGITIZER, 0x32) => "InRange".into(),
        (PAGE_DIGITIZER, 0x3C) => "Invert".into(),
        (PAGE_DIGITIZER, 0x42) => "TipSwitch".into(),
        (PAGE_DIGITIZER, 0x44) => "BarrelSwitch(primary)".into(),
        (PAGE_DIGITIZER, 0x45) => "Eraser".into(),
        (PAGE_DIGITIZER, 0x5A) => "SecondaryBarrel".into(),
        (PAGE_DIGITIZER, u) => format!("Digitizer:0x{u:02X}"),
        (PAGE_BUTTON, u) => format!("Button{u}"),
        (p, u) => format!("0x{p:02X}:0x{u:02X}"),
    }
}

/// Reads the names of all pressed buttons/switches out of one HID input report.
unsafe fn active_usages(preparsed: &[u8], report: &[u8]) -> BTreeSet<(u16, u16)> {
    let mut found = BTreeSet::new();
    for page in [PAGE_DIGITIZER, PAGE_BUTTON] {
        let mut list = [0u16; 32];
        let mut len = list.len() as u32;
        let mut rep = report.to_vec();
        let status = HidP_GetUsages(
            HidP_Input,
            page,
            None,
            list.as_mut_ptr(),
            &mut len,
            PHIDP_PREPARSED_DATA(preparsed.as_ptr() as isize),
            &mut rep,
        );
        if status == HIDP_STATUS_SUCCESS {
            for u in &list[..len as usize] {
                found.insert((page, *u));
            }
        }
    }
    found
}

unsafe fn device_name(dev: HANDLE) -> String {
    let mut size = 0u32;
    GetRawInputDeviceInfoW(Some(dev), RIDI_DEVICENAME, None, &mut size);
    let mut buf = vec![0u16; size as usize];
    GetRawInputDeviceInfoW(Some(dev), RIDI_DEVICENAME, Some(buf.as_mut_ptr() as *mut c_void), &mut size);
    String::from_utf16_lossy(&buf).trim_end_matches('\0').to_string()
}

/// Lists the (usage page, usage) of every HID collection belonging to Wacom (VID 056A).
unsafe fn wacom_collections() -> BTreeSet<(u16, u16)> {
    let list_size = size_of::<RAWINPUTDEVICELIST>() as u32;
    let mut count = 0u32;
    GetRawInputDeviceList(None, &mut count, list_size);
    let mut list = vec![RAWINPUTDEVICELIST::default(); count as usize];
    GetRawInputDeviceList(Some(list.as_mut_ptr()), &mut count, list_size);

    let mut found = BTreeSet::new();
    for d in &list[..count as usize] {
        if d.dwType != RIM_TYPEHID {
            continue;
        }
        let name = device_name(d.hDevice);
        if !name.to_uppercase().contains("VID_056A") {
            continue;
        }
        let mut info = RID_DEVICE_INFO::default();
        info.cbSize = size_of::<RID_DEVICE_INFO>() as u32;
        let mut sz = info.cbSize;
        GetRawInputDeviceInfoW(Some(d.hDevice), RIDI_DEVICEINFO, Some(&mut info as *mut _ as *mut c_void), &mut sz);
        let hid = info.Anonymous.hid;
        println!("found Wacom collection: page=0x{:04X} usage=0x{:02X}  {name}", hid.usUsagePage, hid.usUsage);
        found.insert((hid.usUsagePage, hid.usUsage));
    }
    found
}

unsafe fn load_preparsed(dev: HANDLE) -> Vec<u8> {
    let mut size = 0u32;
    GetRawInputDeviceInfoW(Some(dev), RIDI_PREPARSEDDATA, None, &mut size);
    let mut buf = vec![0u8; size as usize];
    GetRawInputDeviceInfoW(Some(dev), RIDI_PREPARSEDDATA, Some(buf.as_mut_ptr() as *mut c_void), &mut size);
    buf
}

/// The tablet also exposes a mouse-type collection (Windows hides it from the HID
/// list above). Log its button flags, ignoring every other mouse on the system.
unsafe fn on_raw_mouse(dev: HANDLE, body: &[u8]) {
    let key = dev.0 as isize;
    let known = STATE.with(|s| s.borrow().mouse_is_wacom.get(&key).copied());
    let is_wacom = match known {
        Some(v) => v,
        None => {
            let name = device_name(dev);
            let v = name.to_uppercase().contains("VID_056A");
            if v {
                println!("[{:>7}ms] new Wacom MOUSE device: {name}", stamp());
            }
            STATE.with(|s| s.borrow_mut().mouse_is_wacom.insert(key, v));
            v
        }
    };
    if !is_wacom || body.len() < 6 {
        return;
    }
    // RAWMOUSE: usFlags(2) + pad(2) + usButtonFlags(2) + usButtonData(2) ...
    let flags = u16::from_le_bytes([body[4], body[5]]);
    if flags != 0 {
        println!("[{:>7}ms] RAWMOUSE buttonFlags=0x{flags:04X}", stamp());
    }
}

unsafe fn on_raw_input(handle: HRAWINPUT) {
    let hdr_size = size_of::<RAWINPUTHEADER>() as u32;
    let mut size = 0u32;
    GetRawInputData(handle, RID_INPUT, None, &mut size, hdr_size);
    let mut buf = vec![0u8; size as usize];
    let got = GetRawInputData(handle, RID_INPUT, Some(buf.as_mut_ptr() as *mut c_void), &mut size, hdr_size);
    if got == u32::MAX {
        return;
    }

    let header = std::ptr::read_unaligned(buf.as_ptr() as *const RAWINPUTHEADER);
    if header.dwType == RIM_TYPEMOUSE.0 {
        on_raw_mouse(header.hDevice, &buf[hdr_size as usize..]);
        return;
    }
    if header.dwType != RIM_TYPEHID.0 {
        return;
    }
    let body = &buf[hdr_size as usize..];
    let size_hid = u32::from_le_bytes(body[0..4].try_into().unwrap()) as usize;
    let count = u32::from_le_bytes(body[4..8].try_into().unwrap()) as usize;
    let data = &body[8..];
    let dev = header.hDevice;
    let key = dev.0 as isize;

    let first_seen = STATE.with(|s| !s.borrow().preparsed.contains_key(&key));
    if first_seen {
        let pp = load_preparsed(dev);
        println!("[{:>7}ms] new HID device: {}", stamp(), device_name(dev));
        STATE.with(|s| s.borrow_mut().preparsed.insert(key, pp));
    }

    for i in 0..count {
        let report = &data[i * size_hid..(i + 1) * size_hid];
        let usages = STATE.with(|s| {
            let st = s.borrow();
            active_usages(&st.preparsed[&key], report)
        });
        let changed = STATE.with(|s| {
            let mut st = s.borrow_mut();
            let prev = st.last.insert(key, usages.clone());
            // The status byte can carry bits with no HID usage name (e.g. a button
            // the descriptor doesn't declare), so watch it directly as well.
            let status = report.get(1).copied();
            let prev_status = st.last_status.insert(key, status);
            prev.as_ref() != Some(&usages) || prev_status != Some(status)
        });
        if changed {
            let names: Vec<String> = usages.iter().map(|(p, u)| usage_name(*p, *u)).collect();
            let hex: Vec<String> = report.iter().take(12).map(|b| format!("{b:02X}")).collect();
            println!(
                "[{:>7}ms] RAW   buttons=[{}]  report={}",
                stamp(),
                names.join(", "),
                hex.join(" ")
            );
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_INPUT {
        on_raw_input(HRAWINPUT(lparam.0 as *mut c_void));
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let what = match wparam.0 as u32 {
            WM_LBUTTONDOWN => Some("Left down"),
            WM_LBUTTONUP => Some("Left up"),
            WM_RBUTTONDOWN => Some("Right down"),
            WM_RBUTTONUP => Some("Right up"),
            WM_MBUTTONDOWN => Some("Middle down"),
            WM_MBUTTONUP => Some("Middle up"),
            WM_XBUTTONDOWN => Some("X down"),
            WM_XBUTTONUP => Some("X up"),
            WM_MOUSEWHEEL => Some("Wheel"),
            WM_MOUSEHWHEEL => Some("HWheel"),
            _ => None,
        };
        if let Some(what) = what {
            let source = if info.dwExtraInfo & SIGNATURE_MASK == MI_WP_SIGNATURE {
                "PEN"
            } else {
                "mouse"
            };
            println!(
                "[{:>7}ms] MOUSE {what:<11} source={source} extra=0x{:X} data={} injected={}",
                stamp(),
                info.dwExtraInfo,
                (info.mouseData >> 16) as i16,
                info.flags & 1 != 0
            );
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// Logs only F13-F24, the keys we'd assign to pen buttons in Wacom Center.
/// Everything else is ignored so normal typing never reaches the console.
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if (0x7C..=0x87).contains(&info.vkCode) {
            let dir = match wparam.0 as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => "down",
                _ => "up",
            };
            println!(
                "[{:>7}ms] KEY   F{} {dir:<4} injected={}",
                stamp(),
                info.vkCode - 0x7C + 13,
                info.flags.0 & 0x10 != 0
            );
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn main() -> Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("penprobe_window");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("penprobe"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst.into()),
            None,
        )?;

        // Every digitizer-page device (pens, touch) even when we aren't focused.
        let mut devices = vec![RAWINPUTDEVICE {
            usUsagePage: PAGE_DIGITIZER,
            usUsage: 0,
            dwFlags: RIDEV_INPUTSINK | RIDEV_PAGEONLY,
            hwndTarget: hwnd,
        }];
        // Plus every other HID collection the tablet exposes (vendor pages etc.),
        // since the near-tip button doesn't show up on the pen collection.
        for (page, usage) in wacom_collections() {
            if page == PAGE_DIGITIZER {
                continue;
            }
            devices.push(RAWINPUTDEVICE {
                usUsagePage: page,
                usUsage: usage,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            });
        }
        devices.push(RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        });
        RegisterRawInputDevices(&devices, size_of::<RAWINPUTDEVICE>() as u32)?;

        let _hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(hinst.into()), 0)?;
        let _khook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(hinst.into()), 0)?;

        println!("penprobe running. Hover/tap the pen and press both barrel buttons.");
        println!("Ctrl+C to quit.\n");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
