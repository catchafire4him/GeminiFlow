use serde::{Deserialize, Serialize};

use crate::store::Store;

const KEY: &str = "settings";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub hotkey: String,
    pub input_device: Option<String>,
    pub paste_mode: String,
    /// Defaults OFF. M0.5 measured live at 374ms and made it the primary path,
    /// but in real use its sessions intermittently acknowledge setup and then
    /// ignore audio entirely -- roughly a third of attempts. Batch is slower
    /// (~2-4.5s, scaling with clip length) and has not failed once. Predictable
    /// beats fast-but-unreliable for something used all day.
    pub use_live: bool,
    pub language: String,
    pub retention_days: i64,
    /// Appends a space after each dictation so consecutive ones do not run
    /// together ("...previous session.For example..."). Trailing rather than
    /// leading, so starting in an empty field does not begin with a space.
    #[serde(default = "default_true")]
    pub trailing_space: bool,

    /// Notes chord. A toggle, not hold-to-talk.
    #[serde(default = "default_notes_hotkey")]
    pub notes_hotkey: String,
    /// Hard cap on a notes recording. A toggle that is never toggled off would
    /// otherwise record until the app closes, then bill for all of it.
    #[serde(default = "default_note_max")]
    pub note_max_minutes: i64,
    /// Auto-stop after this long with no speech.
    #[serde(default = "default_note_silence")]
    pub note_silence_minutes: i64,
    /// Phone-call chord. Separate from notes because the write-up differs:
    /// a call transcript is only one side of a conversation.
    #[serde(default = "default_call_hotkey")]
    pub call_hotkey: String,
    /// On speakerphone the mic hears both parties, so the call becomes a real
    /// two-sided recording: diarisation on, two-sided template.
    #[serde(default)]
    pub speakerphone: bool,
    /// Keeps the last N seconds of microphone audio in memory so starting a
    /// recording part-way into a call still captures the opening. Off by
    /// default: it means the mic is open whenever the app is idle.
    #[serde(default)]
    pub prebuffer_enabled: bool,
    #[serde(default = "default_prebuffer_seconds")]
    pub prebuffer_seconds: i64,
    /// Short tones when a recording starts and stops. On by default: without
    /// them there is no confirmation a hold-to-talk key registered.
    #[serde(default = "default_true")]
    pub sounds_enabled: bool,
    /// 0-100. 50 is the level the tones were tuned at.
    #[serde(default = "default_sound_volume")]
    pub sound_volume: i64,
    /// A floating button that can be held to dictate, for touchscreens.
    ///
    /// Off by default: it is a permanent thing on the screen, and on a
    /// machine without a touchscreen it is only in the way.
    #[serde(default)]
    pub touch_button: bool,
    /// Where it was last left, in logical screen pixels. Clamped back
    /// into view at startup, since the screen it was saved on may be gone.
    #[serde(default)]
    pub touch_x: f64,
    #[serde(default)]
    pub touch_y: f64,
    /// Whether a position has ever been saved. Zero is a legitimate
    /// coordinate, so it cannot stand in for "never placed".
    #[serde(default)]
    pub touch_placed: bool,
    /// Diameter in logical pixels. Thumb-sized by default.
    #[serde(default = "default_touch_size")]
    pub touch_size: f64,
    /// Expose a local HTTP endpoint so external hardware can drive the app.
    ///
    /// Off by default. It opens a listening socket -- loopback only and
    /// token authenticated, but this app's posture for anything that opens
    /// a surface is that you turn it on deliberately.
    #[serde(default)]
    pub control_enabled: bool,
    /// Preferred port. If it is taken, the OS picks one and the real port
    /// is written to control.json.
    #[serde(default = "default_control_port")]
    pub control_port: i64,
    /// Keep the recording behind each dictation.
    ///
    /// Off by default. This is the only way to tell a model that misheard
    /// from audio that was never intelligible, but it also means every word
    /// dictated is sitting on disk as a WAV, so it is opt-in and pruned.
    #[serde(default)]
    pub keep_dictation_audio: bool,
    /// How long retained dictation audio survives.
    #[serde(default = "default_audio_days")]
    pub dictation_audio_days: i64,
    /// Start with Windows and go straight to the tray.
    #[serde(default)]
    pub launch_at_login: bool,
    /// Hide the window on a normal launch too, not just a login launch.
    #[serde(default)]
    pub start_hidden: bool,
    /// Verbose diagnostics: per-5s hotkey counters and raw WebSocket frames.
    /// Invaluable while debugging, pure noise in daily use.
    #[serde(default)]
    pub debug_logging: bool,
    /// Model used to turn a note transcript into a summary and action items.
    /// Configurable because these models go through periods of high demand and
    /// start refusing requests -- switching is faster than waiting.
    #[serde(default = "default_notes_model")]
    pub notes_model: String,
}

fn default_true() -> bool {
    true
}

fn default_notes_hotkey() -> String {
    "CtrlShiftSemicolon".to_string()
}

fn default_note_max() -> i64 {
    60
}

fn default_note_silence() -> i64 {
    2
}

/// 88 logical pixels. Comfortably past the ~44 that guidance calls the
/// minimum touch target, without being a landmark on the screen.
fn default_touch_size() -> f64 {
    88.0
}

fn default_control_port() -> i64 {
    8787
}

fn default_audio_days() -> i64 {
    7
}

fn default_sound_volume() -> i64 {
    50
}

fn default_call_hotkey() -> String {
    "CtrlShiftQuote".to_string()
}

fn default_prebuffer_seconds() -> i64 {
    30
}

fn default_notes_model() -> String {
    "gemini-3.5-flash-lite".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hotkey: "RightCtrl".to_string(),
            input_device: None,
            paste_mode: "ctrlV".to_string(),
            use_live: false,
            language: "en-US".to_string(),
            retention_days: 30,
            trailing_space: true,
            notes_hotkey: default_notes_hotkey(),
            note_max_minutes: default_note_max(),
            note_silence_minutes: default_note_silence(),
            call_hotkey: default_call_hotkey(),
            speakerphone: false,
            prebuffer_enabled: false,
            prebuffer_seconds: default_prebuffer_seconds(),
            notes_model: default_notes_model(),
            touch_button: false,
            touch_x: 0.0,
            touch_y: 0.0,
            touch_placed: false,
            touch_size: default_touch_size(),
            control_enabled: false,
            control_port: default_control_port(),
            keep_dictation_audio: false,
            dictation_audio_days: default_audio_days(),
            sounds_enabled: true,
            sound_volume: default_sound_volume(),
            launch_at_login: false,
            start_hidden: false,
            debug_logging: false,
        }
    }
}

impl Settings {
    pub fn load(store: &Store) -> Settings {
        store
            .get_setting(KEY)
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, store: &Store) -> anyhow::Result<()> {
        store.set_setting(KEY, &serde_json::to_string(self)?)?;
        Ok(())
    }
}
