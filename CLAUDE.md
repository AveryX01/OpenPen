# OpenPen: notes for Claude

Driver-free Rust replacement for the Wacom software, for the Wacom One CTL-672 (no display). The Wacom driver is uninstalled on the dev machine; OpenPen reads the tablet over HID and injects input itself. See README.md for features, the protocol table and the architecture/file map. Don't duplicate it here.

## Working with the user

- Pair programming: the user tests on real hardware and pastes output. Keep steps small, build before handing back, and say plainly what's untested.
- They run via RustRover's play button or `cargo run`. Name non-default binaries explicitly (`cargo run --bin reader`).
- Don't propose anything that depends on Wacom Center, the Wacom driver, or Wintab.
- When behavior is unclear, add temporary logging (to `SaveData/debug.log`) and read the file yourself rather than guessing; remove it afterwards.

## Build

- `cargo run`: debug, a few seconds once deps are cached. Use while iterating.
- `cargo build --release` → `target/release/OpenPen.exe` (standalone, ~11 MB, slow on purpose: LTO). This is what goes on GitHub Releases.
- `build.rs` embeds the icon from `src/icon.rs` using the Windows SDK's `rc.exe`.
- A running OpenPen locks its exe; close it before rebuilding.

## Gotchas learned the hard way

- Without the `[02 02]` feature report the tablet is a plain relative mouse. No known command switches it back; replug resets it.
- Windows drops mouse clicks while a pen is in range → hover is sent as mouse movement, the pen only while the tip is down.
- Injected keys need scan codes (`wScan`) or raw-input apps like Blender ignore them.
- The radial overlay must stay one persistent, non-activating window; recreating it per open made it slide in, and focus theft broke key delivery.
- Radial picks happen on button release (or tip tap while held); toggled slices latch until picked again or "Release all".
- Settings: `SaveData/config.json` (profiles/buttons) and `SaveData/radials.json` next to the exe. `SaveData/` and `captures/` (USB captures with the tablet serial) are gitignored.
