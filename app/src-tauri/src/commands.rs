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
    crate::sound::set_enabled(settings.sounds_enabled);

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

/// Rebuilds notes from audio files the database has lost track of.
///
/// The recording is the irreplaceable part; a note row can be rebuilt from it.
/// Each recovered note is transcribed and saved unsummarised, so it appears
/// immediately and can be summarised with one click.
#[tauri::command]
pub fn recover_recordings(state: State<'_, Arc<AppState>>) -> CmdResult<usize> {
    let dir = crate::store::recordings_dir().map_err(fail)?;
    let api_key = secrets::get_api_key().ok_or("no API key set")?;
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or_default();

    let known: std::collections::HashSet<String> = state
        .store
        .known_audio_paths()
        .into_iter()
        .map(|p| p.to_lowercase())
        .collect();

    let mut orphans: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map_err(fail)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "wav").unwrap_or(false))
        .filter(|p| !known.contains(&p.to_string_lossy().to_lowercase()))
        .collect();
    orphans.sort();

    if orphans.is_empty() {
        return Ok(0);
    }
    crate::logln!("[data] recovering {} orphaned recordings", orphans.len());

    let batch = crate::gemini::batch::BatchClient::new().map_err(fail)?;
    let vocabulary = state.store.vocabulary();
    let mut recovered = 0usize;

    for path in orphans {
        let Ok(wav) = std::fs::read(&path) else { continue };
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let kind = if name.starts_with("call-") { "call" } else { "note" };

        let transcript = match batch.transcribe(
            &api_key,
            gemini::BATCH_MODEL,
            &wav,
            &vocabulary,
            &settings.language,
            false,
        ) {
            Ok(t) if !t.trim().is_empty() => t,
            Ok(_) => {
                crate::logln!("[data] {name}: no speech found, skipped");
                continue;
            }
            Err(e) => {
                crate::logln!("[data] {name}: transcription failed ({e})");
                continue;
            }
        };

        // Duration from file size: 16 kHz mono 16-bit, minus the 44-byte header.
        let duration_ms = ((wav.len().saturating_sub(44)) as i64 * 1000) / (16_000 * 2);
        let title: String = transcript.split_whitespace().take(8).collect::<Vec<_>>().join(" ");

        let draft = crate::store::NoteDraft {
            title: if title.is_empty() { "Recovered recording".into() } else { format!("{title}…") },
            ..Default::default()
        };

        let note = state
            .store
            .insert_note(
                &draft,
                &transcript,
                Some(&path.to_string_lossy()),
                duration_ms,
                true,
                kind,
                false,
                0,
            )
            .map_err(fail)?;

        if let Some(ts) = timestamp_from_name(&name) {
            let _ = state.store.set_note_created(note.id, &ts);
        }

        crate::logln!("[data] recovered {name} as note {}", note.id);
        recovered += 1;
    }

    Ok(recovered)
}

/// "note-20260828-195804.wav" -> RFC3339. Filenames are written in UTC.
fn timestamp_from_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".wav")?;
    let (_, rest) = stem.split_once('-')?;
    let (date, time) = rest.split_once('-')?;
    if date.len() != 8 || time.len() != 6 {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}:{}:{}+00:00",
        &date[0..4], &date[4..6], &date[6..8],
        &time[0..2], &time[2..4], &time[4..6]
    ))
}

/// Audio files on disk that no note references.
fn orphaned_files(state: &AppState) -> CmdResult<Vec<std::path::PathBuf>> {
    let dir = crate::store::recordings_dir().map_err(fail)?;
    let known: std::collections::HashSet<String> = state
        .store
        .known_audio_paths()
        .into_iter()
        .map(|p| p.to_lowercase())
        .collect();

    let mut orphans: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map_err(fail)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "wav").unwrap_or(false))
        .filter(|p| !known.contains(&p.to_string_lossy().to_lowercase()))
        .collect();
    orphans.sort();
    Ok(orphans)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanInfo {
    pub count: usize,
    pub bytes: u64,
}

#[tauri::command]
pub fn orphaned_audio(state: State<'_, Arc<AppState>>) -> CmdResult<OrphanInfo> {
    let orphans = orphaned_files(&state)?;
    let bytes = orphans
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    Ok(OrphanInfo {
        count: orphans.len(),
        bytes,
    })
}

/// Deletes orphaned audio outright.
///
/// The counterpart to recovery: an orphan is either a note worth rebuilding or
/// a leftover from one you deliberately deleted, and only you know which.
#[tauri::command]
pub fn delete_orphaned_audio(state: State<'_, Arc<AppState>>) -> CmdResult<usize> {
    let orphans = orphaned_files(&state)?;
    let mut removed = 0usize;
    for path in orphans {
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) => crate::logln!("[data] could not delete {}: {e}", path.display()),
        }
    }
    crate::logln!("[data] deleted {removed} orphaned recordings");
    Ok(removed)
}

/// Audio for one note, base64 for the webview to play.
///
/// Capped rather than streamed: an hour of audio is ~115 MB, and pushing that
/// through IPC to build a Blob would stall the UI. Long recordings are better
/// opened in a real player, so the cap fails with a message that says so.
const MAX_INLINE_AUDIO: u64 = 25 * 1024 * 1024;

#[tauri::command]
pub fn note_audio(id: i64, state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    use base64::Engine;

    let note = state.store.note(id).ok_or("that note no longer exists")?;
    let path = note.audio_path.ok_or("this note has no saved audio")?;

    let meta = std::fs::metadata(&path)
        .map_err(|_| format!("the recording is missing from disk ({path})"))?;
    if meta.len() > MAX_INLINE_AUDIO {
        return Err(format!(
            "this recording is {:.0} MB, too large to play in the app -- open              the recordings folder and play it there",
            meta.len() as f64 / 1024.0 / 1024.0
        ));
    }

    let bytes = std::fs::read(&path).map_err(|e| format!("could not read the recording: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Tail of the log file, for the in-app diagnostics view.
#[tauri::command]
pub fn read_log(lines: usize) -> CmdResult<String> {
    let path = crate::store::data_dir().map_err(fail)?.join("geminiflow.log");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read the log ({}): {e}", path.display()))?;

    let wanted = lines.clamp(50, 5000);
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(wanted);
    Ok(all[start..].join("
"))
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
