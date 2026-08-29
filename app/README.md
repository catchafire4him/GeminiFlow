# GeminiFlow — M1

Tauri 2 desktop app. Hold a key anywhere in Windows, speak, release, and the
text lands in whatever field has focus.

## Run in development

```bash
cd app; npm run app
```

That starts Vite and the Rust backend together with hot reload on the frontend.
Rust changes need a restart.

On first launch: open **Settings**, paste a Gemini API key from Google AI
Studio, and it is written to Windows Credential Manager — never to disk in
plaintext. Saving the first key also seeds the custom vocabulary with a
starter list of programming terms.

## Build a distributable

```bash
cd app; npm run tauri build
```

Produces an NSIS installer under `src-tauri/target/release/bundle/`. It is
unsigned, so expect SmartScreen on first run — see the risks section in
`../PLAN.md`.

## What works in M1

- Hold **Right Ctrl** (rebindable) to dictate into any focused text field
- Live streaming transcription — ~410 ms from key release to visible text
- Batch fallback when streaming is turned off (~2.4 s, measurably worse)
- Custom vocabulary editor, up to the API's 1,000-term limit
- Dictation history with target app, latency, and copy/delete
- Microphone selection, paste-method choice, retention window
- Tray icon; closing the window hides rather than quits

## Not in M1

- The floating overlay (M2). The status pill in the header shows live partials
  for now, but only while the main window is open.
- Notes and phone-call modes (M3 / M3.5)
- System audio loopback (M5)

## Architecture

```
src/                     React UI
  routes/                Home, Dictation, Settings
  lib/api.ts             Typed wrappers over Tauri commands + events

src-tauri/src/
  hotkey.rs              WH_KEYBOARD_LL hook, both key edges, rebindable
  audio.rs               cpal capture -> 16 kHz mono, live tap
  inject.rs              Focus restore, clipboard, Shift+Insert
  gemini/live.rs         WebSocket streaming (primary path)
  gemini/batch.rs        REST /v1beta/interactions (fallback)
  engine.rs              Session state machine, emits UI events
  store.rs               SQLite in %APPDATA%/GeminiFlow
  secrets.rs             API key in Credential Manager
  commands.rs            Tauri command surface
```

## Things that will bite you

- **The dictation key is swallowed globally.** While GeminiFlow runs, Right
  Ctrl does not work as a modifier in any app. That is intended — it is a
  dedicated key — but it surprises people.
- **Injection into elevated windows fails** unless GeminiFlow is elevated too.
  The error message says so rather than silently dropping the text.
- **A keyboard hook plus SendInput plus mic access looks like a keylogger** to
  Defender. Expect a prompt.
- **The Live API replies with binary WebSocket frames, not text frames.** A
  client that handles only `Message::Text` silently discards everything
  including `setupComplete`, which looks exactly like an unresponsive server.
  This cost a debugging cycle in M0; see `gemini/live.rs`.
- **Never pre-warm the HTTP connection.** Google serves over HTTP/2, so a
  stalled warm-up request poisons the shared connection and the real request
  hangs until timeout.
