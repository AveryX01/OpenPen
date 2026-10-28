//! User settings, saved as JSON in a `SaveData` folder next to the exe:
//! `config.json` for profiles and buttons, `radials.json` for radial menus.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KeyCombo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    /// Windows virtual-key code of the main key, if any (modifiers alone are fine).
    pub key: Option<u16>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    Mouse(MouseButton),
    Keys(KeyCombo),
    /// Pen button: shows the radial menu with this id while held. Slice: enters it as a submenu.
    Radial(u32),
    /// Releases everything held: toggled slices and toggled pen buttons.
    ReleaseAll,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Pen button: active while held. Radial slice: a single press and release.
    Hold,
    /// First press activates, second press releases.
    Toggle,
}

/// What a pen button or a radial slice does.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct ButtonMap {
    pub action: Action,
    pub mode: Mode,
    /// Only counts as pressed while the pen tip is touching the tablet (pen buttons only).
    #[serde(default)]
    pub require_tip: bool,
}

impl ButtonMap {
    pub fn new(action: Action, mode: Mode) -> Self {
        ButtonMap { action, mode, require_tip: false }
    }
}

/// Everything a profile controls.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Bindings {
    /// Pressure curve exponent, 1.0 = linear; higher needs more force for the same output.
    #[serde(default = "default_curve")]
    pub pressure_curve: f32,
    pub near: ButtonMap,
    pub far: ButtonMap,
}

impl Default for Bindings {
    fn default() -> Self {
        // Same as the tablet's own defaults: near = middle, far = right.
        Bindings {
            pressure_curve: default_curve(),
            near: ButtonMap::new(Action::Mouse(MouseButton::Middle), Mode::Hold),
            far: ButtonMap::new(Action::Mouse(MouseButton::Right), Mode::Hold),
        }
    }
}

pub const RADIAL_MIN: usize = 2;
pub const RADIAL_MAX: usize = 8;
pub const OUTER_RADIAL_MAX: usize = 12;

/// A named radial menu; any pen button in any profile can open it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct RadialMenu {
    /// Stable id that `Action::Radial` refers to (survives renames and reordering).
    pub id: u32,
    pub name: String,
    pub slices: Vec<ButtonMap>,
    /// Slice highlight color (sRGB), so different menus are easy to tell apart.
    #[serde(default = "default_color")]
    pub color: [u8; 3],
    /// A second ring around the first, picked by pointing past the inner ring.
    #[serde(default)]
    pub outer_enabled: bool,
    /// Outer ring slices; kept while the ring is disabled so turning it back on restores them.
    #[serde(default)]
    pub outer: Vec<ButtonMap>,
}

impl RadialMenu {
    pub fn new(id: u32, name: String) -> Self {
        RadialMenu { id, name, slices: default_slices(), color: default_color(), outer_enabled: false, outer: Vec::new() }
    }

    /// Inner slices followed by the outer ones (if enabled). Indexes into this are how
    /// the worker and editor refer to slices; anything at `slices.len()` or above is outer.
    pub fn ring(&self) -> impl Iterator<Item = &ButtonMap> {
        self.slices.iter().chain(if self.outer_enabled { &self.outer[..] } else { &[] })
    }

    pub fn ring_mut(&mut self, i: usize) -> Option<&mut ButtonMap> {
        let n = self.slices.len();
        if i < n {
            self.slices.get_mut(i)
        } else if self.outer_enabled {
            self.outer.get_mut(i - n)
        } else {
            None
        }
    }

    /// A new menu with a random bright color.
    pub fn with_random_color(id: u32, name: String) -> Self {
        RadialMenu { color: random_color(), ..Self::new(id, name) }
    }
}

/// The original OpenPen blue.
fn default_color() -> [u8; 3] {
    [73, 115, 202]
}

/// A saturated color with a random hue, avoiding purple (that means "toggle on").
fn random_color() -> [u8; 3] {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let mut hue = (nanos % 360) as f32;
    if (250.0..310.0).contains(&hue) {
        hue += 60.0;
    }
    hsv(hue % 360.0, 0.6, 0.8)
}

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r, g, b].map(|u| ((u + m) * 255.0).round() as u8)
}

fn default_radials() -> Vec<RadialMenu> {
    vec![RadialMenu::new(1, "Modifiers".into())]
}

fn default_slices() -> Vec<ButtonMap> {
    let keys = |ctrl, shift, alt, key| Action::Keys(KeyCombo { ctrl, shift, alt, win: false, key });
    vec![
        ButtonMap::new(keys(true, false, false, None), Mode::Toggle),
        ButtonMap::new(keys(false, true, false, None), Mode::Toggle),
        ButtonMap::new(keys(false, false, true, None), Mode::Toggle),
        ButtonMap::new(keys(true, false, false, Some(0x5A)), Mode::Hold),
    ]
}

/// Bindings that take over while one of `apps` is the focused window.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Profile {
    pub name: String,
    /// Lowercase exe names, e.g. "blender.exe".
    pub apps: Vec<String>,
    pub bindings: Bindings,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Config {
    pub pen_mode: bool,
    /// Relative-mouse speed when pen mode is off; 1.0 = tablet width moves one screen width.
    #[serde(default = "default_speed")]
    pub mouse_speed: f32,
    /// "No profile": used whenever the focused app has no profile.
    #[serde(default)]
    pub default: Bindings,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// Stored in its own file, see `load`/`save`.
    #[serde(skip)]
    pub radials: Vec<RadialMenu>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            pen_mode: true,
            mouse_speed: default_speed(),
            default: Bindings::default(),
            profiles: Vec::new(),
            radials: default_radials(),
        }
    }
}

impl Config {
    /// Index of the profile claiming this exe, if any.
    pub fn profile_for(&self, exe: Option<&str>) -> Option<usize> {
        let exe = exe?;
        self.profiles.iter().position(|p| p.apps.iter().any(|a| a.eq_ignore_ascii_case(exe)))
    }

    /// `None` is "No profile".
    pub fn bindings(&self, profile: Option<usize>) -> &Bindings {
        profile.and_then(|i| self.profiles.get(i)).map_or(&self.default, |p| &p.bindings)
    }

    pub fn radial(&self, id: u32) -> Option<&RadialMenu> {
        self.radials.iter().find(|r| r.id == id)
    }

    pub fn next_radial_id(&self) -> u32 {
        self.radials.iter().map(|r| r.id).max().unwrap_or(0) + 1
    }
}

fn default_speed() -> f32 {
    1.0
}

fn default_curve() -> f32 {
    1.0
}

fn path(file: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("SaveData").join(file))
}

fn read<T: serde::de::DeserializeOwned>(file: &str) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path(file)?).ok()?).ok()
}

fn write<T: Serialize>(file: &str, value: &T) {
    let Some(p) = path(file) else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(p, json);
    }
}

pub fn load() -> Config {
    let mut config: Config = read("config.json").unwrap_or_default();
    config.radials = read("radials.json").unwrap_or_else(default_radials);
    config
}

pub fn save(config: &Config) {
    write("config.json", config);
    write("radials.json", &config.radials);
}
