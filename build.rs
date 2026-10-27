//! Build script: bakes the OpenPen icon into the exe so Explorer, the taskbar and
//! download pages show it. Cargo runs this before compiling the crate.

#[path = "src/icon.rs"]
mod icon;

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("openpen.ico");
    std::fs::write(&ico, icon::ico(&[16, 24, 32, 48, 64, 128, 256])).expect("write icon");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap());
    res.compile().expect("embed icon (needs the Windows SDK's rc.exe)");
}
