//! A floating button you can hold to dictate, for touchscreens.
//!
//! Two windows rather than one. The button is small and has to receive
//! touches; the dismiss target sits near the bottom of the screen and must not
//! receive anything, since the finger dropping onto it is still captured by
//! the button. A single window would have to cover the whole screen and then
//! work out which parts of itself were solid, which is harder than owning two.
//!
//! Both carry the same window styles as the overlay: always on top, absent
//! from the taskbar, and -- the part everything depends on -- never taking
//! focus. If this button stole focus, the dictation would paste into us
//! instead of into whatever the user was actually writing in.

use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition};
use windows::Win32::Foundation::HWND;

pub const BUTTON: &str = "touch";
pub const TARGET: &str = "touchTarget";

/// Height of the dismiss target, and how far above the bottom edge it sits.
const TARGET_SIZE: f64 = 150.0;
const TARGET_MARGIN: f64 = 40.0;

/// Called once at startup. Applies the window styles and either shows or
/// hides the button according to the setting.
pub fn setup(app: &AppHandle, enabled: bool, size: f64, saved: Option<(f64, f64)>) {
    // Before the window styles below, not after. Asking Tauri to ignore the
    // cursor rewrites the same extended-style word we are about to set, so
    // doing it second wiped the no-activate flag straight back off -- the
    // target came up without it while the button kept it.
    if let Some(target) = app.get_webview_window(TARGET) {
        let _ = target.set_ignore_cursor_events(true);
        let _ = target.hide();
    }

    for label in [BUTTON, TARGET] {
        let Some(window) = app.get_webview_window(label) else {
            crate::logln!("[touch] window {label} missing from the app configuration");
            continue;
        };
        if let Ok(hwnd) = window.hwnd() {
            // Tauri bundles its own windows crate, so its handle is a
            // distinct type from ours over the same pointer.
            crate::overlay::make_non_activating(HWND(hwnd.0 as _));
        }
    }

    if let Some(button) = app.get_webview_window(BUTTON) {
        let _ = button.set_size(LogicalSize::new(size, size));
    }

    place(app, saved, size);

    if enabled {
        show(app);
    } else {
        hide(app);
    }
}

/// Puts the button where it was left, or somewhere sensible on first use.
///
/// Always clamped to the current screen. A remembered position is only valid
/// for the screen it was saved on: a tablet docked to a larger monitor and
/// then undocked would otherwise leave the button parked off the edge with no
/// way to reach it.
pub fn place(app: &AppHandle, saved: Option<(f64, f64)>, size: f64) {
    let Some(button) = app.get_webview_window(BUTTON) else {
        return;
    };
    let Some((screen_w, screen_h, scale)) = screen(&button) else {
        return;
    };

    // Default: low on the right, near where a thumb rests, and clear of the
    // taskbar.
    let (x, y) = saved.unwrap_or((screen_w - size - 28.0, screen_h - size - 140.0));

    let x = x.clamp(0.0, (screen_w - size).max(0.0));
    let y = y.clamp(0.0, (screen_h - size).max(0.0));
    let _ = button.set_position(LogicalPosition::new(x, y));

    // Centred along the bottom, where a thumb naturally drags to.
    if let Some(target) = app.get_webview_window(TARGET) {
        let _ = target.set_size(LogicalSize::new(TARGET_SIZE, TARGET_SIZE));
        let _ = target.set_position(LogicalPosition::new(
            (screen_w - TARGET_SIZE) / 2.0,
            screen_h - TARGET_SIZE - TARGET_MARGIN,
        ));
    }

    let _ = scale;
}

/// The work area in logical pixels, plus the scale factor.
///
/// Deliberately the work area rather than the whole screen, so the button is
/// never parked underneath the taskbar.
fn screen(window: &tauri::WebviewWindow) -> Option<(f64, f64, f64)> {
    let monitor = window.current_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    let size = monitor.size().to_logical::<f64>(scale);
    Some((size.width, size.height, scale))
}

pub fn show(app: &AppHandle) {
    if let Some(button) = app.get_webview_window(BUTTON) {
        let _ = button.show();
        // Re-asserted on every show. Tauri can rebuild the native window, and
        // a button that has quietly become focus-stealing breaks pasting in a
        // way that is hard to connect back to this.
        if let Ok(hwnd) = button.hwnd() {
            // Tauri bundles its own windows crate, so its handle is a
            // distinct type from ours over the same pointer.
            crate::overlay::make_non_activating(HWND(hwnd.0 as _));
        }
        let _ = button.set_always_on_top(true);
    }
}

pub fn hide(app: &AppHandle) {
    for label in [BUTTON, TARGET] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.hide();
        }
    }
}

/// Moves the button so its top-left lands at the given screen point.
pub fn move_to(app: &AppHandle, x: f64, y: f64) {
    if let Some(button) = app.get_webview_window(BUTTON) {
        let _ = button.set_position(LogicalPosition::new(x, y));
    }
}

/// Where the button currently is, in logical screen pixels.
pub fn position(app: &AppHandle) -> Option<(f64, f64)> {
    let button = app.get_webview_window(BUTTON)?;
    let scale = button.scale_factor().ok()?;
    let PhysicalPosition { x, y } = button.outer_position().ok()?;
    Some((x as f64 / scale, y as f64 / scale))
}

/// Shows or hides the dismiss target during a drag.
pub fn show_target(app: &AppHandle, showing: bool) {
    if let Some(target) = app.get_webview_window(TARGET) {
        if showing {
            let _ = target.show();
            let _ = target.set_always_on_top(true);
        } else {
            let _ = target.hide();
        }
    }
}

/// Whether the button's centre is over the dismiss target.
///
/// Measured against the target's real position rather than assumed geometry,
/// so the two cannot disagree about where it is.
pub fn over_target(app: &AppHandle, size: f64) -> bool {
    let (Some(button), Some(target)) = (
        app.get_webview_window(BUTTON),
        app.get_webview_window(TARGET),
    ) else {
        return false;
    };

    let Some((bx, by)) = position(app) else {
        return false;
    };
    let Ok(scale) = target.scale_factor() else {
        return false;
    };
    let Ok(tp) = target.outer_position() else {
        return false;
    };
    let (tx, ty) = (tp.x as f64 / scale, tp.y as f64 / scale);

    let (cx, cy) = (bx + size / 2.0, by + size / 2.0);
    let (ox, oy) = (tx + TARGET_SIZE / 2.0, ty + TARGET_SIZE / 2.0);

    // A circle, matching what is drawn. A square hit area around a round
    // target catches drops that visibly missed it.
    let radius = TARGET_SIZE / 2.0;
    ((cx - ox).powi(2) + (cy - oy).powi(2)).sqrt() <= radius

        && button.is_visible().unwrap_or(false)
}
