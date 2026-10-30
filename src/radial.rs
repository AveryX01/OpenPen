//! Radial menu: geometry (shared by the worker, which does the picking), the ring
//! drawing (shared by the overlay and the editor preview), and the overlay window.
//!
//! Sizes are in 96-DPI units: egui points on the GUI side, scaled by the system DPI
//! on the worker side where the cursor is in physical pixels.

use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use windows::core::w;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::config::{Action, ButtonMap, Mode, MouseButton};
use crate::inject;

pub const INNER: f32 = 34.0;
pub const OUTER: f32 = 120.0;
/// The optional outer ring spans OUTER_IN..OUTER_MAX.
const OUTER_IN: f32 = OUTER + 8.0;
pub const OUTER_MAX: f32 = 195.0;
/// Overlay window half-size, a little bigger than the outer ring.
const HALF: f32 = OUTER_MAX + 10.0;

/// What the overlay needs to draw one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct RadialView {
    /// Physical-pixel screen position of the menu center.
    pub center: (i32, i32),
    /// Inner slices first, then the outer ring's; `inner` is where the outer ones start.
    pub labels: Vec<String>,
    pub inner: usize,
    pub latched: Vec<bool>,
    pub hovered: Option<usize>,
    pub color: [u8; 3],
}

/// Slice in the direction of an offset from the center. Slice 0 is at the top, going clockwise.
pub fn slice_at(dx: f32, dy: f32, count: usize) -> usize {
    let step = TAU / count as f32;
    let angle = dx.atan2(-dy).rem_euclid(TAU);
    (((angle + step / 2.0) / step) as usize) % count
}

/// Slice under `pos` for rings drawn at `center` with `scale`, if on a ring.
/// `inner` slices are on the inner ring, the rest of `count` on the outer.
pub fn slice_at_pos(center: egui::Pos2, scale: f32, pos: egui::Pos2, inner: usize, count: usize) -> Option<usize> {
    let d = (pos - center) / scale;
    let r = d.length();
    if (INNER..=OUTER).contains(&r) {
        Some(slice_at(d.x, d.y, inner))
    } else if count > inner && (OUTER_IN..=OUTER_MAX).contains(&r) {
        Some(inner + slice_at(d.x, d.y, count - inner))
    } else {
        None
    }
}

/// Radius of the outermost ring drawn.
pub fn extent(has_outer: bool) -> f32 {
    if has_outer { OUTER_MAX } else { OUTER }
}

pub fn action_label(action: &Action) -> String {
    match action {
        Action::None => "—".into(),
        Action::Mouse(MouseButton::Left) => "Left click".into(),
        Action::Mouse(MouseButton::Right) => "Right click".into(),
        Action::Mouse(MouseButton::Middle) => "Middle click".into(),
        Action::Radial(_) => "Radial".into(),
        Action::ReleaseAll => "Reset held".into(),
        Action::Keys(k) => {
            let mut parts: Vec<String> = Vec::new();
            for (on, name) in [(k.ctrl, "Ctrl"), (k.shift, "Shift"), (k.alt, "Alt"), (k.win, "Win")] {
                if on {
                    parts.push(name.into());
                }
            }
            parts.extend(k.key.map(inject::key_name));
            if parts.is_empty() {
                "—".into()
            } else {
                parts.join("+")
            }
        }
    }
}

pub const FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(24, 24, 29, 215);
/// A toggle slice that is currently on.
pub const FILL_ON: egui::Color32 = egui::Color32::from_rgba_premultiplied(105, 55, 165, 230);
pub const FILL_ON_HOVER: egui::Color32 = egui::Color32::from_rgba_premultiplied(145, 90, 210, 240);

/// The menu's color at full strength: the hovered slice.
fn fill_hover(color: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], 235)
}

/// A dimmer version of the menu's color, for the editor's mouse-over.
pub fn fill_active(color: [u8; 3]) -> egui::Color32 {
    let [r, g, b] = color.map(|c| (c as f32 * 0.6) as u8);
    egui::Color32::from_rgba_unmultiplied(r, g, b, 225)
}

pub fn fill(hovered: bool, on: bool, color: [u8; 3]) -> egui::Color32 {
    match (hovered, on) {
        (true, true) => FILL_ON_HOVER,
        (true, false) => fill_hover(color),
        (false, true) => FILL_ON,
        (false, false) => FILL,
    }
}

/// Marks "Unhold all" (unlock) and "Single Burst" (hourglass). These have to be glyphs
/// egui's bundled fonts have.
const ICON_UNHOLD: &str = "\u{1F513}";
const ICON_BURST: &str = "\u{23F3}";

/// Slice text; toggles say whether they're on, since that's otherwise invisible.
/// Modifier icons go on a second line, since labels can already be wide.
pub fn slice_label(map: &ButtonMap, on: bool) -> String {
    let label = action_label(&map.action);
    let mut text = match map.mode {
        Mode::Toggle => format!("{label} ({})", if on { "On" } else { "Off" }),
        Mode::Hold => label,
    };
    if let Action::Keys(k) = map.action {
        let burst = k.burst && map.mode == Mode::Toggle;
        if k.unhold || burst {
            text.push('\n');
        }
        if k.unhold {
            text.push_str(ICON_UNHOLD);
        }
        if burst {
            text.push_str(ICON_BURST);
        }
    }
    text
}

/// Gap between slices, in screen points (constant width, not a constant angle).
const GAP: f32 = 4.0;
/// Width of the anti-aliasing fringe, in points.
const FEATHER: f32 = 1.0;

/// Draws the inner ring of slices, plus the outer ring if `labels` has more than `inner`.
/// `fill_of(i)` picks each slice's color.
pub fn draw_ring(
    painter: &egui::Painter,
    center: egui::Pos2,
    scale: f32,
    labels: &[String],
    inner: usize,
    fill_of: impl Fn(usize) -> egui::Color32,
) {
    draw_band(painter, center, (INNER * scale, OUTER * scale), scale, &labels[..inner], &fill_of);
    if labels.len() > inner {
        let radii = (OUTER_IN * scale, OUTER_MAX * scale);
        draw_band(painter, center, radii, scale, &labels[inner..], |i| fill_of(inner + i));
    }
    let stroke = egui::Stroke::new(1.5, egui::Color32::from_white_alpha(90));
    painter.circle_stroke(center, (INNER - 6.0) * scale, stroke);
}

/// One ring of slices between two radii (in screen points).
fn draw_band(
    painter: &egui::Painter,
    center: egui::Pos2,
    (r_in, r_out): (f32, f32),
    scale: f32,
    labels: &[String],
    fill_of: impl Fn(usize) -> egui::Color32,
) {
    const SEGMENTS: usize = 32;
    let at = |r: f32, a: f32| center + egui::vec2(r * a.sin(), -r * a.cos());
    let n = labels.len();
    let step = TAU / n as f32;
    for i in 0..n {
        let mid = i as f32 * step;
        // Pull each side in by the same distance at every radius, so gaps stay parallel.
        let arc = |r: f32| -> Vec<egui::Pos2> {
            let d = (GAP / 2.0 / r).min(1.0).asin();
            let (a0, a1) = (mid - step / 2.0 + d, mid + step / 2.0 - d);
            (0..=SEGMENTS).map(|k| at(r, a0 + (a1 - a0) * k as f32 / SEGMENTS as f32)).collect()
        };
        painter.add(egui::Shape::mesh(wedge_mesh(&arc(r_in), &arc(r_out), fill_of(i))));
        // Line by line, since painter.text left-aligns the lines of multi-line text.
        let font = egui::FontId::proportional(13.0 * scale.max(0.8));
        let line_height = font.size * 1.25;
        let lines = labels[i].lines().count();
        for (l, line) in labels[i].lines().enumerate() {
            let y = (l as f32 - (lines - 1) as f32 / 2.0) * line_height;
            let pos = at((r_in + r_out) / 2.0, mid) + egui::vec2(0.0, y);
            painter.text(pos, egui::Align2::CENTER_CENTER, line, font.clone(), egui::Color32::WHITE);
        }
    }
}

/// Filled annular wedge between two matching arcs, with a soft edge all around.
/// egui only anti-aliases convex shapes, so the fringe is built by hand: the outline
/// is inset by half the feather for the solid fill, and a ring of transparent
/// vertices sits half a feather outside it.
fn wedge_mesh(inner: &[egui::Pos2], outer: &[egui::Pos2], color: egui::Color32) -> egui::Mesh {
    let outline: Vec<egui::Pos2> = outer.iter().chain(inner.iter().rev()).copied().collect();
    let m = outline.len();
    let area: f32 = (0..m)
        .map(|k| {
            let (a, b) = (outline[k], outline[(k + 1) % m]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    let sign = if area > 0.0 { 1.0 } else { -1.0 };
    let edge_normal = |e: egui::Vec2| egui::vec2(e.y, -e.x).normalized() * sign;

    let mut mesh = egui::Mesh::default();
    for k in 0..m {
        let p = outline[k];
        let n = (edge_normal(p - outline[(k + m - 1) % m]) + edge_normal(outline[(k + 1) % m] - p)).normalized();
        mesh.colored_vertex(p - n * FEATHER / 2.0, color);
        mesh.colored_vertex(p + n * FEATHER / 2.0, egui::Color32::TRANSPARENT);
    }
    let solid = |k: usize| (2 * k) as u32;
    let fringe = |k: usize| (2 * k + 1) as u32;
    // Fill: strip between outer[j] (outline j) and inner[j] (outline m-1-j).
    for j in 0..outer.len() - 1 {
        let (o0, o1, i0, i1) = (solid(j), solid(j + 1), solid(m - 1 - j), solid(m - 2 - j));
        mesh.add_triangle(o0, o1, i1);
        mesh.add_triangle(o0, i1, i0);
    }
    for k in 0..m {
        let k2 = (k + 1) % m;
        mesh.add_triangle(solid(k), fringe(k), solid(k2));
        mesh.add_triangle(fringe(k), fringe(k2), solid(k2));
    }
    mesh
}

/// The overlay is one long-lived click-through window. Creating a fresh window per
/// open made it visibly slide in from its default position; instead it's parked
/// off-screen while closed and moved to the cursor when opened.
///
/// Call every frame; returns whether the window is on screen.
pub fn show_overlay(ctx: &egui::Context, view: Option<&RadialView>) -> bool {
    let ppp = ctx.pixels_per_point();
    let pos = match view {
        Some(v) => egui::pos2(v.center.0 as f32 / ppp - HALF, v.center.1 as f32 / ppp - HALF),
        None => egui::pos2(-30000.0, -30000.0),
    };
    let builder = egui::ViewportBuilder::default()
        .with_title("OpenPen radial")
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_mouse_passthrough(true)
        .with_active(false)
        .with_taskbar(false)
        .with_resizable(false)
        .with_position(pos)
        .with_inner_size([HALF * 2.0, HALF * 2.0]);
    let view = view.cloned();
    let shown = view.is_some();
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("openpen_radial"), builder, move |ui, _| {
        let Some(view) = &view else { return };
        let fill_of = |i: usize| fill(view.hovered == Some(i), view.latched[i], view.color);
        draw_ring(ui.painter(), ui.max_rect().center(), 1.0, &view.labels, view.inner, fill_of);
    });
    make_unfocusable();
    shown
}

/// egui's `with_active(false)` only covers the first show; the overlay could still take
/// focus later and swallow the keys meant for the app underneath. Windows' no-activate
/// style rules that out for good (and tool-window keeps it out of Alt+Tab).
fn make_unfocusable() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Relaxed) {
        return;
    }
    unsafe {
        let Ok(hwnd) = FindWindowW(None, w!("OpenPen radial")) else { return };
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize);
    }
    DONE.store(true, Ordering::Relaxed);
}
