//! Wacom One (CTL-672) protocol, worked out from the USB captures in `captures/`.
//!
//!   * SET_FEATURE report 0x02 = [02] switches the tablet from its built-in
//!     relative-mouse mode into pen-report mode.
//!   * Pen report, ID 0x02, 10 bytes:
//!       [0]     0x02
//!       [1]     status: 0x40 in range, 0x20 close hover, 0x01 tip,
//!               0x02 near-tip button, 0x04 far button (0x80 alone = out of range)
//!       [2..4]  X, u16 LE, 0..=21600
//!       [4..6]  Y, u16 LE, 0..=13500
//!       [6..8]  pressure, u16 LE, 0..=2047
//!       [8..10] hover height / signal strength (higher = closer)

use hidapi::{HidApi, HidDevice, HidResult};

const VID: u16 = 0x056A;
const PID: u16 = 0x037B;
/// Usage page of the tablet's own vendor-defined pen collection.
const VENDOR_PAGE: u16 = 0xFF0D;

pub const TABLET_W: u32 = 21600;
pub const TABLET_H: u32 = 13500;
pub const PRESSURE_MAX: u16 = 2047;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct PenSample {
    pub in_range: bool,
    pub tip: bool,
    pub near_button: bool,
    pub far_button: bool,
    pub x: u16,
    pub y: u16,
    pub pressure: u16,
}

impl PenSample {
    pub fn parse(report: &[u8]) -> Option<Self> {
        if report.len() < 10 || report[0] != 0x02 {
            return None;
        }
        let st = report[1];
        let word = |i: usize| u16::from_le_bytes([report[i], report[i + 1]]);
        let in_range = st & 0x40 != 0;
        Some(PenSample {
            in_range,
            tip: in_range && st & 0x01 != 0,
            near_button: in_range && st & 0x02 != 0,
            far_button: in_range && st & 0x04 != 0,
            x: word(2),
            y: word(4),
            pressure: word(6),
        })
    }
}

/// Raw pressure shaped by the user's curve, as 0.0..=1.0. Full force still reaches 1.0.
pub fn shaped_pressure(raw: u16, curve: f32) -> f32 {
    let norm = raw.min(PRESSURE_MAX) as f32 / PRESSURE_MAX as f32;
    norm.powf(curve.max(1.0))
}

/// Opens the tablet's vendor pen collection. Only visible with Wacom's driver uninstalled.
pub fn open(api: &mut HidApi) -> Option<HidDevice> {
    let _ = api.refresh_devices();
    let info = api
        .device_list()
        .find(|d| d.vendor_id() == VID && d.product_id() == PID && d.usage_page() == VENDOR_PAGE)?;
    api.open_path(info.path()).ok()
}

/// Switches the tablet from its built-in relative mouse into sending pen reports.
/// It stays that way until unplugged (no known command switches it back).
pub fn enable_pen_reports(dev: &HidDevice) -> HidResult<()> {
    dev.send_feature_report(&[0x02, 0x02])
}
