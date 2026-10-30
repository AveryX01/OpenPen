//! Background thread: owns the tablet, applies the config, drives the outputs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow, SetForegroundWindow};

use crate::apps;
use crate::config::{Action, Bindings, ButtonMap, Config, Mode, MouseButton};
use crate::inject::{self, PenOutput};
use crate::radial::{self, RadialView};
use crate::tablet::{self, PenSample};

#[derive(Clone, Default)]
pub struct Status {
    pub connected: bool,
    pub sample: PenSample,
    pub error: Option<String>,
    /// Profile currently applied; `None` is "No profile".
    pub active_profile: Option<usize>,
    pub radial: Option<RadialView>,
    /// Set by the GUI so the worker can wake it the moment the radial menu changes.
    pub gui: Option<eframe::egui::Context>,
    /// Actions currently held by toggled radial slices.
    pub latched: Vec<Action>,
    /// Set by the GUI's "Release all" button; the worker clears it.
    pub release_held: bool,
}

fn action_down(action: &Action) {
    match action {
        Action::Mouse(b) => inject::mouse_button(*b, true),
        Action::Keys(k) => inject::key_combo(k, true),
        Action::None | Action::Radial(_) | Action::ReleaseAll => {}
    }
}

fn action_up(action: &Action) {
    match action {
        Action::Mouse(b) => inject::mouse_button(*b, false),
        Action::Keys(k) => inject::key_combo(k, false),
        Action::None | Action::Radial(_) | Action::ReleaseAll => {}
    }
}

/// How long the pen must rest on a submenu slice to enter it.
const SUBMENU_DWELL: Duration = Duration::from_millis(300);

/// One pen button's mapping state.
#[derive(Default)]
struct ButtonState {
    was_pressed: bool,
    /// The action currently held down, remembered so we release exactly what we pressed
    /// even if the mapping or profile changes meanwhile.
    active: Option<Action>,
}

impl ButtonState {
    /// Applies a press/release. Returns the action for `Outputs` to run if this press is
    /// one it handles itself (opening a radial menu, releasing everything held).
    fn update(&mut self, pressed: bool, map: &ButtonMap) -> Option<Action> {
        let edge_down = pressed && !self.was_pressed;
        let edge_up = !pressed && self.was_pressed;
        self.was_pressed = pressed;
        if matches!(map.action, Action::Radial(_) | Action::ReleaseAll) {
            self.release();
            return edge_down.then_some(map.action);
        }
        match map.mode {
            Mode::Hold => {
                if edge_down {
                    self.activate(map.action);
                } else if edge_up {
                    self.release();
                }
            }
            Mode::Toggle => {
                if edge_down {
                    if self.active.is_some() {
                        self.release();
                    } else {
                        self.activate(map.action);
                    }
                }
            }
        }
        None
    }

    fn activate(&mut self, action: Action) {
        self.release();
        if action != Action::None {
            action_down(&action);
            self.active = Some(action);
        }
    }

    fn release(&mut self) {
        if let Some(action) = self.active.take() {
            action_up(&action);
        }
    }

    fn holds_mouse(&self) -> bool {
        matches!(self.active, Some(Action::Mouse(_)))
    }

    /// Whether this press is about to activate a combo marked "Unhold all".
    fn will_unhold(&self, pressed: bool, map: &ButtonMap) -> bool {
        pressed && !self.was_pressed && unholds(&map.action) && (map.mode == Mode::Hold || self.active.is_none())
    }
}

fn unholds(action: &Action) -> bool {
    matches!(action, Action::Keys(k) if k.unhold)
}

#[derive(Clone, Copy, PartialEq)]
enum PenButton {
    Near,
    Far,
}

/// An open radial menu.
struct Radial {
    /// The button holding it open; releasing it closes the menu.
    opened_by: PenButton,
    /// Id of the menu the button opened, which stays the same inside submenus.
    root: u32,
    /// The window that was focused when the menu opened; picked keys go to it.
    target: HWND,
    center: POINT,
    /// Inner ring slices, then the outer ring's (from `inner` on).
    slices: Vec<ButtonMap>,
    inner: usize,
    /// When `hovered` last changed, for entering submenus by resting on them.
    hovered_since: Instant,
    /// Name of the menu each slice opens, for slices that open another radial menu.
    submenus: Vec<Option<String>>,
    color: [u8; 3],
    hovered: Option<usize>,
}

#[derive(Default)]
struct Outputs {
    pen: Option<PenOutput>,
    near: ButtonState,
    far: ButtonState,
    radial: Option<Radial>,
    /// Toggle slices picked from a radial menu, held until picked again.
    latched: Vec<Action>,
    /// Single Burst toggles still held, with the root menu whose next opening releases them.
    bursts: Vec<(Action, u32)>,
    /// Ignore the tip until it lifts (it was used to pick in the radial menu).
    swallow_tip: bool,
    /// Physical pixels per 96-DPI unit.
    dpi_scale: f32,
    /// Relative mode: last tablet position and sub-pixel leftovers.
    rel_last: Option<(u16, u16)>,
    rel_frac: (f32, f32),
    /// Relative mode: tip acts as the left mouse button.
    left_down: bool,
}

fn submenu_names(cfg: &Config, slices: &[ButtonMap]) -> Vec<Option<String>> {
    slices
        .iter()
        .map(|m| match m.action {
            Action::Radial(id) => Some(cfg.radial(id).map_or("(missing)".into(), |r| r.name.clone())),
            _ => None,
        })
        .collect()
}

fn cursor() -> POINT {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    pt
}

impl Outputs {
    fn release_all(&mut self) {
        self.near.release();
        self.far.release();
        self.near.was_pressed = false;
        self.far.was_pressed = false;
        self.release_latched();
        self.radial = None;
        self.set_left(false);
        self.rel_last = None;
        self.leave_pen();
    }

    fn leave_pen(&mut self) {
        if let Some(pen) = &mut self.pen {
            pen.leave();
        }
    }

    fn set_left(&mut self, down: bool) {
        if self.left_down != down {
            inject::mouse_button(MouseButton::Left, down);
            self.left_down = down;
        }
    }

    fn mouse_held(&self) -> bool {
        self.near.holds_mouse()
            || self.far.holds_mouse()
            || self.latched.iter().any(|a| matches!(a, Action::Mouse(_)))
    }

    fn handle(&mut self, cfg: &Config, b: &Bindings, raw: &PenSample) {
        let mut s = *raw;
        if !s.tip {
            self.swallow_tip = false;
        }
        if self.swallow_tip {
            s.tip = false;
        }

        let near = s.near_button && (!b.near.require_tip || s.tip);
        let far = s.far_button && (!b.far.require_tip || s.tip);
        for (by, pressed) in [(PenButton::Near, near), (PenButton::Far, far)] {
            let (state, other, map) = match by {
                PenButton::Near => (&mut self.near, &mut self.far, &b.near),
                PenButton::Far => (&mut self.far, &mut self.near, &b.far),
            };
            if state.will_unhold(pressed, map) {
                other.release();
                self.release_latched();
            }
            let state = if by == PenButton::Near { &mut self.near } else { &mut self.far };
            match state.update(pressed, map) {
                Some(Action::Radial(id)) => self.open_radial(cfg, id, by, &mut s),
                Some(Action::ReleaseAll) => self.reset_held(),
                _ => {}
            }
        }
        self.update_radial(cfg, &mut s);
        // Releasing the button picks the slice it's pointing at (center hole = cancel).
        let holding = |by| if by == PenButton::Near { near } else { far };
        if self.radial.as_ref().is_some_and(|r| !holding(r.opened_by)) {
            let menu = self.radial.take().unwrap();
            if let Some(i) = menu.hovered.filter(|&i| menu.submenus[i].is_none()) {
                self.pick_for(menu.target, menu.root, menu.slices[i]);
            }
        }

        let mouse_held = self.mouse_held() || self.radial.is_some();
        if cfg.pen_mode {
            self.absolute(&s, mouse_held, b.pressure_curve);
        } else {
            self.relative(cfg, &s, mouse_held);
        }
    }

    fn open_radial(&mut self, cfg: &Config, id: u32, by: PenButton, s: &mut PenSample) {
        let Some(menu) = cfg.radial(id) else { return };
        self.release_bursts(id);
        if s.tip {
            // Opened while touching: the tip must lift before it can pick.
            self.swallow_tip = true;
            s.tip = false;
        }
        let slices: Vec<ButtonMap> = menu.ring().copied().collect();
        self.radial = Some(Radial {
            opened_by: by,
            root: id,
            target: unsafe { GetForegroundWindow() },
            center: cursor(),
            submenus: submenu_names(cfg, &slices),
            slices,
            inner: menu.slices.len(),
            hovered_since: Instant::now(),
            color: menu.color,
            hovered: None,
        });
    }

    /// Replaces the open menu with radial menu `id`, centered on the cursor. Keeps the
    /// button and target window, so releasing still picks from the new menu.
    fn enter_submenu(&mut self, cfg: &Config, id: u32) {
        let (Some(menu), Some(open)) = (cfg.radial(id), &mut self.radial) else { return };
        open.center = cursor();
        open.slices = menu.ring().copied().collect();
        open.inner = menu.slices.len();
        open.submenus = submenu_names(cfg, &open.slices);
        open.hovered_since = Instant::now();
        open.color = menu.color;
        open.hovered = None;
    }

    /// Tracks the hovered slice; a tip press picks it and keeps the menu open (so several
    /// slices can be picked). Releasing the button picks too, see `handle`.
    /// With an outer ring, pointing past the inner ring picks from the outer one. Resting
    /// on (or tapping) a slice that opens another menu enters it.
    fn update_radial(&mut self, cfg: &Config, s: &mut PenSample) {
        let Some(menu) = &mut self.radial else { return };
        let pos = cursor();
        let dx = (pos.x - menu.center.x) as f32 / self.dpi_scale;
        let dy = (pos.y - menu.center.y) as f32 / self.dpi_scale;
        // Picks by direction only: anywhere past the center hole counts, however far out.
        let dist = (dx * dx + dy * dy).sqrt();
        let (inner, total) = (menu.inner, menu.slices.len());
        let hovered = if dist < radial::INNER {
            None
        } else if total > inner && dist > radial::OUTER {
            Some(inner + radial::slice_at(dx, dy, total - inner))
        } else {
            Some(radial::slice_at(dx, dy, inner))
        };
        if hovered != menu.hovered {
            menu.hovered = hovered;
            menu.hovered_since = Instant::now();
        }
        let submenu = menu.hovered.and_then(|i| match menu.slices[i].action {
            Action::Radial(id) => Some(id),
            _ => None,
        });

        if let Some(id) = submenu {
            // Pointing past the slice's outer edge enters at once, no dwell.
            let edge = if menu.hovered.is_some_and(|i| i >= inner) { radial::OUTER_MAX } else { radial::OUTER };
            if s.tip || dist >= edge || menu.hovered_since.elapsed() >= SUBMENU_DWELL {
                if s.tip {
                    self.swallow_tip = true;
                    s.tip = false;
                }
                self.enter_submenu(cfg, id);
            }
        } else if s.tip {
            // Every touch while open is for the menu, never the app underneath.
            let picked = menu.hovered.map(|i| menu.slices[i]);
            let (target, root) = (menu.target, menu.root);
            self.swallow_tip = true;
            s.tip = false;
            if let Some(slice) = picked {
                self.pick_for(target, root, slice);
            }
        }
    }

    /// Picks a slice, first handing focus back to `target` if anything stole it.
    fn pick_for(&mut self, target: HWND, root: u32, slice: ButtonMap) {
        unsafe {
            if !target.is_invalid() && GetForegroundWindow() != target {
                let _ = SetForegroundWindow(target);
            }
        }
        self.pick(root, slice);
    }

    fn pick(&mut self, root: u32, slice: ButtonMap) {
        let action = slice.action;
        if action == Action::ReleaseAll {
            self.reset_held();
            return;
        }
        // Turning a toggle back off leaves everything else alone.
        if unholds(&action) && !self.latched.contains(&action) {
            self.reset_held();
        }
        match slice.mode {
            Mode::Toggle => {
                if let Some(i) = self.latched.iter().position(|a| *a == action) {
                    action_up(&self.latched.remove(i));
                } else if !matches!(action, Action::None | Action::Radial(_)) {
                    action_down(&action);
                    self.latched.push(action);
                    self.bursts.retain(|(a, _)| *a != action);
                    if matches!(action, Action::Keys(k) if k.burst) {
                        self.bursts.push((action, root));
                    }
                }
            }
            Mode::Hold => {
                action_down(&action);
                action_up(&action);
            }
        }
    }

    /// Everything that stays held without the pen holding it: toggled slices and toggled
    /// pen buttons.
    fn reset_held(&mut self) {
        self.release_latched();
        self.near.release();
        self.far.release();
    }

    /// Releases the Single Burst toggles that were picked under root menu `root`.
    fn release_bursts(&mut self, root: u32) {
        let (due, rest): (Vec<_>, Vec<_>) = self.bursts.drain(..).partition(|(_, r)| *r == root);
        self.bursts = rest;
        for (action, _) in due {
            if let Some(i) = self.latched.iter().position(|a| *a == action) {
                action_up(&self.latched.remove(i));
            }
        }
    }

    fn release_latched(&mut self) {
        self.bursts.clear();
        for action in self.latched.drain(..) {
            action_up(&action);
        }
    }

    /// Releases toggles whose slice was edited or removed, since nothing could
    /// release them from the menu any more.
    fn prune_latched(&mut self, cfg: &Config) {
        let exists = |a: &Action| {
            cfg.radials.iter().flat_map(|r| r.ring()).any(|s| s.mode == Mode::Toggle && s.action == *a)
        };
        self.latched.retain(|a| {
            let keep = exists(a);
            if !keep {
                action_up(a);
            }
            keep
        });
    }

    fn radial_view(&self) -> Option<RadialView> {
        let menu = self.radial.as_ref()?;
        let latched: Vec<bool> =
            menu.slices.iter().map(|m| m.mode == Mode::Toggle && self.latched.contains(&m.action)).collect();
        Some(RadialView {
            center: (menu.center.x, menu.center.y),
            inner: menu.inner,
            labels: menu
                .slices
                .iter()
                .zip(&latched)
                .zip(&menu.submenus)
                .map(|((m, &on), sub)| match sub {
                    Some(name) => format!("{name} >"),
                    None => radial::slice_label(m, on),
                })
                .collect(),
            latched,
            hovered: menu.hovered,
            color: menu.color,
        })
    }

    /// Pen mode: position maps straight onto the screen.
    ///
    /// The Windows pen is only used while the tip is down. Hovering moves the plain
    /// mouse cursor instead: Windows ignores mouse clicks while a pen is in range,
    /// which made mapped button clicks get dropped unless held for a while.
    /// Likewise while a mapped mouse button is held, so apps see a normal mouse drag
    /// (e.g. middle-drag to orbit in Blender), and while the radial menu is open.
    fn absolute(&mut self, s: &PenSample, mouse_held: bool, curve: f32) {
        if s.tip && !mouse_held {
            if let Some(pen) = &mut self.pen {
                pen.update(s, curve);
            }
        } else {
            self.leave_pen();
            if s.in_range {
                inject::mouse_move(s.x, s.y);
            }
        }
    }

    /// Pen mode off: behaves like the tablet's own mouse mode, moving the cursor by
    /// how far the pen moves, with the tip as left click.
    fn relative(&mut self, cfg: &Config, s: &PenSample, mouse_held: bool) {
        self.leave_pen();
        if !s.in_range {
            self.rel_last = None;
            self.set_left(false);
            return;
        }
        if let Some((lx, ly)) = self.rel_last {
            let (w, _) = inject::screen_size();
            let scale = w as f32 / tablet::TABLET_W as f32 * cfg.mouse_speed;
            let fx = (s.x as f32 - lx as f32) * scale + self.rel_frac.0;
            let fy = (s.y as f32 - ly as f32) * scale + self.rel_frac.1;
            let (dx, dy) = (fx.trunc(), fy.trunc());
            self.rel_frac = (fx - dx, fy - dy);
            inject::mouse_move_relative(dx as i32, dy as i32);
        }
        self.rel_last = Some((s.x, s.y));
        self.set_left(s.tip && !mouse_held);
    }
}

pub fn run(config: Arc<Mutex<Config>>, status: Arc<Mutex<Status>>, quit: Arc<AtomicBool>) {
    let set_status = |f: &dyn Fn(&mut Status)| f(&mut status.lock().unwrap());

    let mut api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            set_status(&|s| s.error = Some(format!("HID init failed: {e}")));
            return;
        }
    };
    let mut out = Outputs { dpi_scale: unsafe { GetDpiForSystem() } as f32 / 96.0, ..Default::default() };
    match PenOutput::new() {
        Ok(pen) => out.pen = Some(pen),
        Err(e) => set_status(&|s| s.error = Some(format!("pen injection unavailable: {e}"))),
    }

    while !quit.load(Ordering::Relaxed) {
        let Some(dev) = tablet::open(&mut api) else {
            set_status(&|s| s.connected = false);
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        set_status(&|s| s.connected = true);
        session(&dev, &config, &status, &quit, &mut out);
        out.release_all();
        set_status(&|s| {
            s.connected = false;
            s.sample = PenSample::default();
            s.radial = None;
        });
    }
}

/// Runs until the tablet disconnects or the app quits.
fn session(
    dev: &HidDevice,
    config: &Mutex<Config>,
    status: &Mutex<Status>,
    quit: &AtomicBool,
    out: &mut Outputs,
) {
    // The tablet only sends pen reports in pen mode; "Pen Mode" off is emulated
    // on our side (see `Outputs::relative`), so this is sent once per connection.
    if tablet::enable_pen_reports(dev).is_err() {
        return;
    }
    let mut last_pen_mode = None;
    let mut buf = [0u8; 64];
    let mut exe: Option<String> = None;
    let mut last_poll = Instant::now() - Duration::from_secs(1);
    loop {
        let cfg = config.lock().unwrap().clone();
        if quit.load(Ordering::Relaxed) {
            return;
        }
        if last_pen_mode != Some(cfg.pen_mode) {
            out.release_all();
            last_pen_mode = Some(cfg.pen_mode);
        }

        // Which app is focused decides the profile. Our own window is ignored so
        // editing settings doesn't flip back to "No profile".
        if last_poll.elapsed() >= Duration::from_millis(250) {
            last_poll = Instant::now();
            match apps::foreground_exe() {
                Some(e) if e == "openpen.exe" => {}
                other => exe = other,
            }
        }
        let profile = cfg.profile_for(exe.as_deref());

        let n = match dev.read_timeout(&mut buf, 100) {
            Ok(n) => n,
            Err(_) => return,
        };
        let sample = tablet::PenSample::parse(&buf[..n]);
        if let Some(sample) = &sample {
            out.handle(&cfg, cfg.bindings(profile), sample);
        }
        out.prune_latched(&cfg);

        let mut st = status.lock().unwrap();
        if std::mem::take(&mut st.release_held) {
            out.reset_held();
        }
        st.latched = out.latched.clone();
        st.active_profile = profile;
        let radial = out.radial_view();
        if radial != st.radial {
            if let Some(gui) = &st.gui {
                gui.request_repaint();
            }
            st.radial = radial;
        }
        if let Some(sample) = sample {
            st.sample = sample;
        }
    }
}
