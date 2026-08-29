# M0 Spike

Throwaway. One job: produce a real latency number and prove the paste lands
correctly across apps. Delete this directory once M1 starts.

## Prerequisites

Already installed on this machine:

- rustc / cargo 1.98.0, `stable-x86_64-pc-windows-msvc`
- Visual Studio 2022 Build Tools with the VCTools workload (MSVC 14.44)

If `cargo` is not on PATH in a given shell, it lives at
`%USERPROFILE%\.cargo\bin`.

## Run

```bash
cd m0-spike; $env:GEMINI_API_KEY = "your-key-here"; cargo run --release
```

Use `--release`. A debug build's resampling loop adds noise to the very
measurement this exists to take.

Then hold **Right Ctrl**, say a sentence, release. The transcript pastes into
whatever field had focus, and a timing table prints in the console.

### Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `GEMINI_API_KEY` | required | API key, sent as `x-goog-api-key` |
| `M0_MODEL` | `gemini-3.5-transcribe` | Override the model |
| `M0_PASTE` | `shift_insert` | Set to `ctrl_v` to compare paste chords |
| `M0_SAVE_WAV` | unset | Set to `1` to dump each clip next to the binary |
| `M0_SELFTEST` | unset | Post a synthetic tone, dump the raw response, exit |
| `M0_PREWARM` | unset (off) | Re-enable connection pre-warming. Known harmful. |
| `M0_LIVE` | unset | **Live WebSocket mode** (M0.5) instead of batch |
| `M0_LIVE_VERBOSE` | unset | Dump every raw WebSocket frame |

## M0.5 — the live measurement

```bash
$env:M0_LIVE=1; C:\Coding\GeminiFlow_v2\m0-spike\target\release\m0-spike.exe
```

Streams 100 ms PCM16 chunks over WebSocket to `gemini-3.5-transcribe-live`
while you speak, sends `audioStreamEnd` on key release, and reports:

- **`finalize (live)`** — ms from `audioStreamEnd` to the final transcript.
  This is the number that decides whether dictation is viable. Batch was
  ~2,400 ms; anything under ~400 ms here makes the feature work.
- **`first partial at`** — how far into the utterance the first interim result
  arrived. Determines whether the overlay has anything to show while speaking.
- **`partials received`** — if this is 0, streaming is not actually working and
  the finalize number is meaningless.

If it errors or hangs, re-run with `M0_LIVE_VERBOSE=1` to dump raw frames. The
docs were already wrong once about response shape, so verify rather than trust:
the parser looks for `serverContent.interimInputTranscription.text` and
`serverContent.inputTranscription.text`.

## Results (answered)

**Batch transcribe is ~2,400 ms.** Far over the ~700 ms the plan assumed.

- 31 KB payload: 3,049 ms. 185 KB payload: 2,395 ms. Latency is not
  payload-driven, it is roughly fixed inference cost. Shrinking audio will not
  help.
- Transcription accuracy was correct on the first real utterance.
- `prewarm()` was actively harmful and is now default-off — see below.

**Consequence:** the batch call cannot sit in the dictation critical path. The
live streaming model becomes the primary path and the overlay becomes
load-bearing rather than decorative. See `../PLAN.md` for the revised plan.

## M0.5 results — live wins, dictation is viable

Measured on a 6.48s utterance, 53 chunks streamed:

| Stage | Measured |
|---|---|
| Stop capture, flush tail | 10 ms |
| **Live finalization after `audioStreamEnd`** | **374 ms** |
| Focus restore + paste | ~25 ms |
| **Perceived total** | **~410 ms** |

6.4× faster than batch. Transcription was verbatim-accurate.

- First partial arrived **1,926 ms** into speech (11 partials over 6.5s). The
  overlay is empty for ~2s at the start of every utterance — it needs a
  listening indicator, not just text.
- The clipboard restore was moved off the critical path (it was adding 250 ms
  to every dictation after the text had already landed).

## Verified API facts

- Endpoint `POST /v1beta/interactions`, key in `x-goog-api-key`. Inline base64
  audio works; no Files API round-trip needed.
- **The Live API replies with BINARY WebSocket frames containing JSON**, not
  text frames. A client that handles only `Message::Text` discards every
  message including `setupComplete`, which is indistinguishable from a server
  that never answers. This cost a debugging cycle — do not repeat it in M2.
- Live wire protocol is otherwise exactly as documented: setup with
  `models/<model>` prefix, audio as `realtimeInput.audio` with
  `mimeType: audio/pcm;rate=16000`, end via `realtimeInput.audioStreamEnd`,
  partials at `serverContent.interimInputTranscription.text`, finals at
  `serverContent.inputTranscription.text`. Finals also arrive mid-utterance at
  natural pauses, so only a final received *after* `audioStreamEnd` counts.
- **Transcript is at `steps[].content[].text`**, not the documented
  `interaction.output_text`. Audio with no speech returns **no `steps` key at
  all** rather than an empty transcript.
- **Never pre-warm with a throwaway request.** Google serves this over HTTP/2,
  so requests multiplex onto one connection; a stalled warm-up poisons it and
  the real POST hangs until the client timeout (60s observed, vs 2.4s without).
  `M0_PREWARM=1` re-enables it only for measurement.

## Paste target matrix

Test each and record pass/fail for both `shift_insert` and `ctrl_v`:

| App | Notes |
|---|---|
| VS Code | The primary use case |
| Windows Terminal (PowerShell) | Ctrl+V is the suspect one here |
| Git Bash / mintty | Expected to need Shift+Insert |
| Chrome address bar + a textarea | |
| Slack message box | Electron |
| Notepad | Control case — if this fails, something basic is wrong |
| An elevated PowerShell | Expected to FAIL unless the spike runs elevated |

## Known rough edges (deliberate — it's a spike)

- **Right Ctrl is swallowed globally** while running. It will not work as a
  modifier in any app until you quit.
- **The resampler has no anti-aliasing filter.** Fine for latency numbers. If
  transcripts look sloppy, that missing low-pass is the first suspect, not the
  model.
- No pre-buffer, no silence detection, no max duration, no UI, no persistence.
- The `windows` crate is pinned to 0.58 and compiles clean against it; its
  win32 signatures shift between releases, so a version bump may need small
  adjustments in `hook.rs` and `inject.rs`.
- If a rebuild fails with `Access is denied` on `m0-spike.exe`, the previous
  run is still holding the keyboard hook. Ctrl+C it first.

## If Windows Defender complains

A global keyboard hook plus `SendInput` plus mic access is structurally what a
keylogger looks like. Expect SmartScreen on first run and possibly a Defender
prompt. Add an exclusion for the `target/release` directory.
