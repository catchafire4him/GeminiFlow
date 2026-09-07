//! Session state machine: Idle -> Arming -> Recording -> Finalizing ->
//! Injecting -> Idle, with Error reachable from anywhere and always returning
//! to Idle.
//!
//! Runs on its own thread. Nothing here may block the keyboard hook thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::gemini::{self, batch::BatchClient, live, notes::NotesClient};
use crate::settings::Settings;
use crate::store::Store;
use crate::{audio, hotkey, inject, overlay, secrets};

pub const STATUS_EVENT: &str = "engine://status";
pub const DICTATION_EVENT: &str = "engine://dictation";
pub const LEVEL_EVENT: &str = "engine://level";
pub const NOTE_EVENT: &str = "engine://note";
pub const NOTE_SUMMARY_EVENT: &str = "engine://note-summary";

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Idle,
    Arming,
    Recording,
    Finalizing,
    Injecting,
    Error,
    /// Notes capture is running. Distinct from Recording so the overlay can
    /// make it unmistakable -- a note left running by accident is the failure
    /// mode this state exists to prevent.
    NoteRecording,
    NoteProcessing,
    /// A phone call. Separate from NoteRecording purely so the overlay can
    /// look different -- confusing a call with a note while one is running is
    /// exactly the mistake worth designing out.
    CallRecording,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: State,
    pub detail: Option<String>,
    pub partial: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            state: State::Idle,
            detail: None,
            partial: None,
        }
    }
}

pub struct AppState {
    pub store: Arc<Store>,
    pub settings: Mutex<Settings>,
    pub status: Mutex<Status>,
}

impl AppState {
    pub fn new(store: Arc<Store>, settings: Settings) -> AppState {
        AppState {
            store,
            settings: Mutex::new(settings),
            status: Mutex::new(Status::default()),
        }
    }
}

/// The shared state, for deferred work that outlives its caller.
static APP_STATE: std::sync::OnceLock<Arc<AppState>> = std::sync::OnceLock::new();

/// Counts published statuses.
///
/// A timer that wants to change the state later has to know whether
/// anything happened in the meantime. Comparing the status itself would
/// not do -- two identical errors in a row are still two events.
static STATUS_SEQ: AtomicU64 = AtomicU64::new(0);

fn publish(app: &AppHandle, state: &AppState, status: Status) {
    STATUS_SEQ.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut current) = state.status.lock() {
        *current = status.clone();
    }

    // The overlay is the only feedback during a 2-4.5s batch transcription, so
    // its visibility tracks state rather than being toggled by hand.
    match status.state {
        State::Idle => overlay::hide_after(app, Duration::from_millis(900)),
        State::Error => overlay::hide_after(app, Duration::from_secs(5)),
        _ => overlay::show(app),
    }

    // Anything subscribed to the control surface sees the same state the
    // UI does, from the same place, so the two cannot drift.
    if let Ok(payload) = serde_json::to_string(&status) {
        crate::control::broadcast(&payload);
    }

    let _ = app.emit(STATUS_EVENT, status);
}

/// True while a dictation is mid-flight. Dictation is the foreground activity:
/// the user is waiting on text to appear, so a note finishing in the
/// background must not take over the overlay.
fn dictation_in_flight(state: &AppState) -> bool {
    state
        .status
        .lock()
        .map(|s| {
            matches!(
                s.state,
                State::Arming | State::Recording | State::Finalizing | State::Injecting
            )
        })
        .unwrap_or(false)
}

/// Publishes only if nothing more urgent is happening.
fn publish_background(app: &AppHandle, state: &AppState, status: Status) {
    if dictation_in_flight(state) {
        return;
    }
    publish(app, state, status);
}

fn set_state(app: &AppHandle, state: &AppState, s: State) {
    publish(
        app,
        state,
        Status {
            state: s,
            detail: None,
            partial: None,
        },
    );
}

/// How long a failure stays on show before the app calls itself ready again.
///
/// Matches how long the overlay lingers, so the pill and anything else
/// watching agree about when the failure stopped being current.
const ERROR_LINGER: Duration = Duration::from_secs(5);

fn set_error(app: &AppHandle, state: &AppState, message: impl Into<String>) {
    let message = message.into();
    crate::logln!("engine error: {message}");
    publish(
        app,
        state,
        Status {
            state: State::Error,
            detail: Some(message),
            partial: None,
        },
    );

    // Then return to idle on its own.
    //
    // The overlay used to hide itself while the state stayed Error forever.
    // That was invisible until something else started watching: a Stream Deck
    // button sat on "failed" indefinitely after a trivial problem, because
    // that genuinely was still the app state. Hiding a stale state is not the
    // same as clearing it.
    let seq = STATUS_SEQ.load(Ordering::SeqCst);
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(ERROR_LINGER);
        // Anything published since means this failure is no longer what is
        // happening, and whoever published last owns the state now.
        if STATUS_SEQ.load(Ordering::SeqCst) != seq {
            return;
        }
        if let Some(state) = APP_STATE.get() {
            publish(&app, state, Status::default());
        }
    });
}

/// Perceptual-ish level for the meter: RMS, then a curve that makes normal
/// speech occupy most of the bar instead of hugging the bottom.
fn rms(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|&s| {
        let v = s as f64 / i16::MAX as f64;
        v * v
    }).sum();
    let rms = (sum / pcm.len() as f64).sqrt() as f32;
    (rms * 4.0).powf(0.6).clamp(0.0, 1.0)
}

fn describe_mods(mods: u32) -> String {
    let mut parts = Vec::new();
    if mods & 1 != 0 {
        parts.push("Ctrl");
    }
    if mods & 2 != 0 {
        parts.push("Shift");
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("+")
    }
}

pub fn spawn(app: AppHandle, state: Arc<AppState>, events: Receiver<hotkey::Event>) {
    // Kept so a timer can publish a status change after the call that
    // started it has returned. Set once, at startup.
    let _ = APP_STATE.set(Arc::clone(&state));

    let (dictation_tx, dictation_rx) = std::sync::mpsc::channel::<DictationJob>();
    let (note_tx, note_rx) = std::sync::mpsc::channel::<NoteJob>();

    // Dictation worker. Serial on purpose: two dictations processed in
    // parallel could paste out of order, which scrambles the text.
    {
        let app = app.clone();
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let batch = match BatchClient::new() {
                Ok(client) => client,
                Err(e) => {
                    set_error(&app, &state, format!("could not start HTTP client: {e}"));
                    return;
                }
            };
            for job in dictation_rx {
                if let Err(e) = process_dictation(&app, &state, &batch, job) {
                    set_error(&app, &state, e.to_string());
                }
            }
        });
    }

    let (summary_tx, summary_rx) = std::sync::mpsc::channel::<SummaryJob>();

    // Notes worker: record -> transcribe -> save. Never summarises, so a slow
    // or failing summariser cannot delay the next note reaching disk.
    {
        let app = app.clone();
        let state = Arc::clone(&state);
        let summary_tx = summary_tx.clone();
        std::thread::spawn(move || {
            let batch = match BatchClient::new() {
                Ok(batch) => batch,
                Err(e) => {
                    set_error(&app, &state, format!("could not start notes client: {e}"));
                    return;
                }
            };
            for job in note_rx {
                if let Err(e) = process_note(&app, &state, &batch, &summary_tx, job) {
                    set_error(&app, &state, e.to_string());
                }
            }
        });
    }

    // Summariser, on its own queue. Retries here are slow by design and only
    // ever hold up other summaries.
    {
        let app = app.clone();
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let client = match NotesClient::new() {
                Ok(client) => client,
                Err(e) => {
                    set_error(&app, &state, format!("could not start summariser: {e}"));
                    return;
                }
            };
            for job in summary_rx {
                summarise(&app, &state, &client, job);
            }
        });
    }

    std::thread::spawn(move || {
        let recorder = audio::Recorder::spawn();
        let prebuffer = audio::PreBuffer::spawn();

        // Arm at startup if enabled, so a call that starts before the user
        // touches the app still has its opening.
        arm_prebuffer_if_enabled(&state, &prebuffer);

        crate::logln!("[engine] ready");

        let mut session: Option<Session> = None;
        let mut notes_session: Option<NoteSession> = None;
        // Remembered so dictating while our own window is focused still lands
        // where the user was actually working.
        let mut last_external: Option<TargetWindow> = None;

        for event in events {
            match event {
                hotkey::Event::Press => {
                    if session.is_some() {
                        crate::logln!("[engine] press ignored: a session is already open");
                        continue;
                    }
                    // One microphone, one recorder. Without this guard the
                    // release would stop the *note* recording and hand its
                    // audio to the dictation path, ruining both.
                    if notes_session.is_some() {
                        crate::logln!("[engine] press ignored: a note is recording");
                        set_error(
                            &app,
                            &state,
                            "a note is recording -- stop it before dictating",
                        );
                        continue;
                    }
                    crate::logln!("[engine] press");
                    match begin(&app, &state, &recorder, last_external) {
                        Ok(started) => {
                            last_external = Some(started.target);
                            session = Some(started);
                        }
                        Err(e) => set_error(&app, &state, e.to_string()),
                    }
                }
                hotkey::Event::NotesKeyMissedModifiers(held) => {
                    crate::logln!(
                        "[hotkey] notes key seen but modifiers were {} -- note that \
                         the dictation key is swallowed, so if it is Right Ctrl it \
                         cannot also serve as the Ctrl in this chord; use Left Ctrl",
                        describe_mods(held)
                    );
                }

                hotkey::Event::CallToggle => {
                    if session.is_some() {
                        crate::logln!("[engine] call toggle ignored: dictation in progress");
                        continue;
                    }
                    match notes_session.take() {
                        Some(started) => {
                            crate::logln!("[engine] {} stop", started.kind);
                            match stop_note(&app, &state, &recorder, &prebuffer, started) {
                                Ok(Some(job)) => {
                                    let _ = note_tx.send(job);
                                }
                                Ok(None) => {}
                                Err(e) => set_error(&app, &state, e.to_string()),
                            }
                        }
                        None => {
                            crate::logln!("[engine] call start");
                            match begin_note(&app, &state, &recorder, &prebuffer, "call") {
                                Ok(started) => notes_session = Some(started),
                                Err(e) => set_error(&app, &state, e.to_string()),
                            }
                        }
                    }
                }

                hotkey::Event::NotesToggle => {
                    if session.is_some() {
                        crate::logln!("[engine] notes toggle ignored: dictation in progress");
                        continue;
                    }
                    match notes_session.take() {
                        Some(started) => {
                            crate::logln!("[engine] {} stop", started.kind);
                            match stop_note(&app, &state, &recorder, &prebuffer, started) {
                                Ok(Some(job)) => {
                                    let _ = note_tx.send(job);
                                }
                                Ok(None) => {}
                                Err(e) => set_error(&app, &state, e.to_string()),
                            }
                        }
                        None => {
                            crate::logln!("[engine] notes start");
                            match begin_note(&app, &state, &recorder, &prebuffer, "note") {
                                Ok(started) => notes_session = Some(started),
                                Err(e) => set_error(&app, &state, e.to_string()),
                            }
                        }
                    }
                }

                hotkey::Event::Release => {
                    let Some(started) = session.take() else {
                        crate::logln!("[engine] release ignored: no session was open");
                        continue;
                    };
                    crate::logln!("[engine] release");

                    // Stop partials from the session being torn down; a late
                    // one would otherwise stomp the next session's status.
                    started.active.store(false, Ordering::SeqCst);

                    match stop_dictation(&app, &state, &recorder, started) {
                        Ok(Some(job)) => {
                            let _ = dictation_tx.send(job);
                        }
                        Ok(None) => {}
                        Err(e) => set_error(&app, &state, e.to_string()),
                    }
                }
            }
        }
        crate::logln!("[engine] event channel closed -- the keyboard hook is gone");
    });
}

/// A window handle that can cross threads.
///
/// `HWND` wraps a raw pointer and so is not `Send`, but the value is just an
/// opaque kernel handle -- carrying it as an integer and rebuilding it on the
/// worker is safe and lets processing move off the event loop.
#[derive(Clone, Copy)]
pub struct TargetWindow(isize);

impl TargetWindow {
    fn new(hwnd: windows::Win32::Foundation::HWND) -> Self {
        TargetWindow(hwnd.0 as isize)
    }
    fn hwnd(self) -> windows::Win32::Foundation::HWND {
        windows::Win32::Foundation::HWND(self.0 as *mut std::ffi::c_void)
    }
}

/// Captured audio handed to a worker. The event loop must never do network
/// work: a note takes two API calls and can run for ten seconds, and blocking
/// there makes every other shortcut dead in the meantime.
struct DictationJob {
    session: Session,
    samples: Vec<f32>,
    released: Instant,
    /// Measured on the event loop, where the samples are already in hand.
    peak: f32,
}

struct NoteJob {
    session: NoteSession,
    samples: Vec<f32>,
}

/// Summarising runs on its own queue.
///
/// Measured: a note whose summary was retrying for two minutes held up the
/// *next* note's transcription for 112 seconds, because both shared one
/// worker. Saving a recording must never wait on someone else's summary.
struct SummaryJob {
    note_id: i64,
    transcript: String,
    api_key: String,
    /// Chosen at capture time, so a call is never written up with the
    /// note template even if the setting changes afterwards.
    template: &'static str,
}

struct Session {
    target: TargetWindow,
    live: Option<live::LiveSession>,
    settings: Settings,
    vocabulary: Vec<String>,
    api_key: String,
    /// Cleared on release so late partials from this session cannot overwrite
    /// the status of whatever happens next.
    active: Arc<AtomicBool>,
}

struct NoteSession {
    settings: Settings,
    api_key: String,
    started: Instant,
    /// "note" or "call". Decides which structuring template is used and how
    /// the recording is labelled.
    kind: &'static str,
    /// Audio recovered from the rolling pre-buffer, prepended to the take.
    prebuffered: Vec<f32>,
    /// Cleared when the session ends, so the watchdog stops.
    active: Arc<AtomicBool>,
}

/// Starts a notes recording. Unlike dictation this is a toggle, so it needs a
/// watchdog: a note left running by accident would otherwise record until the
/// app is closed and then bill for the whole thing.
/// Arms the rolling pre-buffer when the setting is on. Explicitly opt-in:
/// it holds an open microphone while the app is idle.
fn arm_prebuffer_if_enabled(state: &Arc<AppState>, prebuffer: &audio::PreBuffer) {
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or_default();
    if settings.prebuffer_enabled {
        prebuffer.arm(
            settings.input_device.clone(),
            settings.prebuffer_seconds.clamp(5, 120) as usize,
        );
    } else {
        prebuffer.disarm();
    }
}

fn begin_note(
    app: &AppHandle,
    state: &Arc<AppState>,
    recorder: &audio::Recorder,
    prebuffer: &audio::PreBuffer,
    kind: &'static str,
) -> Result<NoteSession> {
    let Some(api_key) = secrets::get_api_key() else {
        return Err(anyhow!("no API key set -- add one in Settings"));
    };
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or_default();

    let active = Arc::new(AtomicBool::new(true));
    let quiet_since: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    // Taking the snapshot also releases the microphone, so this must happen
    // before the recorder opens its own stream.
    let prebuffered = if settings.prebuffer_enabled {
        let snap = prebuffer.snapshot();
        if !snap.is_empty() {
            crate::logln!(
                "[engine] prepending {:.1}s from the pre-buffer",
                snap.len() as f32 / audio::TARGET_RATE as f32
            );
        }
        snap
    } else {
        Vec::new()
    };

    // Level tap doubles as the silence detector.
    let (level_tx, level_rx) = std::sync::mpsc::channel::<f32>();
    {
        let app = app.clone();
        std::thread::spawn(move || {
            for level in level_rx {
                let _ = app.emit(LEVEL_EVENT, level);
            }
        });
    }

    let tap: audio::AudioTap = {
        let quiet_since = Arc::clone(&quiet_since);
        Arc::new(move |pcm| {
            let level = rms(&pcm);
            let _ = level_tx.send(level);

            if let Ok(mut quiet) = quiet_since.lock() {
                if level < 0.02 {
                    quiet.get_or_insert_with(Instant::now);
                } else {
                    *quiet = None;
                }
            }
        })
    };

    crate::sound::play(crate::sound::Tone::Start);
    recorder.start(settings.input_device.clone(), Some(tap));
    set_state(
        app,
        state,
        if kind == "call" {
            State::CallRecording
        } else {
            State::NoteRecording
        },
    );

    spawn_note_watchdog(
        app.clone(),
        Arc::clone(&active),
        quiet_since,
        settings.note_max_minutes,
        settings.note_silence_minutes,
    );

    Ok(NoteSession {
        settings,
        api_key,
        started: Instant::now(),
        kind,
        prebuffered,
        active,
    })
}

/// Stops the note by synthesising a toggle event, so auto-stop and a manual
/// stop take exactly the same path.
fn spawn_note_watchdog(
    app: AppHandle,
    active: Arc<AtomicBool>,
    quiet_since: Arc<Mutex<Option<Instant>>>,
    max_minutes: i64,
    silence_minutes: i64,
) {
    std::thread::spawn(move || {
        let started = Instant::now();
        let max = Duration::from_secs((max_minutes.max(1) * 60) as u64);
        let silence = Duration::from_secs((silence_minutes.max(1) * 60) as u64);

        while active.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(2));
            if !active.load(Ordering::SeqCst) {
                return;
            }

            let too_long = started.elapsed() >= max;
            let too_quiet = quiet_since
                .lock()
                .ok()
                .and_then(|q| *q)
                .map(|since| since.elapsed() >= silence)
                .unwrap_or(false);

            if too_long || too_quiet {
                crate::logln!(
                    "[engine] notes auto-stop ({})",
                    if too_long { "max duration" } else { "silence" }
                );
                let _ = app.emit(
                    STATUS_EVENT,
                    Status {
                        state: State::NoteProcessing,
                        detail: Some(
                            if too_long {
                                "Stopped at the time limit"
                            } else {
                                "Stopped after a long silence"
                            }
                            .into(),
                        ),
                        partial: None,
                    },
                );
                crate::hotkey::request_notes_stop();
                return;
            }
        }
    });
}

/// Event-loop half: stop the recorder and hand the audio off. Must not touch
/// the network, or every other shortcut is dead while a note is written up.
fn stop_note(
    app: &AppHandle,
    state: &Arc<AppState>,
    recorder: &audio::Recorder,
    prebuffer: &audio::PreBuffer,
    session: NoteSession,
) -> Result<Option<NoteJob>> {
    session.active.store(false, Ordering::SeqCst);
    crate::sound::play(crate::sound::Tone::Stop);
    publish_background(
        app,
        state,
        Status { state: State::NoteProcessing, detail: None, partial: None },
    );

    let recorded = recorder.stop()?;

    // The microphone is free again, so the rolling window can resume.
    arm_prebuffer_if_enabled(state, prebuffer);

    // Pre-buffered audio goes in front, so the take starts before the keypress.
    let mut samples = session.prebuffered.clone();
    samples.extend_from_slice(&recorded);

    let seconds = samples.len() as f32 / audio::TARGET_RATE as f32;
    let peak = audio::peak(&samples);
    crate::logln!(
        "[engine] {} captured {seconds:.1}s, peak {peak:.4} ({:.1}s pre-buffered)",
        session.kind,
        session.prebuffered.len() as f32 / audio::TARGET_RATE as f32
    );

    if seconds < 1.0 {
        set_error(app, state, "that recording was too short to save");
        return Ok(None);
    }

    // Dictation has always checked this; notes and calls did not, so a muted
    // microphone produced a mystifying "no speech was recognised" after a
    // full transcription round-trip instead of an immediate, accurate error.
    if peak < 0.005 {
        log_microphone_state(session.settings.input_device.as_deref());
        set_error(
            app,
            state,
            "the microphone captured silence for that recording -- check it is              not muted and is the right device in Settings",
        );
        return Ok(None);
    }

    Ok(Some(NoteJob { session, samples }))
}

/// Worker half: transcribe, structure, persist. Runs on the notes thread.
fn process_note(
    app: &AppHandle,
    state: &Arc<AppState>,
    batch: &BatchClient,
    summary_tx: &std::sync::mpsc::Sender<SummaryJob>,
    job: NoteJob,
) -> Result<()> {
    let NoteJob { session, mut samples } = job;

    if let Some(gain) = audio::normalize(&mut samples) {
        crate::logln!("[engine] quiet recording, applied {gain:.1}x gain");
    }

    let wav = audio::to_wav(&samples)?;

    // Audio is kept: notes are worth replaying, and re-running an old
    // recording against an improved prompt is on the roadmap.
    let audio_path = save_note_audio(&wav, session.kind).map_err(|e| {
        crate::logln!("[engine] could not save note audio: {e}");
        e
    });

    // Speakerphone means both parties are audible, so speaker labels are
    // worth the reduced audio ceiling. A one-sided call has one voice and
    // gains nothing from diarisation.
    let diarize = session.kind == "call" && session.settings.speakerphone;

    let transcript = batch.transcribe(
        &session.api_key,
        gemini::BATCH_MODEL,
        &wav,
        &state.store.vocabulary(),
        &session.settings.language,
        diarize,
    )?;

    if transcript.trim().is_empty() {
        set_error(
            app,
            state,
            format!("no speech was recognised in that {}", session.kind),
        );
        return Ok(());
    }
    crate::logln!("[engine] note transcript {} chars", transcript.trim().len());

    // Save first, summarise second.
    //
    // Summarising can take minutes when the model is under load, and doing it
    // before the insert means the note simply does not exist during that time
    // -- indistinguishable from having lost it. The transcript is the valuable
    // part and it is already in hand, so it goes to disk immediately and shows
    // up in the UI; the summary fills in afterwards.
    let note = state.store.insert_note(
        &fallback_draft(&transcript),
        &transcript,
        audio_path.as_deref().ok(),
        session.started.elapsed().as_millis() as i64,
        true,
        session.kind,
        session.settings.speakerphone,
        (session.prebuffered.len() as f32 / audio::TARGET_RATE as f32 * 1000.0) as i64,
    )?;
    let _ = app.emit(NOTE_EVENT, &note);
    let _ = app.emit(NOTE_SUMMARY_EVENT, SummaryStatus { id: note.id, state: "summarising" });
    crate::logln!("[engine] note {} saved; queued for summarising", note.id);
    publish_background(app, state, Status::default());

    // Handed to the summariser queue so this worker is free for the next note.
    let template = match (session.kind, session.settings.speakerphone) {
        ("call", true) => crate::gemini::notes::SPEAKERPHONE_TEMPLATE,
        ("call", false) => crate::gemini::notes::CALL_TEMPLATE,
        _ => crate::gemini::notes::DEFAULT_TEMPLATE,
    };

    let _ = summary_tx.send(SummaryJob {
        note_id: note.id,
        transcript,
        api_key: session.api_key,
        template,
    });

    Ok(())
}

#[derive(Clone, Serialize)]
struct SummaryStatus {
    id: i64,
    state: &'static str,
}

fn summarise(app: &AppHandle, state: &Arc<AppState>, client: &NotesClient, job: SummaryJob) {
    // Read per job so changing the model in Settings takes effect immediately,
    // including for notes already waiting in this queue.
    let model = state
        .settings
        .lock()
        .map(|s| s.notes_model.clone())
        .unwrap_or_else(|_| gemini::NOTES_MODEL.to_string());

    match client.structure(&job.api_key, &model, job.template, &job.transcript) {
        Ok(draft) => {
            crate::logln!(
                "[engine] note {} summarised: {} takeaways, {} action items",
                job.note_id,
                draft.takeaways.len(),
                draft.action_items.len()
            );
            if let Err(e) = state.store.apply_summary(job.note_id, &draft) {
                crate::logln!("[engine] could not store summary: {e}");
                return;
            }
            if let Some(updated) = state.store.note(job.note_id) {
                let _ = app.emit(NOTE_EVENT, &updated);
            }
        }
        Err(e) => {
            crate::logln!("[engine] summarising note {} failed: {e}", job.note_id);
            // Not a modal error: the note itself is safe on disk, and the UI
            // shows a retry button on it.
            let _ = app.emit(
                NOTE_SUMMARY_EVENT,
                SummaryStatus {
                    id: job.note_id,
                    state: "failed",
                },
            );
        }
    }
}

/// Stand-in note for when structuring failed: the transcript is kept whole and
/// the title is its opening words, so the note is still findable in the list.
fn fallback_draft(transcript: &str) -> crate::store::NoteDraft {
    let title: String = transcript.split_whitespace().take(8).collect::<Vec<_>>().join(" ");
    crate::store::NoteDraft {
        title: if title.is_empty() {
            "Untitled note".to_string()
        } else {
            format!("{title}…")
        },
        summary: String::new(),
        takeaways: Vec::new(),
        action_items: Vec::new(),
        notable: String::new(),
        counterparty: String::new(),
        inferred: Vec::new(),
        open_questions: Vec::new(),
    }
}

/// Kept apart from note audio by name so the two are never confused, and so
/// the pruner can only ever delete its own.
fn save_dictation_audio(wav: &[u8]) -> Result<String> {
    let dir = crate::store::recordings_dir()?;
    let name = format!(
        "dictation-{}.wav",
        chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f")
    );
    let path = dir.join(name);
    std::fs::write(&path, wav)?;
    Ok(path.to_string_lossy().to_string())
}

fn save_note_audio(wav: &[u8], kind: &str) -> Result<String> {
    let dir = crate::store::recordings_dir()?;
    // Named by kind so an orphaned file can be restored as what it actually
    // was; everything used to be written as "note-" regardless.
    let name = format!("{kind}-{}.wav", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    let path = dir.join(name);
    std::fs::write(&path, wav)?;
    Ok(path.to_string_lossy().to_string())
}

fn begin(
    app: &AppHandle,
    state: &Arc<AppState>,
    recorder: &audio::Recorder,
    last_external: Option<TargetWindow>,
) -> Result<Session> {
    set_state(app, state, State::Arming);

    let Some(api_key) = secrets::get_api_key() else {
        return Err(anyhow!("no API key set -- add one in Settings"));
    };

    let settings = state
        .settings
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let vocabulary = state.store.vocabulary();
    let mut target = inject::foreground_window();

    // Pasting into our own window would put the text somewhere the user cannot
    // see it. Fall back to whatever they were working in last.
    if inject::is_own_window(target) {
        match last_external {
            Some(previous) => {
                crate::logln!(
                    "[engine] GeminiFlow itself is focused; falling back to {}",
                    inject::describe_window(previous.hwnd())
                );
                target = previous.hwnd();
            }
            None => {
                return Err(anyhow!(
                    "GeminiFlow is the focused window, so there is nowhere to put                      the text -- click into the app you want to dictate into first"
                ));
            }
        }
    }

    crate::logln!("[engine] target window: {}", inject::describe_window(target));

    let active = Arc::new(AtomicBool::new(true));

    // Partials are forwarded to the UI on a separate thread. Emitting inline
    // would run JSON serialisation, IPC and a mutex lock *inside* the
    // WebSocket loop, stalling audio sends and starving the server -- which
    // showed up as sessions that transcribed almost nothing and never
    // finalised.
    let (partial_tx, partial_rx) = std::sync::mpsc::channel::<String>();
    {
        let app = app.clone();
        let state = Arc::clone(state);
        let active = Arc::clone(&active);
        std::thread::spawn(move || {
            for partial in partial_rx {
                if !active.load(Ordering::SeqCst) {
                    continue;
                }
                publish(
                    &app,
                    &state,
                    Status {
                        state: State::Recording,
                        detail: None,
                        partial: Some(partial),
                    },
                );
            }
        });
    }

    // Streaming is the primary path. Starting it here means transcription
    // overlaps with speech, which is the whole reason dictation feels instant.
    let session = if settings.use_live {
        Some(live::start(
            api_key.clone(),
            gemini::LIVE_MODEL.to_string(),
            vocabulary.clone(),
            settings.language.clone(),
            // Must stay cheap: this runs on the WebSocket loop.
            move |partial| {
                let _ = partial_tx.send(partial);
            },
        ))
    } else {
        None
    };
    let live_sink = session.as_ref().map(|s| s.sender());

    // The tap is installed in both modes now: batch has no partials, so the
    // level meter is the only sign the overlay can give that we are hearing
    // anything at all.
    let (level_tx, level_rx) = std::sync::mpsc::channel::<f32>();
    {
        let app = app.clone();
        std::thread::spawn(move || {
            for level in level_rx {
                let _ = app.emit(LEVEL_EVENT, level);
            }
        });
    }

    let tap: audio::AudioTap = Arc::new(move |pcm| {
        let _ = level_tx.send(rms(&pcm));
        if let Some(sink) = &live_sink {
            let _ = sink.send(live::LiveMsg::Audio(pcm));
        }
    });

    crate::sound::play(crate::sound::Tone::Start);
    recorder.start(settings.input_device.clone(), Some(tap));
    set_state(app, state, State::Recording);

    Ok(Session {
        target: TargetWindow::new(target),
        live: session,
        settings,
        vocabulary,
        api_key,
        active,
    })
}

/// Reports the microphone's own level and mute state.
///
/// Called when a recording comes back empty. "Check it is not muted" is
/// advice; this is the answer. Windows keeps a wireless headset's
/// microphone listed while the headset is switched off, so a device that
/// looks present and records nothing is an ordinary situation rather than
/// a strange one.
fn log_microphone_state(device: Option<&str>) {
    match (crate::mic::volume(device), crate::mic::is_muted(device)) {
        (Ok(level), Ok(muted)) => crate::logln!(
            "[engine] microphone {:?} is at {level}% and {}",
            device.unwrap_or("(system default)"),
            if muted { "MUTED" } else { "not muted" }
        ),
        _ => crate::logln!(
            "[engine] could not read the state of microphone {:?} -- it may be \
             disconnected",
            device.unwrap_or("(system default)")
        ),
    }
}

/// Event-loop half: stop the recorder, sanity-check the audio, hand it off.
/// Deliberately does no network work -- blocking here freezes every shortcut.
fn stop_dictation(
    app: &AppHandle,
    state: &Arc<AppState>,
    recorder: &audio::Recorder,
    session: Session,
) -> Result<Option<DictationJob>> {
    let released = Instant::now();
    crate::sound::play(crate::sound::Tone::Stop);
    set_state(app, state, State::Finalizing);

    // Stopping the recorder flushes the tail of the utterance to the tap, so
    // it must happen before activityEnd.
    let samples = recorder.stop()?;
    let seconds = samples.len() as f32 / audio::TARGET_RATE as f32;
    let peak = audio::peak(&samples);
    crate::logln!("[engine] captured {seconds:.2}s, peak {peak:.4}");

    if seconds < 0.25 {
        // Previously a silent return, which looked identical to the app being
        // broken. Say so instead.
        set_error(
            app,
            state,
            format!("only {:.0} ms of audio was captured -- hold the key while you speak", seconds * 1000.0),
        );
        return Ok(None);
    }
    if peak < 0.005 {
        log_microphone_state(session.settings.input_device.as_deref());
        set_error(
            app,
            state,
            "the microphone captured silence -- check it is not muted and is \
             the right device in Settings",
        );
        return Ok(None);
    }

    Ok(Some(DictationJob {
        session,
        samples,
        released,
        peak,
    }))
}

/// Worker half: transcribe and inject. Runs on the dictation thread, serially,
/// so consecutive dictations still paste in the order they were spoken.
fn process_dictation(
    app: &AppHandle,
    state: &Arc<AppState>,
    batch: &BatchClient,
    job: DictationJob,
) -> Result<()> {
    let DictationJob {
        mut session,
        samples,
        released,
        peak,
    } = job;

    let mut text = String::new();
    let live_attempted = session.live.is_some();
    // Which path actually produced the text that gets pasted. Worth
    // recording because the three are not equivalent: only the finals and
    // the batch call are smart-formatted. Interim hypotheses are the raw
    // running guess, so a transcript that arrives that way keeps the filler
    // words smart mode would have removed.
    let mut source = "none";
    // Time spent obtaining the text, on whichever path won. Not the same as
    // the latency the user feels, which also covers injection.
    let mut transcribe_ms: i64 = 0;

    if let Some(live_session) = session.live.take() {
        match live_session.finish() {
            Ok(result) => {
                crate::logln!(
                    "[engine] live finalize {} ms{}",
                    result.finalize_ms,
                    if result.from_partial { " (from interim)" } else { "" }
                );
                source = if result.from_partial { "live-interim" } else { "live-final" };
                transcribe_ms = result.finalize_ms as i64;
                text = result.transcript;
            }
            Err(e) => crate::logln!("[engine] live failed: {e}"),
        }
    }

    // A live result can come back non-empty but absurdly short -- measured:
    // 11.2s of speech returning 5 characters of interim text, which then got
    // pasted. Empty is not the only failure. Ordinary speech runs 10-15
    // characters per second. Under ~8/s means we likely kept only a later
    // fragment (a 48s hold that pasted 303 chars was ~6/s) and batch should
    // redo it. Complete live takes here land at 10+.
    const MIN_LIVE_CHARS_PER_SEC: f32 = 8.0;
    let seconds = samples.len() as f32 / audio::TARGET_RATE as f32;
    let chars = text.trim().len() as f32;
    let density = if seconds > 0.0 { chars / seconds } else { 0.0 };
    let suspiciously_short = !text.trim().is_empty() && density < MIN_LIVE_CHARS_PER_SEC;

    if live_attempted && !text.trim().is_empty() {
        crate::logln!(
            "[engine] live density {density:.1} chars/s ({} chars, {seconds:.1}s, {source})",
            text.trim().len()
        );
    }

    if suspiciously_short {
        crate::logln!(
            "[engine] live returned {} chars for {seconds:.1}s of audio -- too little \
             to trust, falling back to batch",
            text.trim().len()
        );
        text.clear();
        source = "none";
    }

    // The audio is still in hand, so a live session that produced nothing costs
    // time, not the dictation. Batch is slower but it is the same audio and it
    // is reliable -- far better than telling the user their speech is gone.
    if text.trim().is_empty() {
        if live_attempted {
            crate::logln!("[engine] no live transcript; retrying with the batch model");
        } else {
            crate::logln!("[engine] transcribing with the batch model");
        }
        publish(
            app,
            state,
            Status {
                state: State::Finalizing,
                detail: Some(if live_attempted { "retrying…" } else { "transcribing…" }.into()),
                partial: None,
            },
        );

        let batch_started = Instant::now();
        let wav = audio::to_wav(&samples)?;
        text = batch.transcribe(
            &session.api_key,
            gemini::BATCH_MODEL,
            &wav,
            &session.vocabulary,
            &session.settings.language,
            false,
        )?;
        source = "batch";
        transcribe_ms = batch_started.elapsed().as_millis() as i64;
        crate::logln!(
            "[engine] batch produced {} chars in {} ms",
            text.trim().len(),
            batch_started.elapsed().as_millis()
        );
    }

    if text.trim().is_empty() {
        set_error(app, state, "no speech was recognised in that recording");
        return Ok(());
    }

    // One line per dictation summarising which path won, so a run of these
    // answers "is live actually being used" without reading the whole log.
    crate::logln!(
        "[engine] dictation via {source}: {} chars for {seconds:.1}s of audio",
        text.trim().len()
    );

    // The text itself, only under debug logging. Needed to answer questions
    // about transcription quality -- whether filler words survived, whether
    // a sentence ends mid-thought -- which a character count cannot. Off by
    // default because it puts everything you dictate in a plain-text file.
    if crate::logging::debug_enabled() {
        crate::logln!("[engine] text: {}", text.trim());
    }

    // Written after the text is in hand, so a failed transcription cannot
    // leave a file behind with no row pointing at it.
    let audio_path = if session.settings.keep_dictation_audio {
        match audio::to_wav(&samples).and_then(|wav| save_dictation_audio(&wav)) {
            Ok(path) => Some(path),
            Err(e) => {
                crate::logln!("[engine] could not save dictation audio: {e}");
                None
            }
        }
    } else {
        None
    };

    set_state(app, state, State::Injecting);
    let target_app = inject::window_process_name(session.target.hwnd());
    let paste_mode = inject::PasteMode::from_name(&session.settings.paste_mode);

    // Only what is typed gets the trailing space; history keeps the clean text.
    let to_inject = if session.settings.trailing_space {
        format!("{} ", text.trim_end())
    } else {
        text.clone()
    };

    let injected = inject::paste_into(session.target.hwnd(), &to_inject, paste_mode);
    let latency_ms = released.elapsed().as_millis() as i64;

    if let Err(e) = &injected {
        set_error(app, state, e.to_string());
    }

    // Recorded either way: a failed insert still leaves the text recoverable
    // from history, which matters more than the insert succeeding.
    let record = state.store.insert_dictation(
        &text,
        target_app.as_deref(),
        Some(latency_ms),
        injected.is_ok(),
        &crate::store::DictationDiagnostics {
            audio_seconds: seconds as f64,
            peak: peak as f64,
            source: source.to_string(),
            transcribe_ms,
            fell_back: live_attempted && source == "batch",
            audio_path,
        },
    )?;
    let _ = app.emit(DICTATION_EVENT, &record);

    if injected.is_ok() {
        set_state(app, state, State::Idle);
    }
    Ok(())
}
