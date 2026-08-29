use std::sync::Arc;

use tauri::State;

use crate::engine::{AppState, Status};
use crate::settings::Settings;
use crate::store::{Dictation, Note, NoteSummary};
use crate::{audio, gemini, hotkey, secrets};

/// Tauri commands must return a serializable error; anyhow is not.
type CmdResult<T> = Result<T, String>;

fn fail(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn get_state(state: State<'_, Arc<AppState>>) -> Status {
    state.status.lock().map(|s| s.clone()).unwrap_or_default()
}

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Settings {
    state.settings.lock().map(|s| s.clone()).unwrap_or_default()
}

#[tauri::command]
pub fn save_settings(
    settings: Settings,
    state: State<'_, Arc<AppState>>,
) -> CmdResult<()> {
    settings.save(&state.store).map_err(fail)?;

    // Rebinding has to reach the keyboard hook, which reads an atomic rather
    // than taking a lock -- blocking inside a hook stalls input system-wide.
    hotkey::set_binding(&settings.hotkey);
    hotkey::set_notes_binding(&settings.notes_hotkey);
    hotkey::set_call_binding(&settings.call_hotkey);
    crate::logging::set_debug(settings.debug_logging);

    if let Err(e) = crate::startup::set_enabled(settings.launch_at_login) {
        return Err(e.to_string());
    }

    if let Ok(mut current) = state.settings.lock() {
        *current = settings;
    }
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDevice {
    pub name: String,
    pub is_default: bool,
}

#[tauri::command]
pub fn list_input_devices() -> CmdResult<Vec<InputDevice>> {
    audio::list_input_devices()
        .map(|devices| {
            devices
                .into_iter()
                .map(|d| InputDevice {
                    name: d.name,
                    is_default: d.is_default,
                })
                .collect()
        })
        .map_err(fail)
}

#[tauri::command]
pub fn get_vocabulary(state: State<'_, Arc<AppState>>) -> Vec<String> {
    state.store.vocabulary()
}

#[tauri::command]
pub fn save_vocabulary(
    terms: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> CmdResult<()> {
    state.store.set_vocabulary(&terms).map_err(fail)
}

#[tauri::command]
pub fn list_dictations(limit: i64, state: State<'_, Arc<AppState>>) -> Vec<Dictation> {
    state.store.list_dictations(limit.clamp(1, 1000))
}

#[tauri::command]
pub fn delete_dictation(id: i64, state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.store.delete_dictation(id).map_err(fail)
}

#[tauri::command]
pub fn list_notes(limit: i64, state: State<'_, Arc<AppState>>) -> Vec<NoteSummary> {
    state.store.list_notes(limit.clamp(1, 500))
}

#[tauri::command]
pub fn get_note(id: i64, state: State<'_, Arc<AppState>>) -> Option<Note> {
    state.store.note(id)
}

#[tauri::command]
pub fn set_action_done(
    id: i64,
    done: bool,
    state: State<'_, Arc<AppState>>,
) -> CmdResult<()> {
    state.store.set_action_done(id, done).map_err(fail)
}

/// Re-runs summarisation on a note whose structuring failed at record time.
#[tauri::command]
pub fn retry_note_summary(id: i64, state: State<'_, Arc<AppState>>) -> CmdResult<Option<Note>> {
    let transcript = state
        .store
        .transcript_of(id)
        .ok_or("that note no longer exists")?;
    let api_key = secrets::get_api_key().ok_or("no API key set")?;

    let model = state
        .settings
        .lock()
        .map(|s| s.notes_model.clone())
        .unwrap_or_else(|_| gemini::NOTES_MODEL.to_string());

    let client = crate::gemini::notes::NotesClient::new().map_err(fail)?;
    // Re-uses the template the note was captured with, so a call is never
    // re-summarised as an ordinary note.
    let template = match state.store.note(id).map(|n| n.kind) {
        Some(kind) if kind == "call" => {
            let speakerphone = state
                .settings
                .lock()
                .map(|s| s.speakerphone)
                .unwrap_or(false);
            if speakerphone {
                crate::gemini::notes::SPEAKERPHONE_TEMPLATE
            } else {
                crate::gemini::notes::CALL_TEMPLATE
            }
        }
        _ => crate::gemini::notes::DEFAULT_TEMPLATE,
    };

    let draft = client
        .structure(&api_key, &model, template, &transcript)
        .map_err(fail)?;

    state.store.apply_summary(id, &draft).map_err(fail)?;
    Ok(state.store.note(id))
}

#[tauri::command]
pub fn delete_note(id: i64, state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.store.delete_note(id).map_err(fail)
}

/// Opens the note in VS Code as a new untitled buffer. `code -` reads markdown
/// from stdin, which works regardless of which workspace happens to be open.
#[tauri::command]
pub fn send_to_vscode(markdown: String) -> CmdResult<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("cmd")
        .args(["/C", "code", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not launch VS Code ({e}). Is `code` on your PATH?"))?;

    child
        .stdin
        .as_mut()
        .ok_or("could not write to VS Code")?
        .write_all(markdown.as_bytes())
        .map_err(fail)?;

    Ok(())
}

/// Hotkey forwarded from the UI, for when our own window has focus and the
/// global hook is not being called. See `hotkey::inject_event`.
#[tauri::command]
pub fn ui_hotkey(kind: String) -> CmdResult<()> {
    let event = match kind.as_str() {
        "press" => hotkey::Event::Press,
        "release" => hotkey::Event::Release,
        "notesToggle" => hotkey::Event::NotesToggle,
        "callToggle" => hotkey::Event::CallToggle,
        other => return Err(format!("unknown hotkey event {other}")),
    };
    hotkey::inject_event(event);
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataStats {
    pub folder: String,
    pub database_bytes: u64,
    pub audio_bytes: u64,
    pub audio_files: u64,
    pub dictation_count: i64,
    pub note_count: i64,
}

#[tauri::command]
pub fn data_stats(state: State<'_, Arc<AppState>>) -> CmdResult<DataStats> {
    let dir = crate::store::data_dir().map_err(fail)?;
    let database_bytes = std::fs::metadata(dir.join("geminiflow.db"))
        .map(|m| m.len())
        .unwrap_or(0);

    let (mut audio_bytes, mut audio_files) = (0u64, 0u64);
    if let Ok(entries) = std::fs::read_dir(dir.join("recordings")) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    audio_bytes += meta.len();
                    audio_files += 1;
                }
            }
        }
    }

    let (dictation_count, note_count) = state.store.counts();

    Ok(DataStats {
        folder: dir.to_string_lossy().to_string(),
        database_bytes,
        audio_bytes,
        audio_files,
        dictation_count,
        note_count,
    })
}

#[tauri::command]
pub fn open_data_folder() -> CmdResult<()> {
    let dir = crate::store::data_dir().map_err(fail)?;
    std::process::Command::new("explorer")
        .arg(dir)
        .spawn()
        .map_err(|e| format!("could not open the folder: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn clear_dictations(state: State<'_, Arc<AppState>>) -> CmdResult<usize> {
    let removed = state.store.clear_dictations().map_err(fail)?;
    crate::logln!("[data] cleared {removed} dictations");
    Ok(removed)
}

#[tauri::command]
pub fn delete_all_notes(state: State<'_, Arc<AppState>>) -> CmdResult<usize> {
    let removed = state.store.delete_all_notes().map_err(fail)?;
    crate::logln!("[data] deleted {removed} notes and their audio");
    Ok(removed)
}

#[tauri::command]
pub fn has_api_key() -> bool {
    secrets::has_api_key()
}

#[tauri::command]
pub fn set_api_key(key: String, state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    secrets::set_api_key(&key).map_err(fail)?;

    // A first key on a fresh install means an empty vocabulary; seed it so
    // dictation is useful immediately rather than after manual setup.
    if state.store.vocabulary().is_empty() {
        let _ = state.store.set_vocabulary(&gemini::starter_vocabulary());
    }
    Ok(())
}

#[tauri::command]
pub fn clear_api_key() -> CmdResult<()> {
    secrets::clear_api_key().map_err(fail)
}
