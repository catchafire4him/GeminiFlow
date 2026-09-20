//! The floating status pill.
//!
//! Two properties matter more than anything it displays:
//!
//! 1. **It must never take focus.** Dictation pastes into the window that had
//!    focus when the key went down; an overlay that activates on show would
//!    steal that focus and break the entire feature. Tauri's `focus: false`
//!    covers creation, but `WS_EX_NOACTIVATE` is set explicitly so showing it
//!    later cannot activate it either.
//! 2. **It must not eat clicks.** It sits over other apps, so cursor events
//!    pass straight through.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager, PhysicalPosition};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

pub const LABEL: &str = "overlay";

/// Bumped on every state change so a scheduled hide can tell whether it is
/// still the most recent intent, rather than hiding a session that has since
/// started.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn setup(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        crate::logln!("[overlay] window missing from the Tauri config");
        return;
    };

    // Clicks pass through to whatever is underneath.
    let _ = window.set_ignore_cursor_events(true);

    match window.hwnd() {
        // Tauri bundles its own `windows` crate version, so its HWND is a
        // distinct type from ours; both are newtypes over the same pointer.
        Ok(hwnd) => make_non_activating(HWND(hwnd.0 as _)),
        Err(e) => crate::logln!("[overlay] could not get window handle: {e}"),
    }

    position(app);
}

/// Adds WS_EX_NOACTIVATE (never take focus when shown) and WS_EX_TOOLWINDOW
/// (stay out of Alt+Tab).
pub fn make_non_activating(hwnd: HWND) {
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let wanted =
            current | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);

        let applied = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if applied & WS_EX_NOACTIVATE.0 as isize == 0 {
            // Worth shouting about: without this the overlay steals the focus
            // that dictation depends on.
            crate::logln!("[overlay] WARNING WS_EX_NOACTIVATE did not stick");
        }
    }
}

/// Bottom-centre of the monitor, clear of the taskbar.
pub fn position(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };

    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let screen = monitor.size();
    let scale = monitor.scale_factor();
    let Ok(size) = window.outer_size() else { return };

    let x = (screen.width as i32 - size.width as i32) / 2;
    let y = screen.height as i32 - size.height as i32 - (110.0 * scale) as i32;

    let _ = window.set_position(PhysicalPosition::new(
        monitor.position().x + x,
        monitor.position().y + y,
    ));
}

pub fn show(app: &AppHandle) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        return;
    }
    // Re-position on each appearance so it follows the active monitor.
    position(app);
    let _ = window.show();
}

/// Hides after `delay`, unless another state change has happened since.
pub fn hide_after(app: &AppHandle, delay: Duration) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();

    std::thread::spawn(move || {
        std::thread::sleep(delay);
        if GENERATION.load(Ordering::SeqCst) != generation {
            return; // superseded
        }
        if let Some(window) = app.get_webview_window(LABEL) {
            let _ = window.hide();
        }
    });
}
