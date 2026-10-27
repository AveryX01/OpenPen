//! System tray icon: left-click or "Show" brings the window back, "Quit" exits.
//!
//! Tray callbacks run outside the egui frame, so they only set flags and wake the
//! UI; `App::logic` acts on them (it still runs while the window is hidden).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Default)]
pub struct Flags {
    pub show: AtomicBool,
    pub quit: AtomicBool,
}

pub struct Tray {
    _icon: TrayIcon,
    _items: [MenuItem; 2],
    pub flags: Arc<Flags>,
}

impl Tray {
    pub fn take_show(&self) -> bool {
        self.flags.show.swap(false, Ordering::Relaxed)
    }

    pub fn take_quit(&self) -> bool {
        self.flags.quit.swap(false, Ordering::Relaxed)
    }
}

pub fn create(ctx: &egui::Context) -> Option<Tray> {
    let show = MenuItem::new("Show OpenPen", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::with_items(&[&show, &quit]).ok()?;
    let flags = Arc::new(Flags::default());

    {
        let (flags, ctx) = (flags.clone(), ctx.clone());
        let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if e.id == show_id {
                flags.show.store(true, Ordering::Relaxed);
            } else if e.id == quit_id {
                flags.quit.store(true, Ordering::Relaxed);
            }
            ctx.request_repaint();
        }));
    }
    {
        let (flags, ctx) = (flags.clone(), ctx.clone());
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                flags.show.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            }
        }));
    }

    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("OpenPen")
        .with_icon(icon())
        .build()
        .ok()?;
    Some(Tray { _icon: icon, _items: [show, quit], flags })
}

/// A small blue ring with a dot, drawn in code so there's no asset file to ship.
fn icon() -> Icon {
    const N: u32 = 32;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
            let r = (dx * dx + dy * dy).sqrt();
            let on = (11.0..15.0).contains(&r) || r < 4.5;
            rgba.extend_from_slice(if on { &[90, 150, 255, 255] } else { &[0, 0, 0, 0] });
        }
    }
    Icon::from_rgba(rgba, N, N).expect("valid icon size")
}
