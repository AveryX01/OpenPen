//! Output side: turns tablet samples into Windows pen input, and mapped
//! actions into mouse buttons / key presses.

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::Pointer::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{KeyCombo, MouseButton};
use crate::tablet::{self, PenSample, TABLET_H, TABLET_W};

/// Windows pen pressure runs 0..=1024.
const WIN_PRESSURE_MAX: u32 = 1024;

pub fn screen_size() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

/// Maps tablet coordinates onto the primary monitor (whole tablet area, stretched).
pub fn to_screen(x: u16, y: u16) -> POINT {
    let (w, h) = screen_size();
    POINT {
        x: (x as i64 * (w - 1) as i64 / TABLET_W as i64) as i32,
        y: (y as i64 * (h - 1) as i64 / TABLET_H as i64) as i32,
    }
}

/// A synthetic Windows pen that mirrors the physical one.
pub struct PenOutput {
    device: HSYNTHETICPOINTERDEVICE,
    in_range: bool,
    down: bool,
    last: POINT,
}

impl PenOutput {
    pub fn new() -> windows::core::Result<Self> {
        let device = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT)? };
        Ok(PenOutput { device, in_range: false, down: false, last: POINT::default() })
    }

    fn send(&self, flags: POINTER_FLAGS, pt: POINT, pressure: u32) {
        let mut pen = POINTER_PEN_INFO {
            pointerInfo: POINTER_INFO {
                pointerType: PT_PEN,
                pointerFlags: flags,
                ptPixelLocation: pt,
                ..Default::default()
            },
            ..Default::default()
        };
        if flags.0 & POINTER_FLAG_INCONTACT.0 != 0 {
            pen.penMask = PEN_MASK_PRESSURE;
            pen.pressure = pressure;
        }
        let info = POINTER_TYPE_INFO { r#type: PT_PEN, Anonymous: POINTER_TYPE_INFO_0 { penInfo: pen } };
        unsafe {
            let _ = InjectSyntheticPointerInput(self.device, &[info]);
        }
    }

    pub fn update(&mut self, s: &PenSample, curve: f32) {
        if !s.in_range {
            self.leave();
            return;
        }
        let pt = to_screen(s.x, s.y);
        let pressure = ((tablet::shaped_pressure(s.pressure, curve) * WIN_PRESSURE_MAX as f32) as u32).max(1);
        let flags = match (self.down, s.tip) {
            (false, true) => POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN,
            (true, true) => POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE,
            (true, false) => POINTER_FLAG_INRANGE | POINTER_FLAG_UP,
            (false, false) => POINTER_FLAG_INRANGE | POINTER_FLAG_UPDATE,
        };
        self.send(flags, pt, pressure);
        self.in_range = true;
        self.down = s.tip;
        self.last = pt;
    }

    /// Lifts the pen (if down) and takes it out of range.
    pub fn leave(&mut self) {
        if self.down {
            self.send(POINTER_FLAG_INRANGE | POINTER_FLAG_UP, self.last, 0);
            self.down = false;
        }
        if self.in_range {
            self.send(POINTER_FLAG_UPDATE, self.last, 0);
            self.in_range = false;
        }
    }
}

impl Drop for PenOutput {
    fn drop(&mut self) {
        self.leave();
        unsafe { DestroySyntheticPointerDevice(self.device) };
    }
}

fn send_inputs(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, dwFlags: flags, ..Default::default() } },
    }
}

/// Moves the real cursor (used instead of the pen while a mouse button is held,
/// so apps see an ordinary mouse drag).
pub fn mouse_move(x: u16, y: u16) {
    let (w, h) = screen_size();
    let pt = to_screen(x, y);
    // Absolute mouse coordinates are normalized to 0..=65535.
    let nx = pt.x * 65535 / (w - 1).max(1);
    let ny = pt.y * 65535 / (h - 1).max(1);
    send_inputs(&[mouse_input(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, nx, ny)]);
}

/// Moves the cursor by a pixel delta, like a real mouse (subject to Windows pointer speed).
pub fn mouse_move_relative(dx: i32, dy: i32) {
    if dx != 0 || dy != 0 {
        send_inputs(&[mouse_input(MOUSEEVENTF_MOVE, dx, dy)]);
    }
}

pub fn mouse_button(button: MouseButton, down: bool) {
    let flags = match (button, down) {
        (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
        (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
        (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
        (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
    };
    send_inputs(&[mouse_input(flags, 0, 0)]);
}

/// Keys that live on the "extended" part of the keyboard (arrows, nav block, etc.).
/// Windows needs to be told, or e.g. Delete turns into numpad Decimal.
fn is_extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0x5D | 0x6F | 0x90 | 0xA3 | 0xA5)
}

fn key_input(vk: u16, up: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        // Include the hardware scan code too: apps reading raw input (Blender, games)
        // ignore key events that only carry a virtual-key code.
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

/// Human-readable key name from Windows ("Page Down", "F5", "A", ...).
pub fn key_name(vk: u16) -> String {
    unsafe {
        let scan = MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC);
        let mut lparam = (scan as i32) << 16;
        if is_extended(vk) {
            lparam |= 1 << 24;
        }
        let mut buf = [0u16; 64];
        let len = GetKeyNameTextW(lparam, &mut buf);
        if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            format!("Key 0x{vk:02X}")
        }
    }
}

/// Modifiers and mouse buttons are chosen separately, so key capture skips them.
fn capturable(vk: u16) -> bool {
    !matches!(vk, 0x01..=0x06 | 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

/// Every capturable key currently held down.
pub fn pressed_keys() -> Vec<u16> {
    (0x08u16..=0xFE)
        .filter(|&vk| capturable(vk) && unsafe { GetAsyncKeyState(vk as i32) } < 0)
        .collect()
}

fn combo_keys(combo: &KeyCombo) -> Vec<u16> {
    let mut keys = Vec::new();
    if combo.ctrl {
        keys.push(VK_CONTROL.0);
    }
    if combo.shift {
        keys.push(VK_SHIFT.0);
    }
    if combo.alt {
        keys.push(VK_MENU.0);
    }
    if combo.win {
        keys.push(VK_LWIN.0);
    }
    keys.extend(combo.key);
    keys
}

/// Presses modifiers first then the key; releases in reverse order.
pub fn key_combo(combo: &KeyCombo, down: bool) {
    let mut keys = combo_keys(combo);
    if !down {
        keys.reverse();
    }
    let inputs: Vec<INPUT> = keys.into_iter().map(|vk| key_input(vk, !down)).collect();
    send_inputs(&inputs);
}
