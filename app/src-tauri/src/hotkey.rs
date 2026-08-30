//! Global hold-to-talk key via a low-level keyboard hook.
//!
//! `tauri-plugin-global-shortcut` cannot do this: it reports the press edge
//! only, and hold-to-talk needs both edges. The hook also swallows the key so
//! it never reaches the focused app.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetLastInputInfo, LASTINPUTINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Dictation key held down.
    Press,
    /// Dictation key released.
    Release,
    /// Notes chord tapped. A toggle, because nobody holds a key for a
    /// ten-minute note.
    NotesToggle,
    /// Phone-call chord tapped. Also a toggle.
    CallToggle,
    /// The notes key was pressed but the modifiers did not match. Carries what
    /// was actually detected, so a chord that silently never fires can be
    /// diagnosed instead of guessed at. Logged by the engine, never in the
    /// hook -- file I/O inside a keyboard hook risks Windows unhooking us.
    NotesKeyMissedModifiers(u32),
}

const MOD_CTRL: u32 = 1;
const MOD_SHIFT: u32 = 2;

/// Notes chord, as (virtual key, required modifiers).
static NOTES_VK: AtomicU32 = AtomicU32::new(0xBA); // VK_OEM_1, the ";" key
static NOTES_MODS: AtomicU32 = AtomicU32::new(MOD_CTRL | MOD_SHIFT);
static CALL_VK: AtomicU32 = AtomicU32::new(0xDE); // VK_OEM_7, the quote key
static CALL_MODS: AtomicU32 = AtomicU32::new(MOD_CTRL | MOD_SHIFT);
static CALL_HELD: AtomicBool = AtomicBool::new(false);

pub fn set_call_binding(name: &str) {
    let (vk, mods) = match name {
        "CtrlShiftSemicolon" => (0xBA, MOD_CTRL | MOD_SHIFT),
        "CtrlShiftK" => (0x4B, MOD_CTRL | MOD_SHIFT),
        "F22" => (0x85, 0),
        _ => (0xDE, MOD_CTRL | MOD_SHIFT),
    };
    CALL_VK.store(vk, Ordering::SeqCst);
    CALL_MODS.store(mods, Ordering::SeqCst);
}

pub fn set_notes_binding(name: &str) {
    let (vk, mods) = match name {
        "CtrlShiftQuote" => (0xDE, MOD_CTRL | MOD_SHIFT),
        "F23" => (0x86, 0),
        _ => (0xBA, MOD_CTRL | MOD_SHIFT),
    };
    NOTES_VK.store(vk, Ordering::SeqCst);
    NOTES_MODS.store(mods, Ordering::SeqCst);
}

/// Reads live modifier state. Called from inside the hook, so it must stay
/// cheap -- GetAsyncKeyState is a register read, not a syscall round trip.
fn modifiers_held() -> u32 {
    let mut mods = 0;
    unsafe {
        if GetAsyncKeyState(0x11) as u16 & 0x8000 != 0 {
            mods |= MOD_CTRL;
        }
        if GetAsyncKeyState(0x10) as u16 & 0x8000 != 0 {
            mods |= MOD_SHIFT;
        }
    }
    mods
}

/// Which key is bound. Changed live from Settings, read inside the hook
/// callback, so it has to be an atomic rather than anything lock-based --
/// blocking inside a keyboard hook stalls input system-wide.
static BOUND_VK: AtomicU32 = AtomicU32::new(0xA3); // Right Ctrl
static HELD: AtomicBool = AtomicBool::new(false);
static NOTES_HELD: AtomicBool = AtomicBool::new(false);
static SENDER: OnceLock<Sender<Event>> = OnceLock::new();

/// Maps the UI's key names to virtual-key codes.
pub fn vk_for(name: &str) -> u32 {
    match name {
        "RightAlt" => 0xA5,
        "RightShift" => 0xA1,
        "F24" => 0x87,
        _ => 0xA3, // RightCtrl
    }
}

pub fn set_binding(name: &str) {
    BOUND_VK.store(vk_for(name), Ordering::SeqCst);
}

/// Every callback invocation, regardless of which key. Distinguishes "Windows
/// is not calling us" from "we are calling but filtering everything out" --
/// two failures that look identical from the outside.
pub static HOOK_CALLS: AtomicU32 = AtomicU32::new(0);

/// Times the hook saw the bound dictation key, before any filtering, and how
/// many of those carried the "injected" flag.
///
/// Counting only totals cannot tell "the key never reaches us" apart from "it
/// reaches us and our filtering drops it" -- and those need opposite fixes.
/// Only the bound keys are counted; this is not a keystroke log.
pub static DICT_KEY_SEEN: AtomicU32 = AtomicU32::new(0);
pub static DICT_KEY_INJECTED: AtomicU32 = AtomicU32::new(0);
pub static NOTES_KEY_SEEN: AtomicU32 = AtomicU32::new(0);
pub static CALL_KEY_SEEN: AtomicU32 = AtomicU32::new(0);

/// The live hook, so the watchdog can replace it.
static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// The thread owning the message pump. A WH_KEYBOARD_LL hook belongs to
/// the thread that installed it, so reinstalling has to happen there rather
/// than on the watchdog thread.
static PUMP_TID: AtomicU32 = AtomicU32::new(0);

/// How many times the hook has been rebuilt this run. Non-zero means the
/// shortcuts died at least once and recovered.
pub static REINSTALLS: AtomicU32 = AtomicU32::new(0);

/// Asks the pump thread to rebuild the hook.
const WM_REINSTALL_HOOK: u32 = WM_APP + 1;

/// Rebuild the hook every this many five-second rounds, so a hook Windows
/// has quietly discarded is never dead for more than about a minute.
const REBUILD_EVERY: u32 = 12;

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    HOOK_CALLS.fetch_add(1, Ordering::Relaxed);

    if code >= 0 {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);

        // Never react to our own SendInput traffic or we react to our own paste.
        let injected = kb.flags.0 & LLKHF_INJECTED.0 != 0;

        if kb.vkCode == BOUND_VK.load(Ordering::Relaxed) {
            DICT_KEY_SEEN.fetch_add(1, Ordering::Relaxed);
            if injected {
                DICT_KEY_INJECTED.fetch_add(1, Ordering::Relaxed);
            }
        }
        if kb.vkCode == NOTES_VK.load(Ordering::Relaxed) {
            NOTES_KEY_SEEN.fetch_add(1, Ordering::Relaxed);
        }
        if kb.vkCode == CALL_VK.load(Ordering::Relaxed) {
            CALL_KEY_SEEN.fetch_add(1, Ordering::Relaxed);
        }

        // Notes chord: fires on key-down only, and is swallowed so the
        // character never reaches the focused app.
        if !injected && kb.vkCode == NOTES_VK.load(Ordering::Relaxed) {
            let is_down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let required = NOTES_MODS.load(Ordering::Relaxed);
            let held = modifiers_held();
            if held & required == required {
                if is_down && !NOTES_HELD.swap(true, Ordering::SeqCst) {
                    emit(Event::NotesToggle);
                }
                if !is_down {
                    NOTES_HELD.store(false, Ordering::SeqCst);
                }
                return LRESULT(1);
            }
            if is_down {
                emit(Event::NotesKeyMissedModifiers(held));
            } else {
                NOTES_HELD.store(false, Ordering::SeqCst);
            }
        }

        if !injected && kb.vkCode == CALL_VK.load(Ordering::Relaxed) {
            let is_down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let required = CALL_MODS.load(Ordering::Relaxed);
            if modifiers_held() & required == required {
                if is_down && !CALL_HELD.swap(true, Ordering::SeqCst) {
                    emit(Event::CallToggle);
                }
                if !is_down {
                    CALL_HELD.store(false, Ordering::SeqCst);
                }
                return LRESULT(1);
            }
            if !is_down {
                CALL_HELD.store(false, Ordering::SeqCst);
            }
        }

        if !injected && kb.vkCode == BOUND_VK.load(Ordering::Relaxed) {
            match wparam.0 as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    // Auto-repeat fires keydown continuously while held.
                    if !HELD.swap(true, Ordering::SeqCst) {
                        emit(Event::Press);
                    }
                    return LRESULT(1); // swallow
                }
                WM_KEYUP | WM_SYSKEYUP => {
                    HELD.store(false, Ordering::SeqCst);
                    emit(Event::Release);
                    return LRESULT(1); // swallow
                }
                _ => {}
            }
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}

/// Counts events the hook could not deliver. Deliberately an atomic rather
/// than a log call: this runs inside the hook callback, and Windows silently
/// unhooks a low-level hook whose callback takes too long. File I/O here would
/// risk killing every shortcut in the app.
pub static DROPPED_EVENTS: AtomicU32 = AtomicU32::new(0);

fn emit(event: Event) {
    match SENDER.get() {
        Some(tx) => {
            if tx.send(event).is_err() {
                DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
            }
        }
        None => {
            DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

unsafe fn install_hook() -> Result<HHOOK> {
    let module = GetModuleHandleW(None)?;
    let hook: HHOOK = SetWindowsHookExW(
        WH_KEYBOARD_LL,
        Some(keyboard_proc),
        HINSTANCE(module.0),
        0,
    )?;
    if hook.is_invalid() {
        return Err(anyhow!("SetWindowsHookExW returned an invalid handle"));
    }
    Ok(hook)
}

/// Rebuilds the hook. Must run on the pump thread.
///
/// The new hook goes in before the old one comes out, so there is never an
/// instant with no hook installed and a keypress cannot fall through the
/// gap. Both are briefly live and a key would reach the callback twice,
/// which is harmless: every binding fires on a state change rather than on
/// the event itself, so the second call is swallowed by the same guard that
/// absorbs auto-repeat.
unsafe fn reinstall_hook() {
    let replacement = match install_hook() {
        Ok(hook) => hook,
        Err(e) => {
            // The old hook stays installed. It may be dead, but a dead hook
            // is no worse than none and this can be retried.
            crate::logln!("[hotkey] WARNING could not rebuild the hook: {e}");
            return;
        }
    };

    let old = HOOK_HANDLE.swap(replacement.0 as isize, Ordering::SeqCst);
    if old != 0 {
        // Expected to fail when Windows has already removed it, which is
        // the case being recovered from.
        let _ = UnhookWindowsHookEx(HHOOK(old as *mut core::ffi::c_void));
    }

    let n = REINSTALLS.fetch_add(1, Ordering::SeqCst) + 1;
    if crate::logging::debug_enabled() {
        crate::logln!("[hotkey] hook rebuilt (#{n} this run)");
    }
}

/// Tick of the last user input of any kind, mouse included.
fn last_input_tick() -> u32 {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    unsafe {
        let _ = GetLastInputInfo(&mut info);
    }
    info.dwTime
}

/// Installs the hook and pumps messages. Never returns under normal operation:
/// a WH_KEYBOARD_LL hook only delivers callbacks on a thread with a message
/// pump, so this must own a thread of its own.
pub fn install_and_pump(tx: Sender<Event>) -> Result<()> {
    SENDER
        .set(tx)
        .map_err(|_| anyhow!("keyboard hook already installed"))?;

    unsafe {
        PUMP_TID.store(GetCurrentThreadId(), Ordering::SeqCst);
        let hook = install_hook()?;
        HOOK_HANDLE.store(hook.0 as isize, Ordering::SeqCst);

        let notes_vk = NOTES_VK.load(Ordering::SeqCst);
        let call_vk = CALL_VK.load(Ordering::SeqCst);
        crate::logln!(
            "[hotkey] hook installed (dictation vk 0x{:02X}, notes vk 0x{:02X}, call vk 0x{:02X})",
            BOUND_VK.load(Ordering::SeqCst),
            notes_vk,
            call_vk
        );

        // The notes branch is checked first, so an identical binding means the
        // call chord can never fire -- it would silently start a note instead.
        if notes_vk == call_vk {
            crate::logln!(
                "[hotkey] WARNING notes and call are bound to the same key                  (0x{call_vk:02X}); the call chord will start a note instead"
            );
        }

        // Reports whether Windows is actually invoking the callback. Runs on
        // its own thread so the pump is never delayed.
        std::thread::spawn(|| {
            let mut last = 0u32;
            let mut last_calls = 0u32;
            let mut last_input = last_input_tick();
            let mut silent_rounds = 0u32;
            let mut rebuild_countdown = REBUILD_EVERY;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let calls = HOOK_CALLS.load(Ordering::Relaxed);

                // Windows silently removes a low-level hook whose callback
                // overruns LowLevelHooksTimeout -- 300 ms by default. There
                // is no notification and no API that reports it: the handle
                // stays valid and the callback simply stops being invoked.
                // That is exactly what "the shortcuts stopped working after
                // a while" looks like, and nothing here used to recover it.
                //
                // Windows silently removes a low-level hook whose callback
                // overruns its timeout, without notification, and the handle
                // stays valid afterwards. There is no way to ask whether it
                // is still installed.
                //
                // Two attempts at detecting it failed. Comparing user input
                // against callbacks treats anyone using only the mouse as a
                // dead hook. Sending a test keystroke looked exact, but
                // Windows blocks synthetic input while a higher-privilege
                // window has focus and -- documented behaviour -- reports
                // success anyway, so an installer being on screen was
                // indistinguishable from a dead hook.
                //
                // So the hook is simply rebuilt on a schedule. Recovery
                // takes at most a minute, it cannot be fooled because it
                // asks nothing, and rebuilding a healthy hook costs
                // microseconds and drops no keys.
                rebuild_countdown -= 1;
                if rebuild_countdown == 0 {
                    rebuild_countdown = REBUILD_EVERY;
                    // Already inside the enclosing unsafe block.
                    let _ = PostThreadMessageW(
                        PUMP_TID.load(Ordering::SeqCst),
                        WM_REINSTALL_HOOK,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }

                // Kept purely as a record. It is not acted on, but a long
                // run of these alongside a report of dead shortcuts is what
                // would show the rebuild interval is too slow.
                let input = last_input_tick();
                if input != last_input && calls == last_calls {
                    silent_rounds += 1;
                    if silent_rounds == 6 && crate::logging::debug_enabled() {
                        crate::logln!(
                            "[hotkey] 30s of user input with no keyboard callbacks \
                             (mouse-only use looks the same)"
                        );
                    }
                } else {
                    silent_rounds = 0;
                }
                last_input = input;
                last_calls = calls;

                if calls != last && crate::logging::debug_enabled() {
                    crate::logln!(
                        "[hotkey] {calls} callbacks (+{}), dictation key seen {} \
                         ({} injected), notes key seen {}, call key seen {}, dropped {}",
                        calls - last,
                        DICT_KEY_SEEN.load(Ordering::Relaxed),
                        DICT_KEY_INJECTED.load(Ordering::Relaxed),
                        NOTES_KEY_SEEN.load(Ordering::Relaxed),
                        CALL_KEY_SEEN.load(Ordering::Relaxed),
                        DROPPED_EVENTS.load(Ordering::Relaxed)
                    );
                    last = calls;
                }
            }
        });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // Posted by the watchdog. Handled here because the hook belongs
            // to this thread; a thread message has no window to dispatch to
            // anyway.
            if msg.message == WM_REINSTALL_HOOK {
                reinstall_hook();
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Reaching here means the pump exited, which destroys the hook and kills
    // every shortcut. It should never happen while the app is running.
    crate::logln!("[hotkey] WARNING message pump exited -- the hook is now gone");
    Ok(())
}

/// Lets the watchdog stop a notes recording through the same path a keypress
/// would take, so auto-stop and manual stop cannot diverge.
pub fn request_notes_stop() {
    emit(Event::NotesToggle);
}

/// The three shortcuts, triggerable without a keyboard.
///
/// These feed the same event channel the hook does, so a dictation started
/// from a Stream Deck and one started from Right Ctrl are the same session as
/// far as everything downstream is concerned -- there is no second code path
/// to keep in step.
pub fn request_press() {
    emit(Event::Press);
}

pub fn request_release() {
    emit(Event::Release);
}

pub fn request_notes_toggle() {
    emit(Event::NotesToggle);
}

pub fn request_call_toggle() {
    emit(Event::CallToggle);
}

/// Feeds an event in from the UI.
///
/// Measured: while GeminiFlow's own window has focus, the low-level hook
/// receives nothing at all -- not the bound keys, not any key. The WebView
/// still gets those keystrokes, so the frontend forwards them here and the
/// engine cannot tell the difference. This covers the one case the hook
/// cannot, without giving up key suppression everywhere else.
pub fn inject_event(event: Event) {
    emit(event);
}
