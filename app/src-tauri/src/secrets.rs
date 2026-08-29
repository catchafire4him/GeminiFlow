//! API key storage in Windows Credential Manager.
//!
//! Deliberately never written to the SQLite database or a config file: a
//! plaintext key on disk is the kind of thing that ends up in a backup or a
//! screen share.

use anyhow::{anyhow, Result};
use keyring::Entry;

const SERVICE: &str = "GeminiFlow";
const ACCOUNT: &str = "gemini-api-key";

fn entry() -> Result<Entry> {
    Entry::new(SERVICE, ACCOUNT).map_err(|e| anyhow!("credential store unavailable: {e}"))
}

pub fn get_api_key() -> Option<String> {
    let key = entry().ok()?.get_password().ok()?;
    let trimmed = key.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

pub fn has_api_key() -> bool {
    get_api_key().is_some()
}

pub fn set_api_key(key: &str) -> Result<()> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("the key is empty"));
    }
    entry()?
        .set_password(trimmed)
        .map_err(|e| anyhow!("could not save the key: {e}"))
}

pub fn clear_api_key() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) => Ok(()),
        // Already gone is a success from the caller's point of view.
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow!("could not remove the key: {e}")),
    }
}
