pub mod batch;
pub mod live;
pub mod notes;

pub const BATCH_MODEL: &str = "gemini-3.5-transcribe";
pub const LIVE_MODEL: &str = "gemini-3.5-transcribe-live";

/// Fallback structuring model, used only if the setting is somehow unreadable.
/// The configured default lives in `settings.rs`.
pub const NOTES_MODEL: &str = "gemini-3.7-flash";

/// Seed terms so a fresh install biases toward the words a coding dictation
/// tool mangles most. Editable in Settings.
pub fn starter_vocabulary() -> Vec<String> {
    [
        "Tauri", "WASAPI", "cpal", "useCallback", "useEffect", "useState",
        "TypeScript", "Rust", "cargo", "async", "await", "struct", "enum",
        "SQLite", "GeminiFlow", "Gemini", "webhook", "middleware", "npm",
        "JSON", "API", "CSS", "SVG", "CLI", "repo", "commit", "refactor",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
