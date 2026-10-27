# OpenPen

A driver-free, Rust-based replacement for the Wacom software, mostly made for myself and for the **Wacom One (CTL-672)** tablet.

![OpenPen demo](docs/demo.gif)

## Why

The official Wacom software is unstable and missing things I wanted, mainly a radial menu that can *hold* modifier keys (e.g. keep Shift held while middle-dragging to pan in Blender). OpenPen talks to the tablet directly, so the Wacom driver and Wacom Center can be uninstalled entirely.

## Features

- **Pen mode:** absolute pen input with pressure, mapped to the main monitor. Turn it off for relative, mouse-like movement.
- **Pressure curve:** 1–4, so full force still reaches 100% but light presses give less.
- **Button mapping** for both pen buttons: mouse clicks, key combos (with key capture), or a radial menu. Hold or Toggle, optionally only while the tip is down.
- **Per-app profiles:** bindings switch automatically based on the focused app (e.g. `blender.exe`), with "No profile" as the fallback.
- **Radial menus:** separate named menus with 2–8 slices and a live clickable editor. Hold a pen button to show the ring, point, and release to pick (or tap slices with the tip). Slices are **Tap** (fires once) or **Toggle** (stays held, shown as `(On)`/`(Off)` and in purple when on).
- **Tray icon:** minimizing hides OpenPen to the system tray.
- **Self-contained:** settings live in a `SaveData` folder next to the exe.

## Download and run

1. **Uninstall the Wacom driver/software** (Settings → Apps → "Wacom Tablet"). It hides the tablet's raw interface from other programs. Reboot and replug the tablet.
2. Download `OpenPen.exe` from the [Releases](../../releases) page and run it. There is no installer; it creates `SaveData\` next to itself.

The tablet only works as a pen while OpenPen is running. Replug it after closing OpenPen to get its built-in mouse mode back.

## Build from source

Requirements: Windows 10/11, a Wacom One CTL-672, and the [Rust toolchain](https://rustup.rs).

```
cargo run --release          # build and run
cargo build --release        # just build: target\release\OpenPen.exe
```

The release exe is standalone (no DLLs or runtime to ship), so `target\release\OpenPen.exe` is the file to share.

Extra binaries used while reverse engineering the tablet:

- `cargo run --bin reader` prints decoded pen reports.
- `cargo run --bin penprobe` logs raw HID and mouse events.

## How it works

The tablet's USB interface is mostly vendor-defined, which is why Windows needs Wacom's driver to treat it as a pen. The protocol was worked out from USB captures (USBPcap + Wireshark):

- Sending HID feature report `0x02 = [02]` switches the tablet from its built-in relative-mouse mode into pen-report mode. No command to switch back is known; replugging resets it.
- It then sends 10-byte input reports (ID `0x02`) on the vendor collection (usage page `0xFF0D`):

| Bytes | Meaning |
|---|---|
| 1 | status: `0x40` in range, `0x20` close hover, `0x01` tip, `0x02` near-tip button, `0x04` far button |
| 2–3 | X, little-endian, 0–21600 |
| 4–5 | Y, little-endian, 0–13500 |
| 6–7 | pressure, 0–2047 |
| 8–9 | hover height |

## Architecture (for contributors and AI agents)

Two threads share two `Arc<Mutex<_>>` values: `Config` (GUI → worker) and `Status` (worker → GUI).

- The **worker thread** owns the tablet and produces *all* input. Each report is parsed, then the active profile's bindings are applied and pen/mouse/key input is sent to Windows.
- The **GUI thread** only edits a local copy of `Config` (pushed and saved when it changes), shows `Status`, and draws the radial overlay the worker opens.

| File | Role |
|---|---|
| `src/main.rs` | egui app: Main and Radial menus tabs, profile/app picker, button and slice editors (`action_editor`), key capture, tray/minimize handling, starts the worker. |
| `src/worker.rs` | Tablet session loop and all input logic. `Outputs` holds pen state, `ButtonState` per pen button (Hold/Toggle), radial menu state (open/hover/pick), latched toggles, and absolute vs relative movement. Also picks the profile from the focused exe. |
| `src/tablet.rs` | The protocol: `PenSample::parse`, opening the vendor HID collection, the mode-switch report, the pressure curve. |
| `src/inject.rs` | Output to Windows: synthetic pen (`PenOutput`, `InjectSyntheticPointerInput`), mouse moves and clicks, and key combos via `SendInput` (with scan codes), plus key naming and capture helpers. |
| `src/config.rs` | Serializable settings: `Config` (global options, default `Bindings`, `Profile`s, `RadialMenu`s), `ButtonMap`/`Action`/`Mode`. Saved as `SaveData/config.json` and `SaveData/radials.json`. |
| `src/radial.rs` | Radial geometry (`slice_at`), the ring painter (`draw_ring`, shared by overlay and editor preview), labels/colors, and the overlay window. |
| `src/apps.rs` | Focused app's exe name and the list of apps with open windows. |
| `src/tray.rs` | System tray icon and menu. |
| `src/bin/*.rs` | Standalone reverse-engineering tools (not part of the app). |

Non-obvious decisions worth knowing before changing things:

- **Hover moves the plain mouse cursor; the Windows pen is only used while the tip is down.** Windows ignores mouse clicks while a pen is in range, which dropped mapped button clicks. Mouse-button mappings and an open radial menu also force mouse movement, so apps see normal drags.
- **The radial overlay is one persistent window** parked off-screen when closed. Creating a window per open made it slide in. It's click-through, `WS_EX_NOACTIVATE`, and kept alive while the main window is hidden via `RequestPaintWhileHidden`.
- **Picking restores focus.** The window that was focused when the menu opened gets focus back before keys are sent.
- **Toggles release safely.** Toggled slices are tracked by `Action` value, and auto-released if their slice is edited or removed. "Release all" in the Main tab and quitting also release them.
- Sizes in `radial.rs` are 96-DPI units: egui points in the GUI, scaled by system DPI in the worker.

## Notes

- Only tested on my own CTL-672. Other Wacom models likely use different report layouts.
- Not affiliated with Wacom.

## License

Free to use, modify and fork for any non-commercial purpose; it may not be used in paid products. See [LICENSE](LICENSE).
