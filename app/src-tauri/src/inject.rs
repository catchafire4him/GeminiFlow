//! Text injection: restore focus, put text on the clipboard, synthesize a
//! paste, restore the clipboard off the critical path.
//!
//! Three details decide whether this works everywhere or only in Notepad:
//!
//! 1. Shift+Insert, not Ctrl+V. Ctrl+V is not paste in mintty/Git Bash and is
//!    unreliable in conhost. Shift+Insert is paste in Windows Terminal, VS
//!    Code, browsers and Win32 apps alike.
//! 2. Modifier hygiene. The user released the hotkey milliseconds ago; a
//!    modifier Windows still believes is down silently turns the paste into a
//!    different command.
//! 3. The clipboard restore runs on a background thread. Waiting for it added
//!    250ms to every dictation for work the user never sees.

use std::thread::sleep;
use std::time::Duration;

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::ProcessStatus::K32GetModuleBaseNameW;
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
    PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, SetFocus, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, SetForegroundWindow,
};

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
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

const MODIFIERS: &[u16] = &[
    VK_SHIFT, VK_CONTROL, VK_MENU, VK_LSHIFT, VK_RSHIFT, VK_LCONTROL, VK_RCONTROL,
    VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PasteMode {
    ShiftInsert,
    CtrlV,
    /// Types the text as Unicode keystrokes. Slower, but depends on no
    /// clipboard and no paste shortcut, so it reaches editors that ignore
    /// both -- browser-based editors especially.
    TypeUnicode,
}

impl PasteMode {
    pub fn from_name(name: &str) -> PasteMode {
        match name {
            "ctrlV" => PasteMode::CtrlV,
            "typeUnicode" => PasteMode::TypeUnicode,
            _ => PasteMode::ShiftInsert,
        }
    }
}

pub fn foreground_window() -> HWND {
    unsafe { GetForegroundWindow() }
}

/// True if the window belongs to GeminiFlow itself.
///
/// Dictating while our own window is focused would paste the text into our UI,
/// which has no text field to receive it -- so nothing appears and the hotkey
/// looks broken. Worth detecting rather than silently doing nothing.
pub fn is_own_window(hwnd: HWND) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid != 0 && pid == GetCurrentProcessId()
    }
}

/// Executable name of the window that owns `hwnd`, for the history view.
pub fn window_process_name(hwnd: HWND) -> Option<String> {
    if hwnd.is_invalid() {
        return None;
    }
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        let handle =
            OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;

        let mut buf = [0u16; 260];
        let len = K32GetModuleBaseNameW(handle, None, &mut buf);
        let _ = CloseHandle(handle);

        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Short label for logging: "Code.exe — main.rs - myproject".
pub fn describe_window(hwnd: HWND) -> String {
    if hwnd.is_invalid() {
        return "<no window>".to_string();
    }
    let process = window_process_name(hwnd).unwrap_or_else(|| "<unknown>".into());
    let mut buf = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    let title = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
    if title.is_empty() {
        process
    } else {
        format!("{process} — {title}")
    }
}

/// Puts `target` back in front.
///
/// Windows refuses `SetForegroundWindow` from a process that does not own the
/// current foreground window — that is the anti-focus-stealing rule, and it is
/// exactly the situation a background dictation tool is in. Attaching to the
/// foreground thread's input queue first is the standard way around it.
fn restore_focus(target: HWND) -> bool {
    if target.is_invalid() {
        crate::logln!("[inject] no target window captured; pasting into whatever has focus");
        return true;
    }

    let current = unsafe { GetForegroundWindow() };
    if current == target {
        return true; // already there; do not disturb it
    }

    crate::logln!(
        "[inject] focus moved since key-down: now {}, restoring {}",
        describe_window(current),
        describe_window(target)
    );

    unsafe {
        if SetForegroundWindow(target).as_bool() {
            sleep(Duration::from_millis(20));
            if GetForegroundWindow() == target {
                return true;
            }
        }

        // Refused. Attach to the foreground window's input queue, which makes
        // Windows treat us as entitled to change focus. Retried, because the
        // target app is sometimes mid-transition and takes a moment to accept.
        let mut target_pid = 0u32;
        let target_thread = GetWindowThreadProcessId(target, Some(&mut target_pid));
        let us = GetCurrentThreadId();

        for attempt in 0..3 {
            let foreground = GetForegroundWindow();
            if foreground == target {
                return true;
            }
            let foreground_thread = GetWindowThreadProcessId(foreground, None);

            let attached_fg = AttachThreadInput(us, foreground_thread, true).as_bool();
            let attached_target = AttachThreadInput(us, target_thread, true).as_bool();

            let _ = SetForegroundWindow(target);
            let _ = SetFocus(target);

            if attached_target {
                let _ = AttachThreadInput(us, target_thread, false);
            }
            if attached_fg {
                let _ = AttachThreadInput(us, foreground_thread, false);
            }

            sleep(Duration::from_millis(30 * (attempt + 1)));
            if GetForegroundWindow() == target {
                return true;
            }
        }

        crate::logln!(
            "[inject] could not restore focus to {}",
            describe_window(target)
        );
        false
    }
}

pub fn paste_into(target: HWND, text: &str, mode: PasteMode) -> Result<()> {
    // Refuse rather than paste into the wrong place. Dictated text landing in
    // an unintended app is worse than not landing at all -- it could go into a
    // message box and be sent. The transcript is still saved to history.
    if !restore_focus(target) {
        return Err(anyhow!(
            "could not switch back to {} -- the text is in your history, and \
             clicking into that window before dictating avoids this",
            describe_window(target)
        ));
    }

    if mode == PasteMode::TypeUnicode {
        release_stuck_modifiers();
        return type_unicode(text);
    }

    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| anyhow!("clipboard unavailable: {e}"))?;
    let previous = clipboard.get_text().ok();

    set_text_with_retry(&mut clipboard, text)?;
    release_stuck_modifiers();
    send_paste_chord(mode)?;

    // Restore off the critical path: the paste has already landed.
    if let Some(prev) = previous {
        let ours = text.to_string();
        std::thread::spawn(move || {
            sleep(Duration::from_millis(250));
            let Ok(mut clipboard) = arboard::Clipboard::new() else {
                return;
            };
            // Leave it alone if the user copied something in the meantime.
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
        // Handled before this point by typing the text directly.
        PasteMode::TypeUnicode => return Ok(()),
    };

    let events = [
        key_event(modifier, false),
        key_event(key, false),
        key_event(key, true),
        key_event(modifier, true),
    ];

    crate::logln!(
        "[inject] sending {:?} to {}",
        mode,
        describe_window(unsafe { GetForegroundWindow() })
    );

    let sent = send(&events);
    if sent != events.len() as u32 {
        return Err(anyhow!(
            "SendInput accepted {sent}/{} events -- the focused window is \
             probably running as administrator, which requires GeminiFlow to \
             be elevated too",
            events.len()
        ));
    }
    Ok(())
}

/// Sends `text` as synthetic Unicode keystrokes.
///
/// `KEYEVENTF_UNICODE` bypasses the keyboard layout and delivers the character
/// straight to the focused control, so it works in places that ignore paste
/// shortcuts. Sent in chunks because some apps drop input delivered in one
/// very large batch.
fn type_unicode(text: &str) -> Result<()> {
    crate::logln!(
        "[inject] typing {} chars as Unicode into {}",
        text.chars().count(),
        describe_window(unsafe { GetForegroundWindow() })
    );

    // UTF-16, since each INPUT carries one code unit; surrogate pairs go as
    // two consecutive events, which is what Windows expects.
    let units: Vec<u16> = text.encode_utf16().collect();

    for chunk in units.chunks(64) {
        let mut events = Vec::with_capacity(chunk.len() * 2);
        for &unit in chunk {
            events.push(unicode_event(unit, false));
            events.push(unicode_event(unit, true));
        }
        let sent = send(&events);
        if sent != events.len() as u32 {
            return Err(anyhow!(
                "SendInput accepted {sent}/{} events -- the focused window is \
                 probably running as administrator, which requires GeminiFlow \
                 to be elevated too",
                events.len()
            ));
        }
        sleep(Duration::from_millis(2));
    }

    Ok(())
}

fn unicode_event(unit: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0), // must be 0 for KEYEVENTF_UNICODE
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
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
