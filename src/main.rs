//! OpenPen: a minimal replacement for the Wacom software, for the Wacom One (CTL-672).
//!
//! The GUI only edits the shared `Config` and shows `Status`; the worker thread
//! talks to the tablet and produces all input. The GUI thread also draws the
//! radial menu overlay the worker opens.

#![windows_subsystem = "windows"]

mod apps;
mod config;
mod icon;
mod inject;
mod radial;
mod single;
mod tablet;
mod tray;
mod worker;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use config::{Action, ButtonMap, Config, KeyCombo, Mode, MouseButton, Profile, RadialMenu};
use worker::Status;

/// Which binding a widget edits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Slot {
    /// A pen button in a profile (`None` = "No profile").
    Near(Option<usize>),
    Far(Option<usize>),
    /// (radial menu index, slice index into `RadialMenu::ring`)
    Slice(usize, usize),
}

fn slot_mut(cfg: &mut Config, slot: Slot) -> Option<&mut ButtonMap> {
    fn bindings(cfg: &mut Config, p: Option<usize>) -> Option<&mut config::Bindings> {
        match p {
            Some(i) => cfg.profiles.get_mut(i).map(|p| &mut p.bindings),
            None => Some(&mut cfg.default),
        }
    }
    match slot {
        Slot::Near(p) => bindings(cfg, p).map(|b| &mut b.near),
        Slot::Far(p) => bindings(cfg, p).map(|b| &mut b.far),
        Slot::Slice(r, i) => cfg.radials.get_mut(r)?.ring_mut(i),
    }
}

/// "Select Key" in progress.
struct Capture {
    slot: Slot,
    /// Keys already down when capture started (e.g. Space from clicking the button),
    /// so they don't count until pressed again.
    held: Vec<u16>,
}

impl Capture {
    fn new(slot: Slot) -> Self {
        Capture { slot, held: inject::pressed_keys() }
    }

    /// Returns `Some(new key)` once a fresh key is pressed; Esc gives `Some(None)`.
    fn poll(&mut self) -> Option<Option<u16>> {
        let now = inject::pressed_keys();
        self.held.retain(|k| now.contains(k));
        let fresh = now.into_iter().find(|k| !self.held.contains(k))?;
        Some(if fresh == 0x1B { None } else { Some(fresh) })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Main,
    Radial,
}

struct App {
    config: Arc<Mutex<Config>>,
    status: Arc<Mutex<Status>>,
    /// Local copy the widgets edit; pushed to the worker and saved when it changes.
    edit: Config,
    tab: Tab,
    /// Profile being edited; `None` is "No profile".
    selected: Option<usize>,
    /// Radial menu and slice being edited.
    radial_sel: usize,
    slice_sel: usize,
    capture: Option<Capture>,
    running_apps: Vec<String>,
    apps_refreshed: Option<Instant>,
    tray: Option<tray::Tray>,
    hidden: bool,
    /// Set while un-hiding from the tray, so a stale "minimized" flag doesn't hide it again.
    restoring: bool,
    overlay_shown: bool,
}

impl eframe::App for App {
    /// Transparent, so the radial overlay window has no background.
    /// The main window is fully covered by its panel anyway.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    /// Runs even while hidden in the tray, so tray clicks are handled here.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep ticking while hidden, to notice when the radial menu opens or closes.
        ctx.request_repaint_after(Duration::from_millis(30));
        let radial_open = self.status.lock().unwrap().radial.is_some();
        if self.hidden && (radial_open || self.overlay_shown) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }

        let Some(tray) = &self.tray else { return };
        if tray.take_quit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if tray.take_show() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.hidden = false;
            self.restoring = true;
        }
        let minimized = ctx.input(|i| i.viewport().minimized);
        if self.restoring {
            if minimized == Some(false) {
                self.restoring = false;
            }
        } else if minimized == Some(true) && !self.hidden {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.hidden = true;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let status = self.status.lock().unwrap().clone();
        self.overlay_shown = radial::show_overlay(ui.ctx(), status.radial.as_ref());
        if status.radial.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        if self.hidden {
            return;
        }

        if let Some(capture) = &mut self.capture {
            if let Some(key) = capture.poll() {
                if let Some(ButtonMap { action: Action::Keys(combo), .. }) = slot_mut(&mut self.edit, capture.slot) {
                    combo.key = key;
                }
                self.capture = None;
            }
        }
        if self.apps_refreshed.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            self.running_apps = apps::running_apps();
            self.apps_refreshed = Some(Instant::now());
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Main, egui::RichText::new("Main").size(15.0));
                ui.selectable_value(&mut self.tab, Tab::Radial, egui::RichText::new("Radial menus").size(15.0));
            });
            ui.separator();
            match self.tab {
                Tab::Main => self.main_tab(ui, &status),
                Tab::Radial => self.radial_tab(ui, &status),
            }
        });

        let mut shared = self.config.lock().unwrap();
        if *shared != self.edit {
            *shared = self.edit.clone();
            config::save(&self.edit);
        }
    }
}

impl App {
    fn main_tab(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
            ui.weak(VERSION);
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| self.main_settings(ui, status));
        });
    }

    fn main_settings(&mut self, ui: &mut egui::Ui, status: &Status) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            status_section(ui, status, self.edit.bindings(status.active_profile).pressure_curve);
            self.held_section(ui, status);
            ui.separator();

            ui.checkbox(&mut self.edit.pen_mode, "Enable Pen Mode");
            if self.edit.pen_mode {
                ui.weak("Pen maps onto the screen, with pressure.");
            } else {
                ui.weak("Pen moves the cursor like a mouse; tip is left click.");
                ui.add(egui::Slider::new(&mut self.edit.mouse_speed, 0.25..=4.0).text("Speed"));
            }
            ui.separator();

            self.profile_section(ui, status.active_profile);
            ui.separator();

            let sel = self.selected;
            let Config { default, profiles, radials, .. } = &mut self.edit;
            let b = match sel {
                Some(i) => &mut profiles[i].bindings,
                None => default,
            };
            ui.add(egui::Slider::new(&mut b.pressure_curve, 1.0..=4.0).text("Pressure curve"));
            ui.heading("Buttons");
            action_editor(ui, "Near-tip button", &mut b.near, Slot::Near(sel), &mut self.capture, radials);
            action_editor(ui, "Far button", &mut b.far, Slot::Far(sel), &mut self.capture, radials);
        });
    }

    /// Keys held by toggled radial slices, with a way out if one gets stuck.
    fn held_section(&mut self, ui: &mut egui::Ui, status: &Status) {
        if status.latched.is_empty() {
            return;
        }
        let held: Vec<String> = status.latched.iter().map(radial::action_label).collect();
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(egui::Color32::from_rgb(170, 120, 235), format!("Held: {}", held.join(", ")));
            if ui.button("Release all").clicked() {
                self.status.lock().unwrap().release_held = true;
            }
        });
    }

    fn profile_section(&mut self, ui: &mut egui::Ui, active: Option<usize>) {
        ui.heading("Profile");
        let name_of = |cfg: &Config, p: Option<usize>| {
            let name = p.map_or("No profile".to_string(), |i| cfg.profiles[i].name.clone());
            if p == active {
                format!("{name}  (active)")
            } else {
                name
            }
        };

        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("profile")
                .width(200.0)
                .selected_text(name_of(&self.edit, self.selected))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.selected, None, name_of(&self.edit, None));
                    for i in 0..self.edit.profiles.len() {
                        ui.selectable_value(&mut self.selected, Some(i), name_of(&self.edit, Some(i)));
                    }
                });
            if ui.button("New").clicked() {
                self.edit.profiles.push(Profile {
                    name: format!("Profile {}", self.edit.profiles.len() + 1),
                    apps: Vec::new(),
                    bindings: self.edit.default.clone(),
                });
                self.selected = Some(self.edit.profiles.len() - 1);
            }
            if let Some(i) = self.selected {
                if ui.button("Delete").clicked() {
                    self.edit.profiles.remove(i);
                    self.selected = None;
                    self.capture = None;
                }
            }
        });

        let Some(i) = self.selected else {
            ui.weak("Used whenever the focused app has no profile.");
            return;
        };
        let profile = &mut self.edit.profiles[i];
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut profile.name);
        });

        ui.label("Apps");
        let mut remove = None;
        for (j, app) in profile.apps.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.small_button("✕").clicked() {
                    remove = Some(j);
                }
                ui.label(app);
            });
        }
        if let Some(j) = remove {
            profile.apps.remove(j);
        }
        let mut add = None;
        egui::ComboBox::from_id_salt(("add_app", i)).selected_text("Add app…").show_ui(ui, |ui| {
            for app in &self.running_apps {
                if app != "openpen.exe" && !profile.apps.contains(app) && ui.selectable_label(false, app).clicked() {
                    add = Some(app.clone());
                }
            }
        });
        profile.apps.extend(add);
    }

    /// Left: the selected slice's settings, with the menu's own settings at the bottom.
    /// Right: a live preview of the ring; click a slice to select it.
    fn radial_tab(&mut self, ui: &mut egui::Ui, status: &Status) {
        if self.edit.radials.is_empty() {
            self.edit.radials.push(RadialMenu::new(self.edit.next_radial_id(), "Radial 1".into()));
        }
        self.radial_sel = self.radial_sel.min(self.edit.radials.len() - 1);
        let ring_len = |app: &Self| app.edit.radials[app.radial_sel].ring().count();
        let before = ring_len(self);
        self.slice_sel = self.slice_sel.min(before - 1);

        ui.columns(2, |cols| {
            cols[0].with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                self.radial_menu_controls(ui);
                ui.separator();
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    let (r, s) = (self.radial_sel, self.slice_sel);
                    let inner = self.edit.radials[r].slices.len();
                    ui.heading(if s < inner {
                        format!("Selected slice: {:02}", s + 1)
                    } else {
                        format!("Selected outer slice: {:02}", s - inner + 1)
                    });
                    // The slice is borrowed mutably out of the list, so the submenu picker gets a copy.
                    let menus = self.edit.radials.clone();
                    if let Some(map) = self.edit.radials[r].ring_mut(s) {
                        action_editor(ui, "Does", map, Slot::Slice(r, s), &mut self.capture, &menus);
                    }
                    ui.separator();
                    self.outer_ring_controls(ui);
                });
            });

            let ui = &mut cols[1];
            let menu = &self.edit.radials[self.radial_sel];
            ui.vertical_centered(|ui| ui.heading(&menu.name));
            let size = ui.available_width().min(ui.available_height() - 30.0).min(420.0);
            let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), size), egui::Sense::click());
            let center = rect.center();
            let ring: Vec<ButtonMap> = menu.ring().copied().collect();
            let (inner, count) = (menu.slices.len(), ring.len());
            let scale = size / 2.0 / (radial::extent(count > inner) + 6.0);
            let slice_at = |p| radial::slice_at_pos(center, scale, p, inner, count);
            let hovered = response.hover_pos().and_then(slice_at);
            if response.clicked() {
                if let Some(i) = response.interact_pointer_pos().and_then(slice_at) {
                    self.slice_sel = i;
                    self.capture = None;
                }
            }
            let on: Vec<bool> = ring.iter().map(|m| m.mode == Mode::Toggle && status.latched.contains(&m.action)).collect();
            let labels: Vec<String> = ring.iter().zip(&on).map(|(m, &on)| radial::slice_label(m, on)).collect();
            let (selected, color) = (self.slice_sel, menu.color);
            let fill_of = |i: usize| {
                if i == selected {
                    radial::fill(true, on[i], color)
                } else if hovered == Some(i) {
                    radial::fill_active(color)
                } else {
                    radial::fill(false, on[i], color)
                }
            };
            radial::draw_ring(&ui.painter_at(rect), center, scale, &labels, inner, fill_of);

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                ui.horizontal(|ui| {
                    ui.color_edit_button_srgb(&mut self.edit.radials[self.radial_sel].color);
                    ui.label("Color");
                });
            });
        });

        // Slice counts changed: indexes past the inner ring now point elsewhere.
        let after = ring_len(self);
        if after != before {
            self.slice_sel = self.slice_sel.min(after - 1);
            self.capture = None;
        }
    }

    fn outer_ring_controls(&mut self, ui: &mut egui::Ui) {
        let menu = &mut self.edit.radials[self.radial_sel];
        ui.checkbox(&mut menu.outer_enabled, "Enable Outer Radial Menu");
        if !menu.outer_enabled {
            return;
        }
        if menu.outer.is_empty() {
            menu.outer = vec![ButtonMap::new(Action::None, Mode::Hold); 8];
        }
        let mut count = menu.outer.len();
        ui.add(egui::Slider::new(&mut count, config::RADIAL_MIN..=config::OUTER_RADIAL_MAX).text("Outer slices"));
        menu.outer.resize(count, ButtonMap::new(Action::None, Mode::Hold));
        ui.weak("Point past the inner ring to pick from the outer one.");
    }

    /// Which radial menu is being edited, plus its name and slice count.
    /// Drawn bottom-up, so the menu picker ends up at the very bottom.
    fn radial_menu_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Radial profile");
            let current = self.edit.radials[self.radial_sel].name.clone();
            egui::ComboBox::from_id_salt("radial_menu").selected_text(current).show_ui(ui, |ui| {
                for (i, r) in self.edit.radials.iter().enumerate() {
                    if ui.selectable_label(i == self.radial_sel, &r.name).clicked() {
                        self.radial_sel = i;
                        self.slice_sel = 0;
                        self.capture = None;
                    }
                }
            });
            if ui.button("New").clicked() {
                let id = self.edit.next_radial_id();
                let name = format!("Radial {}", self.edit.radials.len() + 1);
                self.edit.radials.push(RadialMenu::with_random_color(id, name));
                self.radial_sel = self.edit.radials.len() - 1;
                self.slice_sel = 0;
                self.capture = None;
            }
            if self.edit.radials.len() > 1 && ui.button("Delete").clicked() {
                self.edit.radials.remove(self.radial_sel);
                self.radial_sel = 0;
                self.slice_sel = 0;
                self.capture = None;
            }
        });

        let menu = &mut self.edit.radials[self.radial_sel];
        let mut count = menu.slices.len();
        ui.add(egui::Slider::new(&mut count, config::RADIAL_MIN..=config::RADIAL_MAX).text("Slices"));
        menu.slices.resize(count, ButtonMap::new(Action::None, Mode::Hold));
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut menu.name);
        });
    }
}

/// Shown in the Main tab; set by `version` in Cargo.toml.
const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"));

fn status_section(ui: &mut egui::Ui, status: &Status, curve: f32) {
    ui.horizontal(|ui| {
        let (color, text) = if status.connected {
            (egui::Color32::from_rgb(80, 200, 120), "Tablet connected")
        } else {
            (egui::Color32::from_rgb(220, 90, 90), "Tablet not found")
        };
        ui.colored_label(color, "●");
        ui.label(text);
    });
    if let Some(err) = &status.error {
        ui.colored_label(egui::Color32::from_rgb(220, 160, 60), err);
    }

    let s = &status.sample;
    ui.horizontal(|ui| {
        let lamp = |ui: &mut egui::Ui, on: bool, name: &str| {
            let text = egui::RichText::new(name);
            ui.label(if on { text.strong().color(egui::Color32::from_rgb(120, 170, 255)) } else { text.weak() });
        };
        lamp(ui, s.in_range, "In range");
        lamp(ui, s.tip, "Tip");
        lamp(ui, s.near_button, "Near");
        lamp(ui, s.far_button, "Far");
    });
    let raw = if s.tip { s.pressure } else { 0 };
    ui.add(egui::ProgressBar::new(raw as f32 / tablet::PRESSURE_MAX as f32).text(format!("Pressure {raw}")));
    let shaped = tablet::shaped_pressure(raw, curve);
    ui.add(egui::ProgressBar::new(shaped).text(format!("Output {:.0}%", shaped * 100.0)));
}

/// The action dropdown's choices; Keys keeps whatever combo was there before.
#[derive(PartialEq, Clone, Copy)]
enum Kind {
    None,
    Left,
    Right,
    Middle,
    Keys,
    Radial,
    ReleaseAll,
}

fn kind_of(action: &Action) -> Kind {
    match action {
        Action::None => Kind::None,
        Action::Mouse(MouseButton::Left) => Kind::Left,
        Action::Mouse(MouseButton::Right) => Kind::Right,
        Action::Mouse(MouseButton::Middle) => Kind::Middle,
        Action::Keys(_) => Kind::Keys,
        Action::Radial(_) => Kind::Radial,
        Action::ReleaseAll => Kind::ReleaseAll,
    }
}

const KIND_LABELS: [(Kind, &str); 7] = [
    (Kind::None, "Nothing"),
    (Kind::Left, "Left click"),
    (Kind::Right, "Right click"),
    (Kind::Middle, "Middle click"),
    (Kind::Keys, "Keys"),
    (Kind::Radial, "Radial menu"),
    (Kind::ReleaseAll, "Reset all held"),
];

/// Editor for a pen button or a radial slice (slices have "Tap" instead of "Hold"
/// and no tip requirement; a slice set to a radial menu opens it as a submenu).
fn action_editor(
    ui: &mut egui::Ui,
    title: &str,
    map: &mut ButtonMap,
    slot: Slot,
    capture: &mut Option<Capture>,
    radials: &[RadialMenu],
) {
    let is_slice = matches!(slot, Slot::Slice(..));
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.strong(title);

        let current = kind_of(&map.action);
        let mut kind = current;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt((slot, "kind"))
                .selected_text(KIND_LABELS.iter().find(|(k, _)| *k == kind).unwrap().1)
                .show_ui(ui, |ui| {
                    for (k, label) in KIND_LABELS {
                        ui.selectable_value(&mut kind, k, label);
                    }
                });
            if let Action::Radial(id) = &mut map.action {
                let name = radials.iter().find(|r| r.id == *id).map_or("(missing)", |r| r.name.as_str());
                egui::ComboBox::from_id_salt((slot, "radial")).selected_text(name).show_ui(ui, |ui| {
                    for r in radials {
                        ui.selectable_value(id, r.id, &r.name);
                    }
                });
            } else if map.action != Action::ReleaseAll {
                ui.radio_value(&mut map.mode, Mode::Hold, if is_slice { "Tap" } else { "Hold" });
                ui.radio_value(&mut map.mode, Mode::Toggle, if is_slice { "Toggle (stays held)" } else { "Toggle" });
            }
        });
        if kind != current {
            map.action = match kind {
                Kind::None => Action::None,
                Kind::Left => Action::Mouse(MouseButton::Left),
                Kind::Right => Action::Mouse(MouseButton::Right),
                Kind::Middle => Action::Mouse(MouseButton::Middle),
                Kind::Keys => Action::Keys(KeyCombo::default()),
                Kind::Radial => Action::Radial(radials.first().map_or(0, |r| r.id)),
                Kind::ReleaseAll => Action::ReleaseAll,
            };
        }
        if !is_slice {
            ui.checkbox(&mut map.require_tip, "Only while tip is down");
        }

        if let Action::Keys(combo) = &mut map.action {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut combo.ctrl, "Ctrl");
                ui.checkbox(&mut combo.shift, "Shift");
                ui.checkbox(&mut combo.alt, "Alt");
                ui.checkbox(&mut combo.win, "Win");
                let capturing = capture.as_ref().is_some_and(|c| c.slot == slot);
                let text = if capturing {
                    "Press a key (Esc clears)".to_string()
                } else {
                    combo.key.map(inject::key_name).unwrap_or_else(|| "Select Key".to_string())
                };
                let button = ui.selectable_label(capturing, text);
                if button.clicked() {
                    // Don't let Space/Enter re-trigger this button while capturing.
                    button.surrender_focus();
                    *capture = if capturing { None } else { Some(Capture::new(slot)) };
                }
            });
        }
    });
}

fn main() -> eframe::Result {
    let Some(instance) = single::claim() else { return Ok(()) };
    let initial = config::load();
    let config = Arc::new(Mutex::new(initial.clone()));
    let status = Arc::new(Mutex::new(Status::default()));
    let quit = Arc::new(AtomicBool::new(false));

    let worker = {
        let (config, status, quit) = (config.clone(), status.clone(), quit.clone());
        std::thread::spawn(move || worker::run(config, status, quit))
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OpenPen")
            .with_icon(Arc::new(egui::IconData { rgba: icon::rgba(64), width: 64, height: 64 }))
            .with_inner_size([720.0, 660.0])
            .with_min_inner_size([520.0, 420.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "OpenPen",
        options,
        Box::new(move |cc| {
            let tray = tray::create(&cc.egui_ctx);
            if let Some(tray) = &tray {
                instance.listen(tray.flags.clone(), cc.egui_ctx.clone());
            }
            status.lock().unwrap().gui = Some(cc.egui_ctx.clone());
            Ok(Box::new(App {
                config,
                status,
                edit: initial,
                tab: Tab::Main,
                selected: None,
                radial_sel: 0,
                slice_sel: 0,
                capture: None,
                running_apps: Vec::new(),
                apps_refreshed: None,
                tray,
                hidden: false,
                restoring: false,
                overlay_shown: false,
            }))
        }),
    );

    quit.store(true, Ordering::Relaxed);
    let _ = worker.join();
    result
}
