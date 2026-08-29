//! Mic capture. Owns its own thread because a cpal `Stream` is not `Send` on
//! WASAPI -- it has to be built, played and dropped on one thread.
//!
//! Output is always 16 kHz mono f32, which is what the STT model wants and
//! keeps the upload small (5s of speech is ~160 KB of PCM vs ~960 KB at 48 kHz
//! stereo). Upload size is directly in the latency budget, so this matters.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

pub const TARGET_RATE: u32 = 16_000;

/// Live consumer of audio as it is captured, in 16 kHz mono PCM16 chunks.
/// Deliberately a closure rather than a typed channel so this module does not
/// need to know the live session exists.
pub type AudioTap = Arc<dyn Fn(Vec<i16>) + Send + Sync>;

enum Cmd {
    Start(Option<AudioTap>),
    Stop,
}

pub struct Recorder {
    cmd: Sender<Cmd>,
    out: Receiver<Result<Vec<f32>>>,
}

impl Recorder {
    pub fn spawn() -> Recorder {
        let (cmd_tx, cmd_rx) = channel::<Cmd>();
        let (out_tx, out_rx) = channel::<Result<Vec<f32>>>();

        std::thread::spawn(move || loop {
            // Wait for a Start. A stray Stop (release with no matching press)
            // must be ignored, not treated as shutdown -- otherwise one
            // out-of-order event permanently kills capture for the session.
            let tap = match cmd_rx.recv() {
                Ok(Cmd::Start(tap)) => tap,
                Ok(Cmd::Stop) => continue,
                Err(_) => return, // channel closed
            };

            let result = record_until_stop(&cmd_rx, tap);
            if out_tx.send(result).is_err() {
                return;
            }
        });

        Recorder {
            cmd: cmd_tx,
            out: out_rx,
        }
    }

    /// `tap` receives 16 kHz PCM16 chunks during capture. `None` for the batch
    /// path, which only needs the complete buffer at the end.
    pub fn start(&self, tap: Option<AudioTap>) {
        let _ = self.cmd.send(Cmd::Start(tap));
    }

    /// Stops capture and returns 16 kHz mono samples.
    pub fn stop(&self) -> Result<Vec<f32>> {
        self.cmd
            .send(Cmd::Stop)
            .map_err(|_| anyhow!("recorder thread is gone"))?;
        self.out
            .recv()
            .map_err(|_| anyhow!("recorder thread died mid-capture"))?
    }
}

fn record_until_stop(cmd_rx: &Receiver<Cmd>, tap: Option<AudioTap>) -> Result<Vec<f32>> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device -- check Windows mic privacy settings"))?;

    let supported = device.default_input_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    eprintln!(
        "  [audio] {} @ {} Hz, {} ch, {:?}",
        device.name().unwrap_or_else(|_| "<unnamed>".into()),
        sample_rate,
        channels,
        format
    );

    let buf = Arc::new(Mutex::new(Vec::<f32>::with_capacity(
        sample_rate as usize * 8,
    )));
    let err_fn = |e| eprintln!("  [audio] stream error: {e}");

    let stream = {
        let buf = Arc::clone(&buf);
        match format {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _: &_| push_mono(&buf, data, channels, |s| s),
                err_fn,
                None,
            )?,
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _: &_| {
                    push_mono(&buf, data, channels, |s| s as f32 / i16::MAX as f32)
                },
                err_fn,
                None,
            )?,
            SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _: &_| {
                    push_mono(&buf, data, channels, |s| {
                        (s as f32 - 32768.0) / 32768.0
                    })
                },
                err_fn,
                None,
            )?,
            other => return Err(anyhow!("unsupported sample format {other:?}")),
        }
    };

    stream.play()?;

    // Pump loop: wait for the release edge, and while waiting, feed the tap
    // roughly every 100ms so the live session transcribes during speech
    // instead of after it.
    let chunk_frames = sample_rate as usize / 10;
    let mut sent_frames = 0usize;

    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Cmd::Start(_)) => continue,
            Err(RecvTimeoutError::Timeout) => {
                if let Some(tap) = &tap {
                    let pending = take_pending(&buf, &mut sent_frames, chunk_frames);
                    if let Some(chunk) = pending {
                        let resampled = resample_linear(&chunk, sample_rate, TARGET_RATE);
                        if !resampled.is_empty() {
                            tap(to_pcm16(&resampled));
                        }
                    }
                }
            }
        }
    }

    drop(stream); // stops capture

    // Flush whatever was captured after the last pump tick, so the tail of the
    // utterance is not lost.
    if let Some(tap) = &tap {
        if let Some(chunk) = take_pending(&buf, &mut sent_frames, 1) {
            let resampled = resample_linear(&chunk, sample_rate, TARGET_RATE);
            if !resampled.is_empty() {
                tap(to_pcm16(&resampled));
            }
        }
    }

    let raw = buf.lock().unwrap().clone();
    Ok(resample_linear(&raw, sample_rate, TARGET_RATE))
}

/// Takes everything captured since the last call, once at least
/// `min_frames` are available. Advances the cursor.
fn take_pending(
    buf: &Arc<Mutex<Vec<f32>>>,
    sent_frames: &mut usize,
    min_frames: usize,
) -> Option<Vec<f32>> {
    let guard = buf.lock().ok()?;
    if guard.len().saturating_sub(*sent_frames) < min_frames.max(1) {
        return None;
    }
    let chunk = guard[*sent_frames..].to_vec();
    *sent_frames = guard.len();
    Some(chunk)
}

pub fn to_pcm16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

/// Downmix interleaved frames to mono by averaging channels.
fn push_mono<T: Copy>(
    buf: &Arc<Mutex<Vec<f32>>>,
    data: &[T],
    channels: usize,
    conv: impl Fn(T) -> f32,
) {
    let mut guard = match buf.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if channels <= 1 {
        guard.extend(data.iter().copied().map(&conv));
        return;
    }
    for frame in data.chunks_exact(channels) {
        let sum: f32 = frame.iter().copied().map(&conv).sum();
        guard.push(sum / channels as f32);
    }
}

/// Linear interpolation resample. No anti-aliasing filter -- fine for a spike
/// measuring latency, but if transcript quality looks poor at 48k->16k, that
/// missing low-pass is the first thing to suspect.
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == to {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = (input.len() as f64 / ratio).floor() as usize;
    let last = input.len() - 1;

    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let i0 = pos.floor() as usize;
            let i1 = (i0 + 1).min(last);
            let frac = (pos - i0 as f64) as f32;
            input[i0] * (1.0 - frac) + input[i1] * frac
        })
        .collect()
}

/// 16-bit PCM WAV in memory, ready to base64.
pub fn to_wav(samples: &[f32]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for &s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            writer.write_sample((clamped * i16::MAX as f32) as i16)?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}

/// Rough loudness check, so "it returned nothing" can be distinguished from
/// "the mic was muted the whole time".
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}
