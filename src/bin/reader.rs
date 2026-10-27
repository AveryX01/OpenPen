//! reader: talks to the Wacom One (CTL-672) directly, no Wacom software needed.
//!
//! Protocol (worked out from USB captures in `captures/`):
//!   * Startup: SET_FEATURE report 0x02 = [02] switches the tablet from its
//!     default relative-mouse mode into pen-report mode.
//!   * Pen report, ID 0x02, 10 bytes:
//!       [0]    0x02
//!       [1]    status: 0x40 in range, 0x20 close hover, 0x01 tip,
//!              0x02 near-tip button, 0x04 far button (0x80 alone = out of range)
//!       [2..4] X, u16 LE, 0..=21600
//!       [4..6] Y, u16 LE, 0..=13500
//!       [6..8] pressure, u16 LE, 0..=2047
//!       [8..10] hover height / signal strength (higher = closer)
//!
//! Read-only for now: prints decoded samples, doesn't move the cursor.

use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

const VID: u16 = 0x056A;
const PID: u16 = 0x037B;
/// Usage page of the tablet's own vendor-defined pen collection.
const VENDOR_PAGE: u16 = 0xFF0D;

pub const TABLET_W: u16 = 21600;
pub const TABLET_H: u16 = 13500;
pub const PRESSURE_MAX: u16 = 2047;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct PenSample {
    in_range: bool,
    close: bool,
    tip: bool,
    near_button: bool,
    far_button: bool,
    x: u16,
    y: u16,
    pressure: u16,
    height: u16,
}

impl PenSample {
    fn parse(report: &[u8]) -> Option<Self> {
        if report.len() < 10 || report[0] != 0x02 {
            return None;
        }
        let st = report[1];
        let word = |i: usize| u16::from_le_bytes([report[i], report[i + 1]]);
        Some(PenSample {
            in_range: st & 0x40 != 0,
            close: st & 0x20 != 0,
            tip: st & 0x01 != 0,
            near_button: st & 0x02 != 0,
            far_button: st & 0x04 != 0,
            x: word(2),
            y: word(4),
            pressure: word(6),
            height: word(8),
        })
    }

    /// Everything except position/pressure/height, to spot state changes.
    fn state(&self) -> (bool, bool, bool, bool, bool) {
        (self.in_range, self.close, self.tip, self.near_button, self.far_button)
    }
}

fn open_tablet(api: &HidApi) -> Option<HidDevice> {
    let mut found = None;
    for d in api.device_list() {
        if d.vendor_id() != VID || d.product_id() != PID {
            continue;
        }
        println!(
            "  interface={} usage_page=0x{:04X} usage=0x{:02X}  {}",
            d.interface_number(),
            d.usage_page(),
            d.usage(),
            d.path().to_string_lossy()
        );
        if d.usage_page() == VENDOR_PAGE && found.is_none() {
            found = Some(d.path().to_owned());
        }
    }
    let path = found?;
    match api.open_path(&path) {
        Ok(dev) => Some(dev),
        Err(e) => {
            println!("found the pen collection but couldn't open it: {e}");
            None
        }
    }
}

fn main() {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            eprintln!("hidapi init failed: {e}");
            return;
        }
    };

    println!("Wacom HID collections:");
    let Some(dev) = open_tablet(&api) else {
        println!(
            "\nNo vendor pen collection (usage page 0x{VENDOR_PAGE:04X}) found.\n\
             If the Wacom driver is still installed it hides this collection;\n\
             uninstall it and replug the tablet, then try again."
        );
        return;
    };

    match dev.send_feature_report(&[0x02, 0x02]) {
        Ok(()) => println!("\nswitched tablet to pen mode"),
        Err(e) => println!("\nmode switch failed ({e}), reading anyway"),
    }
    println!("reading... (Ctrl+C to quit; replug the tablet to return it to mouse mode)\n");

    let mut buf = [0u8; 64];
    let mut last_state = None;
    let mut last_print = Instant::now();
    loop {
        let n = match dev.read_timeout(&mut buf, 1000) {
            Ok(n) => n,
            Err(e) => {
                println!("\nread failed: {e}");
                return;
            }
        };
        let Some(s) = PenSample::parse(&buf[..n]) else {
            continue;
        };

        let changed = last_state != Some(s.state());
        if changed || last_print.elapsed() >= Duration::from_millis(100) {
            let line = format!(
                "{:<8} tip={} near={} far={}  x={:>5} y={:>5}  pressure={:>4}  height={:>2}",
                if !s.in_range { "OUT" } else if s.close { "hover" } else { "far" },
                s.tip as u8,
                s.near_button as u8,
                s.far_button as u8,
                s.x,
                s.y,
                s.pressure,
                s.height
            );
            // State changes get their own line; continuous updates overwrite in place.
            if changed {
                println!("\r{line}");
            } else {
                print!("\r{line}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            last_state = Some(s.state());
            last_print = Instant::now();
        }
    }
}
