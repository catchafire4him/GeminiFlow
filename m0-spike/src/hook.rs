//! Low-level keyboard hook. This is the piece `tauri-plugin-global-shortcut`
//! cannot do: it reports both the press and the release edge of Right Ctrl,
//! and swallows the key so it never reaches the focused app.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// Right Ctrl. Chosen because almost nothing binds it, so swallowing it is cheap.
const VK_RCONTROL: u32 = 0xA3;

#[derive(Debug, Clone, Copy)]
pub enum Event {
    Press,
    Release,
}

static SENDER: OnceLock<Sender<Event>> = OnceLock::new();
/// Guards against key auto-repeat firing Press over and over while held.
static HELD: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);

        // Never react to our own SendInput traffic, or we deadlock on our own paste.
        let injected = kb.flags.0 & LLKHF_INJECTED.0 != 0;

        if !injected && kb.vkCode == VK_RCONTROL {
            let msg = wparam.0 as u32;
            match msg {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    if !HELD.swap(true, Ordering::SeqCst) {
                        if let Some(tx) = SENDER.get() {
                            let _ = tx.send(Event::Press);
                        }
                    }
                    return LRESULT(1); // swallow
                }
                WM_KEYUP | WM_SYSKEYUP => {
                    HELD.store(false, Ordering::SeqCst);
                    if let Some(tx) = SENDER.get() {
                        let _ = tx.send(Event::Release);
                    }
                    return LRESULT(1); // swallow
                }
                _ => {}
            }
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}

/// Installs the hook and runs a message pump. Never returns under normal use --
/// a WH_KEYBOARD_LL hook only delivers callbacks on a thread that pumps messages.
pub fn install_and_pump(tx: Sender<Event>) -> Result<()> {
    SENDER
        .set(tx)
        .map_err(|_| anyhow!("hook sender already installed"))?;

    unsafe {
        let module = GetModuleHandleW(None)?;
        let hook: HHOOK = SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_proc),
            HINSTANCE(module.0),
            0,
        )?;

        if hook.is_invalid() {
            return Err(anyhow!("SetWindowsHookExW returned an invalid hook"));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}
