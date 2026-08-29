//! Text injection: restore focus, put text on the clipboard, synthesize a
//! paste, put the old clipboard back.
//!
//! Two details here are the difference between "works everywhere" and "works
//! in Notepad":
//!
//! 1. Shift+Insert, not Ctrl+V. Ctrl+V is not paste in mintty/Git Bash and is
//!    historically unreliable in conhost. Shift+Insert is paste in Windows
//!    Terminal, mintty, VS Code, browsers and Win32 apps alike.
//!
//! 2. Modifier hygiene. The user was physically holding Right Ctrl a few
//!    milliseconds ago. A modifier the OS still believes is down silently
//!    turns the paste into a different command -- a stuck Shift makes
//!    Ctrl+V into Ctrl+Shift+V, which is a *different* action in terminals
//!    and browsers.

use std::thread::sleep;
use std::time::Duration;

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12; // Alt
const VK_LWIN: u16 = 0x5B;
const VK_RWIN: u16 = 0x5C;
const VK_INSERT: u16 = 0x2D;
const VK_LSHIFT: u16 = 0xA0;
const VK_RSHIFT: u16 = 0xA1;
const VK_LCONTROL: u16 = 0xA2;
const VK_RCONTROL: u16 = 0xA3;
const VK_LMENU: u16 = 0xA4;
const VK_RMENU: u16 = 0xA5;
const VK_V: u16 = 0x56;

/// Every modifier that could be physically held and poison the paste chord.
const MODIFIERS: &[u16] = &[
    VK_SHIFT, VK_CONTROL, VK_MENU, VK_LSHIFT, VK_RSHIFT, VK_LCONTROL, VK_RCONTROL,
    VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN,
];

#[derive(Clone, Copy, Debug)]
pub enum PasteMode {
    ShiftInsert,
    CtrlV,
}

impl PasteMode {
    pub fn from_env() -> PasteMode {
        match std::env::var("M0_PASTE").unwrap_or_default().as_str() {
            "ctrl_v" => PasteMode::CtrlV,
            _ => PasteMode::ShiftInsert,
        }
    }
}

pub fn foreground_window() -> HWND {
    unsafe { GetForegroundWindow() }
}

/// Puts `text` into the window that was focused when recording started.
pub fn paste_into(target: HWND, text: &str, mode: PasteMode) -> Result<()> {
    // 1. Point focus back where it was. It can move between press and release.
    if !target.is_invalid() {
        unsafe {
            let _ = SetForegroundWindow(target);
        }
        sleep(Duration::from_millis(20));
    }

    // 2. Save whatever the user had on the clipboard.
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| anyhow!("clipboard unavailable: {e}"))?;
    let previous = clipboard.get_text().ok();

    // 3. Our text goes on. Retry: another process can hold the clipboard open.
    set_text_with_retry(&mut clipboard, text)?;

    // 4. Release anything still physically held, then send the chord.
    release_stuck_modifiers();
    send_paste_chord(mode)?;

    // 5. Restore on a background thread. The paste has already landed by this
    //    point, so waiting here would add 250ms of dead time to every
    //    dictation for work the user never sees.
    if let Some(prev) = previous {
        let ours = text.to_string();
        std::thread::spawn(move || {
            sleep(Duration::from_millis(250));
            let Ok(mut clipboard) = arboard::Clipboard::new() else {
                return;
            };
            // Only restore if our text is still there. If the user copied
            // something in the meantime, leave theirs alone.
            if clipboard.get_text().map(|c| c == ours).unwrap_or(false) {
                let _ = clipboard.set_text(prev);
            }
        });
    }

    Ok(())
}

fn set_text_with_retry(clipboard: &mut arboard::Clipboard, text: &str) -> Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        match clipboard.set_text(text) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                sleep(Duration::from_millis(15 * (attempt + 1)));
            }
        }
    }
    Err(anyhow!(
        "could not take the clipboard after 5 tries: {}",
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

fn is_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

fn release_stuck_modifiers() {
    let stuck: Vec<INPUT> = MODIFIERS
        .iter()
        .copied()
        .filter(|&vk| is_down(vk))
        .map(|vk| key_event(vk, true))
        .collect();

    if !stuck.is_empty() {
        send(&stuck);
        sleep(Duration::from_millis(10));
    }
}

fn send_paste_chord(mode: PasteMode) -> Result<()> {
    let (modifier, key) = match mode {
        PasteMode::ShiftInsert => (VK_SHIFT, VK_INSERT),
        PasteMode::CtrlV => (VK_CONTROL, VK_V),
    };

    let events = [
        key_event(modifier, false),
        key_event(key, false),
        key_event(key, true),
        key_event(modifier, true),
    ];

    let sent = send(&events);
    if sent != events.len() as u32 {
        return Err(anyhow!(
            "SendInput accepted {sent}/{} events -- a higher-integrity window \
             is probably focused (run elevated to inject into admin apps)",
            events.len()
        ));
    }
    Ok(())
}

fn key_event(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(events: &[INPUT]) -> u32 {
    unsafe { SendInput(events, std::mem::size_of::<INPUT>() as i32) }
}
