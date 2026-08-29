//! GeminiFlow M0 spike.
//!
//! Measures the real end-to-end latency of
//!   hold Right Ctrl -> talk -> release -> text appears in the focused field
//! and proves the paste lands correctly in VS Code, Windows Terminal, Chrome
//! and Slack.
//!
//! Two modes, so they can be compared in one harness:
//!   default      batch  -- measured at ~2.4s, too slow for the critical path
//!   M0_LIVE=1    live   -- streams while you speak; only finalization remains
//!
//! The live number decides whether dictation is viable at all. Throw this code
//! away afterwards.

mod audio;
mod gemini;
mod hook;
mod inject;
mod live;

use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::HWND;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Batch,
    Live,
}

struct Config {
    api_key: String,
    model: String,
    mode: Mode,
    paste: inject::PasteMode,
    save_wav: bool,
}

fn main() -> Result<()> {
    let api_key = std::env::var("GEMINI_API_KEY").map_err(|_| {
        anyhow!("GEMINI_API_KEY is not set. PowerShell: $env:GEMINI_API_KEY = \"...\"")
    })?;

    let mode = if std::env::var("M0_LIVE").is_ok() {
        Mode::Live
    } else {
        Mode::Batch
    };

    let model = std::env::var("M0_MODEL").unwrap_or_else(|_| {
        match mode {
            Mode::Batch => "gemini-3.5-transcribe",
            Mode::Live => "gemini-3.5-transcribe-live",
        }
        .to_string()
    });

    let config = Config {
        api_key,
        model,
        mode,
        paste: inject::PasteMode::from_env(),
        save_wav: std::env::var("M0_SAVE_WAV").is_ok(),
    };

    println!("GeminiFlow M0 spike");
    println!(
        "  mode       {}   (M0_LIVE=1 for live)",
        match mode {
            Mode::Batch => "BATCH",
            Mode::Live => "LIVE",
        }
    );
    println!("  model      {}", config.model);
    println!("  paste      {:?}   (M0_PASTE=ctrl_v to switch)", config.paste);
    println!();

    // Isolates request shape from audio capture. Posts a synthetic tone and
    // dumps the raw response, then exits.
    if std::env::var("M0_SELFTEST").is_ok() {
        let client = gemini::Client::new(
            config.api_key.clone(),
            config.model.clone(),
            gemini::default_vocabulary(),
        )?;
        return client.self_test();
    }

    println!("Hold RIGHT CTRL, speak, release. Ctrl+C in this window to quit.");
    println!("Right Ctrl is swallowed while this runs -- it will not reach other apps.");
    println!();

    let (tx, rx) = channel::<hook::Event>();
    std::thread::spawn(move || {
        if let Err(e) = run_sessions(rx, config) {
            eprintln!("session loop died: {e:#}");
        }
    });

    // Blocks forever pumping messages for the hook.
    hook::install_and_pump(tx)
}

#[derive(Default)]
struct Timings {
    press: Option<Instant>,
    release: Option<Instant>,
    captured: Option<Instant>,
    transcribed: Option<Instant>,
    injected: Option<Instant>,
}

struct Session {
    timings: Timings,
    target: HWND,
    live: Option<live::LiveHandle>,
}

fn run_sessions(rx: Receiver<hook::Event>, config: Config) -> Result<()> {
    let recorder = audio::Recorder::spawn();
    let client = gemini::Client::new(
        config.api_key.clone(),
        config.model.clone(),
        gemini::default_vocabulary(),
    )?;

    let mut session: Option<Session> = None;

    for event in rx {
        match event {
            hook::Event::Press => {
                if session.is_some() {
                    continue; // already recording
                }
                let target = inject::foreground_window();
                println!("\n-- recording --");

                let (live_handle, tap) = match config.mode {
                    Mode::Batch => (None, None),
                    Mode::Live => {
                        let handle = live::start(
                            config.api_key.clone(),
                            config.model.clone(),
                            gemini::default_vocabulary(),
                        );
                        let sink = handle.sender();
                        let tap: audio::AudioTap = Arc::new(move |pcm| {
                            let _ = sink.send(live::LiveMsg::Audio(pcm));
                        });
                        (Some(handle), Some(tap))
                    }
                };

                recorder.start(tap);
                client.prewarm(); // default off -- see gemini.rs

                session = Some(Session {
                    timings: Timings {
                        press: Some(Instant::now()),
                        ..Default::default()
                    },
                    target,
                    live: live_handle,
                });
            }

            hook::Event::Release => {
                let Some(mut s) = session.take() else { continue };
                s.timings.release = Some(Instant::now());

                // Stopping the recorder also flushes the tail of the utterance
                // to the tap, so this must happen before audioStreamEnd.
                let samples = match recorder.stop() {
                    Ok(samples) => samples,
                    Err(e) => {
                        eprintln!("capture failed: {e:#}");
                        continue;
                    }
                };
                s.timings.captured = Some(Instant::now());

                let seconds = samples.len() as f32 / audio::TARGET_RATE as f32;
                if seconds < 0.25 {
                    println!("too short ({seconds:.2}s) -- ignoring");
                    continue;
                }
                if audio::peak(&samples) < 0.005 {
                    println!(
                        "captured {seconds:.2}s but peak level is near zero -- mic is \
                         probably muted or the wrong device is default"
                    );
                    continue;
                }

                let outcome = match s.live.take() {
                    Some(handle) => finish_live(handle),
                    None => finish_batch(&client, &samples, config.save_wav),
                };

                let (text, detail) = match outcome {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("transcription failed: {e:#}");
                        continue;
                    }
                };
                s.timings.transcribed = Some(Instant::now());

                println!("  \"{text}\"");

                if let Err(e) = inject::paste_into(s.target, &text, config.paste) {
                    eprintln!("injection failed: {e:#}");
                    continue;
                }
                s.timings.injected = Some(Instant::now());

                report(&s.timings, seconds, &detail);
            }
        }
    }

    Ok(())
}

fn finish_live(handle: live::LiveHandle) -> Result<(String, String)> {
    let result = handle.finish()?;
    let detail = format!(
        "  finalize (live)      {:>8} ms   <-- THE NUMBER\n  \
         first partial at     {:>8}\n  \
         partials received    {:>8}",
        result.finalize_ms,
        result
            .first_partial_ms
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_else(|| "never".to_string()),
        result.partial_count
    );
    Ok((result.transcript, detail))
}

fn finish_batch(
    client: &gemini::Client,
    samples: &[f32],
    save_wav: bool,
) -> Result<(String, String)> {
    let wav = audio::to_wav(samples)?;

    if save_wav {
        let name = format!("m0-{}.wav", now_stamp());
        match std::fs::write(&name, &wav) {
            Ok(()) => println!("wrote {name}"),
            Err(e) => eprintln!("could not write {name}: {e}"),
        }
    }

    let started = Instant::now();
    let text = client.transcribe(&wav)?;
    let detail = format!(
        "  transcribe (batch)   {:>8} ms\n  wav payload          {:>8.1} KB",
        started.elapsed().as_millis(),
        wav.len() as f64 / 1024.0
    );
    Ok((text, detail))
}

fn report(t: &Timings, audio_seconds: f32, detail: &str) {
    let (Some(press), Some(release), Some(injected)) = (t.press, t.release, t.injected) else {
        return;
    };
    let ms = |a: Instant, b: Instant| b.duration_since(a).as_secs_f64() * 1000.0;

    println!();
    println!("  spoke for            {audio_seconds:>8.2} s");
    println!("  ---- after key release ----");
    if let Some(captured) = t.captured {
        println!("  stop capture         {:>8.0} ms", ms(release, captured));
    }
    println!("{detail}");
    if let Some(transcribed) = t.transcribed {
        println!("  focus + paste        {:>8.0} ms", ms(transcribed, injected));
    }
    println!("  =========================");
    println!("  TOTAL PERCEIVED      {:>8.0} ms", ms(release, injected));
    println!("  (key down to done    {:>8.0} ms)", ms(press, injected));
}

fn now_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}
