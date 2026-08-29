# GeminiFlow — Implementation Plan

A lightweight Windows tray app with two hotkey-driven modes:

1. **Dictation** — hold a key, talk, release; your words are inserted into whatever text field has focus, in any app.
2. **Notes** — toggle a key, talk (or later, capture a meeting); the recording becomes a structured note with a summary, key takeaways, and an action-item checklist.

## Decisions (settled)

| Area | Decision |
|---|---|
| Stack | Tauri 2 (Rust core + React UI) |
| Dictation UX | Live streaming preview in an overlay, batch-quality text injected on release |
| Notes audio | Mic only for v1; capture layer abstracted so WASAPI loopback drops in later |
| Transcript handling | Raw transcript + custom vocabulary biasing. No second LLM pass for dictation. |
| Notes structuring | Second model call — the transcription model does not summarize |
| Storage | SQLite + loose audio files under `%APPDATA%/GeminiFlow` |
| Secrets | Gemini API key in Windows Credential Manager (`keyring` crate) |
| Data model | Flat. No projects/departments/channels. |

## Models

| Model | Used for | Notes |
|---|---|---|
| `gemini-3.5-transcribe` | Final transcript for both modes | <=1hr audio; <=30min with diarization or word timestamps; 85+ languages w/ code-switching; filler-word removal; **custom vocab bias up to 1,000 terms** |
| `gemini-3.5-transcribe-live` | Streaming overlay preview only | WebSocket, <=10 min/session, no word timestamps |
| `gemini-3.7-flash` | Notes structuring pass | Transcript to summary / takeaways / action items JSON |

### API shape (established)

Transcription goes through `POST https://generativelanguage.googleapis.com/v1beta/interactions` — **not** `generateContent`. Key travels in the `x-goog-api-key` header.

```jsonc
{
  "model": "gemini-3.5-transcribe",
  "input": [{ "type": "audio", "data": "<base64>", "mime_type": "audio/wav" }],
  "generation_config": {
    "transcription_config": {
      "language_codes": ["en-US"],
      "custom_vocabulary": ["Tauri", "WASAPI"],   // <= 1000 terms
      "mode": "smart"                              // or {"type":"verbatim", "diarization_mode":"speaker", "timestamp_granularities":["word"]}
    }
  }
}
```

Transcript lands at `interaction.output_text`. The docs demonstrate audio via a Files API URI, but **inline base64 works under 20MB** — which matters, because the Files API path would put a second HTTP round-trip in the dictation critical path. Notes and calls can use either; dictation must use inline.

**Verify in M0:** exact Live API audio frame format and sample rate, current per-minute pricing for both transcribe endpoints, and whether `gemini-3.7-flash` supports a response JSON schema (if not, the structuring pass needs prompt-level JSON coercion plus a parse-retry). Do not build cost estimates on assumptions.

## Architecture

```
src-tauri/src/
  main.rs           # tray, window wiring, single-instance guard
  state.rs          # session state machine
  hotkey/           # WH_KEYBOARD_LL hook, chord parsing, hold vs toggle
  audio/
    source.rs       # CaptureSource trait  <- loopback slots in here
    mic.rs          # cpal / WASAPI shared-mode mic
    ring.rs         # lock-free ring buffer
    encode.rs       # resample to 16k mono, encode for upload
  inject/
    clipboard.rs    # save / set / restore with retry
    sendinput.rs    # paste synthesis, modifier hygiene
    focus.rs        # capture + restore target HWND
  gemini/
    batch.rs        # REST transcribe, connection pre-warm
    live.rs         # WS streaming client (optional consumer)
    notes.rs        # structuring call, JSON schema
    vocab.rs        # term list -> request payload
  store/            # sqlite migrations, recording files, retention sweep

src/
  routes/Home.tsx       # recordings list + note detail
  routes/Dictation.tsx  # dictation history, vocabulary editor
  routes/Settings.tsx   # keys, hotkeys, devices, retention, usage
  overlay/              # separate non-activating window
```

### Session state machine

`Idle -> Arming -> Recording -> Finalizing -> Injecting -> Idle`, with `Error` reachable from any state and always returning to `Idle`.

`Arming` exists to do work during the ~100ms before the user starts speaking: capture the foreground `HWND`, open the audio stream, and **pre-warm the HTTPS connection** to `generativelanguage.googleapis.com` so the TLS handshake is not in the critical path after release.

## Feature 1 — Dictation

**Hotkey:** hold Right Ctrl. `tauri-plugin-global-shortcut` is not sufficient — it reports press, not release. Use a low-level keyboard hook (`SetWindowsHookEx` / `WH_KEYBOARD_LL`) via the `windows` crate to get both edges, and swallow the keydown so Right Ctrl does not leak into the focused app. Double-tap latches into hands-free mode for long dictations; tap again to stop.

**Flow:**

1. Keydown -> `Arming`. Snapshot foreground HWND. Start mic. Pre-warm connection. Show overlay.
2. Recording -> PCM into ring buffer. A copy feeds the live WS client (this consumer is switchable off in Settings). Overlay renders partials plus a level meter.
3. Keyup -> `Finalizing`. Stop capture, encode the full clip, POST to `gemini-3.5-transcribe` with the vocabulary bias list.
4. `Injecting` -> restore focus to the snapshotted HWND, place text on clipboard, synthesize paste, restore prior clipboard after a short delay.
5. Overlay fades. Transcript is logged to SQLite for the Dictation history view.

**Paste synthesis — the details that decide whether this actually works everywhere:**

- **Prefer `Shift+Insert` over `Ctrl+V`.** Ctrl+V is not paste in mintty/Git Bash and is historically unreliable in conhost; Shift+Insert works in Windows Terminal, mintty, VS Code, browsers, and Win32 apps alike. Keep Ctrl+V as a fallback and allow a per-executable override in Settings.
- **Clear stuck modifiers before injecting.** The user was physically holding Right Ctrl moments ago. Poll `GetAsyncKeyState` for Ctrl/Shift/Alt/Win and emit synthetic keyups for anything still down — a stuck Shift silently converts the paste into a different command.
- **Restore focus explicitly.** Focus can move between press and release. Reactivate the snapshotted HWND before pasting.
- **Restore the clipboard** ~200ms after paste, and only if the clipboard still holds our own text — otherwise the user copied something in between and we would clobber it.
- Clipboard opens fail when another process holds it. Retry with backoff; never panic.

**Custom vocabulary:** a user-editable term list in Settings, capped at 1,000 entries, sent with every batch request. This is where `useCallback`, `WASAPI`, `Tauri`, project names, and coworker names go. Seed it with a starter list of common programming terms.

### M0 measured results

M0 ran. The headline number invalidates the original dictation design.

| Stage | Assumed | **Measured** |
|---|---|---|
| `gemini-3.5-transcribe` batch call | 400–700 ms | **~2,400 ms** |

Supporting observations:

- A 31 KB payload took **3,049 ms**; a 185 KB payload took **2,395 ms**. Latency is not payload-driven — it is close to fixed model inference cost. Shrinking the audio further will not help.
- Both figures are cold-connection. A warm connection saves the TLS handshake, not seconds.
- Transcription quality was correct first try ("Testing 1 2 3. This is a test to see if this works.").

**Consequence — this reverses an earlier assumption.** The pre-M0 plan said that if batch was slow we should drop the batch call and let the live transcript stand, "which removes most of M2's justification." That reasoning was backwards. A ~2.4s post-release stall is unusable for dictation, and the only way to hide it is to transcribe *while the user is still speaking*. So:

- **The live streaming path is now the primary dictation path, not a preview.** M2 is promoted into the critical path and should be built immediately after M1, not treated as polish.
- **Batch is demoted to a fallback** for when the WS session fails, plus notes and calls, where a few seconds of processing is invisible.
- The overlay stops being sugar. If the live transcript is what gets injected, the user needs to see what was heard before it lands.
### M0.5 measured results — dictation is viable

The live path was measured on a 6.48s utterance:

| Stage | Measured |
|---|---|
| Stop capture, flush tail | 10 ms |
| **Live finalization after `audioStreamEnd`** | **374 ms** |
| Focus restore + paste | ~25 ms |
| **Total, key release to visible text** | **~410 ms** |

**6.4× faster than batch, and inside the 1s budget.** The design holds: live streaming as the primary dictation path, batch as fallback. Transcription was verbatim-accurate across a 19-word sentence including mid-sentence self-correction.

Two secondary findings that affect the overlay design:

- **First partial arrived 1,926 ms into speech**, with 11 partials over 6.5s. The overlay will sit empty for roughly the first two seconds of any utterance. It should show a listening/level indicator immediately and treat text as something that fills in later — a text-only overlay would look broken at the start of every dictation.
- **The clipboard restore must not be on the critical path.** Waiting 250ms before restoring the previous clipboard added that time to every dictation for work the user never sees. Restoring on a background thread cut perceived latency from 658 ms to ~410 ms. Same applies in the real app.

### M0 corrections to the API contract

Two things the docs got wrong, both now verified against live responses:

- **The transcript is at `steps[].content[].text`** (step `type: "model_output"`, content `type: "text"`), *not* `interaction.output_text`. Audio containing no speech returns **no `steps` key at all** rather than an empty transcript — so a missing `steps` means silence, not an error.
- **Do not pre-warm the connection with a throwaway request.** Google serves this endpoint over HTTP/2, so all requests multiplex onto one connection; a stalled warm-up request poisons that connection and the real POST then hangs until the client timeout (observed: 60s instead of 2.4s). If connection warming is ever wanted, it needs its own pool or HTTP/1.1 so a bad warm cannot take the real request down with it.

### M1 field results — live is fast but unreliable; batch is the default

M0.5 measured the live path at 374 ms and concluded it should be primary. Real
use contradicts that, and the default has been reversed.

**Live streaming**, over ~20 real dictations: roughly a third of sessions
acknowledge `setupComplete` and then ignore the audio entirely — zero interim
text, zero finals, despite 25–178 chunks being sent. Buffering audio until
`setupComplete` did not eliminate it. When it works it is excellent (290–600 ms);
when it fails it costs ~3.5 s before falling back. Root cause not established;
the evidence points at server-side session setup rather than the client.

**Batch**, same conditions: no failures. Latency scales with clip length rather
than being flat.

| Audio | Transcribe |
|---|---|
| 4.06 s | 2,436 ms |
| 6.24 s | 2,034 ms |
| 10.27 s | 3,867 ms |
| 9.80 s | 4,379 ms |

**Decision: batch (`gemini-3.5-transcribe`, `mode: "smart"`) is the default.**
Live remains a settings toggle. Predictability matters more than peak speed for
something used continuously, and a fast path that fails a third of the time is
worse than a steady one.

**Consequences for M2.** The overlay's job changes. With no live partials there
is no text to stream, so it must show recording state, a level meter, and a
clear "transcribing" phase covering a 2–4.5 s wait. That wait is now the thing
the overlay exists to make bearable — not partial text.

### Do not reuse HTTP connections to this API

The single largest source of latency was connection pooling, not the model.

Measured before the fix — dictations begun a few seconds after the previous one
stalled badly, while identical clips after a longer gap were fast:

| Gap since last request | Audio | Transcribe |
|---|---|---|
| 2.5 s | 3.57 s | **20,294 ms** |
| 4.4 s | 4.40 s | **26,075 ms** |
| 7.1 s | 16.17 s | 4,518 ms |
| 14.5 s | 7.28 s | 2,426 ms |

Google serves this endpoint over HTTP/2, so every request shares one
multiplexed connection and a degraded one stalls the next request instead of
failing it. This is the same failure mode that made connection pre-warming hang
for 60 s in M0 — twice now, from the same root cause.

Setting `pool_max_idle_per_host(0)` so each request opens a fresh connection
fixed it. After: 0.6 s and 1.0 s gaps both transcribed in ~2.3 s. A fresh TLS
handshake costs ~150–250 ms, which is a good trade.

**Do not also force HTTP/1.1.** `http1_only()` was tried alongside this and made
every request fail with a transport error — the endpoint requires HTTP/2.

Residual variance remains (one 9.5 s outlier at a 1.8 s gap), so pooling was not
the only factor. If tail latency needs further work, the option is a hedged
retry: fire a second request on a fresh connection if the first has not answered
in ~6 s and take whichever returns first. That costs a duplicate call on the slow
tail only.

### Other M1 findings

- **Ctrl+V, not Shift+Insert, is the right default.** Electron apps (Claude,
  Slack) ignore Shift+Insert. Shift+Insert remains the option for terminals.
  A third mode types the text as Unicode keystrokes for anything that ignores
  both.
- **The server ends turns mid-dictation** via its own silence detection
  (`generationComplete` partway through). Each new turn resets the interim
  transcript, so anything holding only the latest interim loses everything
  before the last boundary — this showed up as transcripts cut off partway.
  Segments must be banked as turns complete.
- **Never emit to the UI from inside the WebSocket loop.** Doing so ran
  serialisation, IPC and a mutex lock while the socket was blocked, starving
  audio sends. Partials go through a channel to a separate thread.
- **Focus is captured at key-down and restored before pasting.** If restoration
  fails after retries, injection is refused rather than pasting into whatever
  is focused — dictated text landing in the wrong app could be sent somewhere.
  The transcript stays in history.

## Feature 2 — Notes

**Hotkey:** `Ctrl+Shift+;` as a toggle. Tray icon shows recording state; you will not hold a key for ten minutes.

**Flow:** record -> transcribe (batch) -> structuring call -> persist -> show in Home.

There are two capture profiles sharing one pipeline:

| Profile | Trigger | Diarization | Max length | Template |
|---|---|---|---|---|
| Work note | `Ctrl+Shift+;` | off | 60 min | Agenda / action items |
| Phone call | `Ctrl+Shift+'` | off by default | 60 min | One-sided call |

Diarization is **off** for both, which matters more than it looks: enabling diarization or word timestamps drops the audio ceiling from 60 minutes to 30. A one-speaker recording gains nothing from diarization, so leaving it off buys the full hour. The exception is speakerphone — see below.

The structuring call returns strict JSON matching the mockup's shape:

```json
{
  "title": "Daily Work Agenda and Planning",
  "summary": "...",
  "takeaways": ["...", "..."],
  "action_items": [{ "text": "...", "done": false }],
  "notable": "verbatim quotes, numbers, or code mentioned — empty if none"
}
```

Templates are **editable and multiple** from day one, selected by `recordings.kind`. Same JSON shape, different system prompt.

### Phone-call mode (one-sided capture)

The mic hears your half of a cell call and nothing else. That constraint has to be handled explicitly at three layers, or the feature quietly produces confident nonsense.

**1. The structuring prompt must be told it is one-sided.** Given half a dialogue, a model's default behavior is to smooth over the gap and reconstruct what the other party "must have" said. That is exactly the failure mode to avoid — this note exists so you can trust it later. The call template instructs the model to:

- Treat the transcript as **only your side**; never invent or paraphrase the other party's words.
- Extract what is directly recoverable: **commitments you made**, **questions you asked**, **facts you stated** (numbers, addresses, dates, prices, names), and **anything you said you would follow up on**.
- Infer the other side's content only where your own words make it unambiguous (you repeated a number back, you said "okay, Thursday at 2"), and mark those as inferred.
- Emit an explicit `open_questions` list for points where your half clearly responds to something unrecoverable — those are the gaps you will want to check against your own memory while it is fresh.

The call template therefore extends the base schema:

```json
{
  "counterparty": "name if you said it, else null",
  "commitments_i_made": ["..."],
  "facts_stated": ["..."],
  "inferred": [{ "text": "...", "basis": "you repeated the date back" }],
  "open_questions": ["..."]
}
```

**2. Rolling pre-buffer.** A phone call starts before you can reach the keyboard — you answer, say hello, and only then think to record. Since the audio layer already owns a ring buffer, keep the **last 30 seconds of mic audio in memory** and prepend it when the hotkey fires, so the opening of the call survives.

This is opt-in and **off by default**, and it must be built honestly: memory only, never touches disk, discarded continuously, and the mic-in-use indicator is on the whole time it is armed. Make that behavior legible in Settings rather than burying it — an always-listening buffer the user did not know about is the kind of thing that destroys trust in a tool like this.

**3. Speakerphone toggle.** If the call is on speaker, the mic picks up both sides and the recording becomes genuinely two-party: switch the template to the standard two-sided prompt and turn diarization **on** (accepting the 30-minute ceiling). Expose this as a per-recording toggle you can flip before or during capture, not an auto-detect — getting it wrong silently is worse than asking.

Note that a call routed through the PC (Teams, Zoom, Discord) is the M5 loopback case and gives you both sides cleanly. A cell call is the one situation loopback can never solve, which is why mic-only is the right foundation here rather than a stopgap.

**Runaway protection.** Toggle-started recordings get forgotten. Ship all three: a hard max-duration cap that auto-stops and saves, an auto-stop after N minutes of continuous silence, and a tray icon plus overlay pip that is unmistakable while recording.

**Actions on a note:** Copy Markdown; Send to VS Code (`code -` piping the rendered markdown into a new untitled buffer — works regardless of which workspace is open); Delete; replay audio.

## Data model

```sql
-- kind: 'dictation' | 'note' | 'call'
recordings(id, kind, created_at, duration_ms, audio_path, sample_rate, source,
           speakerphone, prebuffered_ms, stop_reason)
transcripts(id, recording_id, text, language, model, diarized, tokens_in, tokens_out, created_at)
templates(id, kind, name, system_prompt, schema_json, is_default, updated_at)
notes(id, recording_id, template_id, title, summary, notable, counterparty, created_at)
takeaways(id, note_id, text, position)
action_items(id, note_id, text, done, position)
note_facts(id, note_id, kind, text, basis, position)  -- commitments / facts / inferred / open questions
vocab_terms(id, term, enabled)
dictations(id, recording_id, text, target_app, injected_ok, latency_ms, created_at)
settings(key, value)
usage(id, day, model, seconds, est_cost_usd)
```

`stop_reason` (`manual` / `max_duration` / `silence`) is worth storing — when a call note looks truncated, the first question is always whether the cap fired.

Audio is retained — the mockup's play buttons depend on it — with a retention sweep on startup deleting recordings older than N days. Default 30, configurable, `0` = keep forever.

## Milestones

**M0 — Spike (do this first, throw it away).** A single Rust binary, no UI: keyboard hook -> cpal capture -> batch transcribe -> clipboard paste. The only goal is a measured latency number and proof the paste lands correctly in VS Code, Windows Terminal, Chrome, and Slack. Every downstream decision depends on the number this produces.

**M0.5 — Live API latency probe. DONE.** 374 ms finalization, ~410 ms perceived. Dictation is viable; proceed.

**M1 — Dictation MVP.** Tauri shell, tray icon, Settings with API key + hotkey binding + mic selection, vocabulary editor, SQLite, dictation history. Batch path only — it is too slow to live with, but it proves the plumbing end to end and is the fallback the streaming path falls back *to*.

**M2 — Streaming (promoted: this is the feature, not polish).** Live WS client as the primary dictation path, with batch as fallback. Non-activating always-on-top transparent overlay (`focus: false`, `decorations: false`, `transparent: true`, `skipTaskbar: true`, `set_ignore_cursor_events(true)`; verify `WS_EX_NOACTIVATE` is actually applied — a focus-stealing overlay defeats the entire feature). Because the injected text now comes from the live path, the overlay is load-bearing: the user has to see what was heard before it lands.

**M3 — Notes mode. DONE.** Toggle hotkey, batch transcription, structuring via a configurable model, notes list and detail, action-item checkboxes, Copy Markdown / Send to VS Code / Delete, runaway protection.

Field lessons: save the transcript *before* summarising (a slow summariser otherwise looks like a lost note); give summarising its own queue (a two-minute retry once delayed the next note by 112s); make the summary model configurable (`gemini-3.7-flash` hit sustained high-demand errors; `gemini-3.5-flash-lite` is the new default).

**M3 — original scope.** Toggle hotkey, longer-form capture, structuring call via `gemini-3.7-flash`, Home list + note detail, Copy Markdown / Send to VS Code / Delete, action-item checkboxes persisted. Runaway protection (max duration, silence auto-stop, unmistakable recording indicator).

**M3.5 — Phone-call mode. DONE.** Call hotkey and `call` kind, one-sided template with `inferred` and `open_questions`, speakerphone toggle with diarisation, opt-in rolling pre-buffer, violet call pill.

Field lessons: the UI hotkey forwarding had to cover the call chord too (it dies over our own window otherwise); notes and calls needed the same peak-level silence check dictation already had; call audio arrives far quieter than dictation (0.098 vs 0.26-0.74 peak) because the phone points away from the mic, so quiet recordings are now normalised before transcription.

**M3.5 — original scope.** Second hotkey and `call` kind, one-sided structuring template with `open_questions` and `inferred`, speakerphone toggle, rolling pre-buffer (opt-in, off by default). Split out from M3 because the template work is where the actual difficulty lives and it deserves its own iteration loop against real calls.

**M4 — Fit and finish.** Usage/cost counter, retention sweep, error toasts, auto-start. Scope is two machines, so: portable build, no installer, no onboarding flow, no code signing — just a first-run screen to paste an API key.

**M5 — Loopback (deferred).** WASAPI loopback `CaptureSource`, two-stream mixing, diarized speaker labels in the note view. Covers PC-routed calls (Teams/Zoom/Discord); cannot help with cell calls.

## Scope

Personal use — your machine, plus possibly your dad's. That removes installers, onboarding, auto-update, multi-user profiles, and code signing from the plan, and it means each machine uses its own Gemini API key entered on first run. It does **not** remove the runaway-recording protections or the pre-buffer's off-by-default posture; those are correctness and trust, not distribution polish.

## Risks

- **One-sided transcripts invite confident fabrication.** The single biggest quality risk in call mode. A summarizer handed half a dialogue will reconstruct the missing half unless forbidden from doing so, and a plausible invented commitment is worse than no note at all. Mitigated by the template's inference rules and the explicit `inferred` / `open_questions` fields — and worth eyeballing hard on the first dozen real calls.
- **Rolling pre-buffer is always listening while armed.** Memory-only, never written to disk unless you press the key, off by default, and surfaced plainly in Settings. Also: if the phone call is on speaker, the buffer holds the other party's voice too. Fine for personal one-party-consent use, but know that is what it does.
- **AV / EDR false positives.** A global low-level keyboard hook plus `SendInput` plus microphone access is, structurally, what a keylogger looks like. At two-machine scale this is just clicking through SmartScreen and adding a Defender exclusion rather than buying a certificate — but expect it on your dad's machine too.
- **Elevated windows.** Injection into admin-elevated apps requires the app itself to be elevated. Accepted limitation — detect the case and show a clear overlay message rather than silently dropping text.
- ~~**Dictation may simply not be fast enough.**~~ **Retired by M0.5** — 374 ms finalization, ~410 ms perceived. Batch at ~2.4s remains unusable for dictation, so the live path is load-bearing: if the WebSocket fails, dictation degrades to a visibly slow fallback rather than silently working. Surface that state in the overlay.
- **Right Ctrl is swallowed globally.** Confirmed working in M0, and it means Right Ctrl stops functioning as a modifier everywhere while the app runs. Intended, but it needs to be stated in Settings, and the hook must be uninstalled cleanly on exit and crash.
- **Hotkey collisions.** Right Ctrl is unusual enough to be safe, but the hook must pass through keys it does not own, and must fail open — a crashed hook thread that swallows input is the worst possible failure. Watchdog plus auto-reinstall.
- **Clipboard clobber.** Mitigated by save/restore with an ownership check, but it is a real behavioral cost of the approach.
- **Live API 10-minute session cap.** Only affects the preview path; long dictations must roll the WS session or drop to batch-only.

## Deferred ideas

- Auto-populate vocabulary by scanning the active repo for identifiers.
- Context-aware cleanup keyed to the foreground app.
- Spoken commands ("new line", "camel case that").
- Re-run an old recording against an updated notes template.
