# GeminiFlow

A Windows tray app for talking instead of typing, built on the Gemini API.

Three hotkey-driven modes:

| Mode | Key | What it does |
|---|---|---|
| **Dictation** | hold `Right Ctrl` | Speak, release, and the text is inserted into whatever field has focus — in any app |
| **Notes** | `Ctrl+Shift+;` | Toggle on, think out loud, toggle off; becomes a note with a summary and action-item checklist |
| **Phone calls** | `Ctrl+Shift+'` | Toggle around a call; written up knowing the mic only heard your half |

All hotkeys are rebindable in Settings.

## Running it

```bash
cd app
npm install
npm run app          # development
npx tauri build      # installer -> src-tauri/target/release/bundle/nsis/
```

Requires Rust (MSVC toolchain) and Node. On first launch, paste a Gemini API
key into Settings; it is stored in Windows Credential Manager, never on disk.

## Layout

```
app/                    the application
  src/                  React UI - Home, Notes, Dictation, Settings, overlay
  src-tauri/src/
    hotkey.rs           WH_KEYBOARD_LL hook; both key edges, swallows the key
    audio.rs            capture, 16 kHz resampling, rolling pre-buffer
    inject.rs           focus restore, clipboard, paste synthesis
    gemini/             batch + live transcription, note structuring
    engine.rs           session state machine and worker queues
    store.rs            SQLite in %APPDATA%/GeminiFlow
m0-spike/               throwaway latency spike that preceded the app
PLAN.md                 design decisions and what measurement produced them
```

## Read PLAN.md first

`PLAN.md` is not a plan that was written once and abandoned — it records what
was measured and which assumptions those measurements overturned. Several
things in this codebase look wrong until you know why they are that way:

- **Connection pooling is disabled** on every API client. Google serves these
  endpoints over HTTP/2, and a degraded pooled connection stalls the *next*
  request instead of failing. This cost 20-26 second dictations and, earlier, a
  60-second hang.
- **Batch transcription is the default, not live streaming**, despite live
  measuring 6x faster. Live sessions intermittently accept audio and return
  nothing.
- **The Live API replies with binary WebSocket frames**, not text frames. A
  client handling only `Message::Text` silently discards everything including
  `setupComplete`.
- **The keyboard hook is not called while the app's own window has focus**, so
  the UI forwards those keystrokes to the same engine.
- **Notes are saved before they are summarised**, and summarising runs on its
  own queue. Doing it the other way round made a slow summariser look like a
  lost note, and once delayed the next note by 112 seconds.

## Data

Everything lives in `%APPDATA%\GeminiFlow` — SQLite database plus note audio.
Uninstalling does not remove it. Settings has a Data section showing what is
stored and how to clear it.

Personal-use scope: no installer signing, no auto-update, no telemetry.
