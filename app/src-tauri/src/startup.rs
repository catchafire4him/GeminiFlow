//! Launch at login, via the per-user Run key.
//!
//! HKCU rather than HKLM deliberately: no elevation needed, and it only ever
//! affects the user who turned it on. The registered command carries
//! `--hidden` so a login launch goes straight to the tray instead of throwing
//! a window in your face every time you boot.

use anyhow::{anyhow, Result};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "GeminiFlow";

/// The flag a login launch passes so the window stays hidden.
pub const HIDDEN_FLAG: &str = "--hidden";

fn command_line() -> Result<String> {
    let exe = std::env::current_exe()?;
    // Quoted: the path routinely contains spaces under Program Files.
    Ok(format!("\"{}\" {HIDDEN_FLAG}", exe.display()))
}

pub fn is_enabled() -> bool {
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ)
    else {
        return false;
    };
    key.get_value::<String, _>(VALUE_NAME).is_ok()
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey_with_flags(RUN_KEY, KEY_WRITE | KEY_READ)
        .map_err(|e| anyhow!("could not open the startup registry key: {e}"))?;

    if enabled {
        let command = command_line()?;
        key.set_value(VALUE_NAME, &command)
            .map_err(|e| anyhow!("could not register for startup: {e}"))?;
        crate::logln!("[startup] registered: {command}");
    } else {
        match key.delete_value(VALUE_NAME) {
            Ok(()) => crate::logln!("[startup] unregistered"),
            // Already absent is the state the caller asked for.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(anyhow!("could not unregister from startup: {e}")),
        }
    }
    Ok(())
}

/// True when this process was launched by the login entry.
pub fn launched_hidden() -> bool {
    std::env::args().any(|a| a == HIDDEN_FLAG)
}

/// Re-points the registered command at the current executable.
///
/// Without this, moving or reinstalling the app leaves the Run key aimed at an
/// executable that no longer exists, and startup silently stops working.
pub fn refresh_if_enabled() {
    if !is_enabled() {
        return;
    }
    let Ok(expected) = command_line() else { return };
    let current = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_READ)
        .ok()
        .and_then(|k| k.get_value::<String, _>(VALUE_NAME).ok());

    if current.as_deref() != Some(expected.as_str()) {
        let _ = set_enabled(true);
    }
}
