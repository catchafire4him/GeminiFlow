//! Mic capture. Owns its own thread because a cpal `Stream` is not `Send` on
//! WASAPI: it must be built, played and dropped on one thread.
//!
//! Emits 16 kHz mono. That is what the model wants, and it keeps the payload
//! small enough that upload time stays out of the latency budget.
//!
//! `CaptureSource` selection is deliberately narrow right now (default device
//! or one named device). WASAPI loopback for meeting capture slots in here.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

pub const TARGET_RATE: u32 = 16_000;

/// Consumes audio as it is captured, in 16 kHz mono PCM16 chunks. A closure
/// rather than a typed channel so this module need not know about the live
/// transcription session.
pub type AudioTap = Arc<dyn Fn(Vec<i16>) + Send + Sync>;

pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    let mut out = Vec::new();
    for device in host.input_devices()? {
        if let Ok(name) = device.name() {
            let is_default = name == default_name;
            out.push(DeviceInfo { name, is_default });
        }
    }
    Ok(out)
}

enum Cmd {
    Start {
        device: Option<String>,
        tap: Option<AudioTap>,
    },
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
            // A stray Stop (release with no matching press) must be ignored,
            // not treated as shutdown, or one out-of-order event permanently
            // kills capture.
            let (device, tap) = match cmd_rx.recv() {
                Ok(Cmd::Start { device, tap }) => (device, tap),
                Ok(Cmd::Stop) => continue,
                Err(_) => return,
            };

            let result = record_until_stop(&cmd_rx, device, tap);
            if out_tx.send(result).is_err() {
                return;
            }
        });

        Recorder {
            cmd: cmd_tx,
            out: out_rx,
        }
    }

    pub fn start(&self, device: Option<String>, tap: Option<AudioTap>) {
        let _ = self.cmd.send(Cmd::Start { device, tap });
    }

    /// Stops capture and returns the complete 16 kHz mono buffer.
    pub fn stop(&self) -> Result<Vec<f32>> {
        self.cmd
            .send(Cmd::Stop)
            .map_err(|_| anyhow!("recorder thread is gone"))?;
        self.out
            .recv()
            .map_err(|_| anyhow!("recorder thread died mid-capture"))?
    }
}

/// Rolling window of recent microphone audio, held only in memory.
///
/// Phone calls start before you can reach the keyboard: you answer, say hello,
/// and only then think to record. This keeps the last N seconds so hitting the
/// key part-way through still captures the opening.
///
/// Deliberate properties, because this is an open microphone:
/// - Off unless explicitly enabled in Settings.
/// - Never written to disk. The ring lives in RAM and is dropped on disarm.
/// - Discarded continuously; only a `snapshot` at the moment you start
///   recording is ever kept.
/// - Disarmed while a real recording runs, so there is only ever one stream.
pub struct PreBuffer {
    cmd: Sender<PreCmd>,
    snap: Receiver<Vec<f32>>,
}

enum PreCmd {
    Arm { device: Option<String>, seconds: usize },
    Disarm,
    Snapshot,
}

impl PreBuffer {
    pub fn spawn() -> PreBuffer {
        let (cmd_tx, cmd_rx) = channel::<PreCmd>();
        let (snap_tx, snap_rx) = channel::<Vec<f32>>();

        std::thread::spawn(move || {
            loop {
                // Idle until armed.
                let (device, seconds) = match cmd_rx.recv() {
                    Ok(PreCmd::Arm { device, seconds }) => (device, seconds),
                    Ok(PreCmd::Snapshot) => {
                        let _ = snap_tx.send(Vec::new());
                        continue;
                    }
                    Ok(PreCmd::Disarm) => continue,
                    Err(_) => return,
                };

                if let Err(e) = run_prebuffer(&cmd_rx, &snap_tx, device, seconds) {
                    crate::logln!("[prebuffer] stopped: {e}");
                }
            }
        });

        PreBuffer {
            cmd: cmd_tx,
            snap: snap_rx,
        }
    }

    pub fn arm(&self, device: Option<String>, seconds: usize) {
        let _ = self.cmd.send(PreCmd::Arm { device, seconds });
    }

    pub fn disarm(&self) {
        let _ = self.cmd.send(PreCmd::Disarm);
    }

    /// Returns the buffered audio as 16 kHz mono and clears the ring.
    pub fn snapshot(&self) -> Vec<f32> {
        if self.cmd.send(PreCmd::Snapshot).is_err() {
            return Vec::new();
        }
        self.snap
            .recv_timeout(Duration::from_millis(500))
            .unwrap_or_default()
    }
}

fn run_prebuffer(
    cmd_rx: &Receiver<PreCmd>,
    snap_tx: &Sender<Vec<f32>>,
    device: Option<String>,
    seconds: usize,
) -> Result<()> {
    let device = open_device(device)?;
    let supported = device.default_input_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let capacity = sample_rate as usize * seconds.clamp(5, 120);
    let ring = Arc::new(Mutex::new(std::collections::VecDeque::<f32>::with_capacity(
        capacity,
    )));
    let err_fn = |e| crate::logln!("[prebuffer] stream error: {e}");

    let stream = {
        let ring = Arc::clone(&ring);
        match format {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _: &_| push_ring(&ring, data, channels, capacity, |s| s),
                err_fn,
                None,
            )?,
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _: &_| {
                    push_ring(&ring, data, channels, capacity, |s| {
                        s as f32 / i16::MAX as f32
                    })
                },
                err_fn,
                None,
            )?,
            SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _: &_| {
                    push_ring(&ring, data, channels, capacity, |s| {
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
    crate::logln!("[prebuffer] armed, keeping {seconds}s");

    loop {
        match cmd_rx.recv() {
            Ok(PreCmd::Snapshot) => {
                let raw: Vec<f32> = ring
                    .lock()
                    .map(|mut r| r.drain(..).collect())
                    .unwrap_or_default();
                let resampled = resample_linear(&raw, sample_rate, TARGET_RATE);
                crate::logln!(
                    "[prebuffer] snapshot {:.1}s",
                    resampled.len() as f32 / TARGET_RATE as f32
                );
                let _ = snap_tx.send(resampled);
                // The real recorder takes the microphone from here.
                return Ok(());
            }
            Ok(PreCmd::Disarm) | Err(_) => {
                crate::logln!("[prebuffer] disarmed");
                return Ok(());
            }
            Ok(PreCmd::Arm { .. }) => continue,
        }
    }
}

fn push_ring<T: Copy>(
    ring: &Arc<Mutex<std::collections::VecDeque<f32>>>,
    data: &[T],
    channels: usize,
    capacity: usize,
    conv: impl Fn(T) -> f32,
) {
    let Ok(mut r) = ring.lock() else { return };
    let mut push = |s: f32| {
        if r.len() == capacity {
            r.pop_front();
        }
        r.push_back(s);
    };

    if channels <= 1 {
        for &s in data {
            push(conv(s));
        }
        return;
    }
    for frame in data.chunks_exact(channels) {
        let sum: f32 = frame.iter().copied().map(&conv).sum();
        push(sum / channels as f32);
    }
}

fn open_device(preferred: Option<String>) -> Result<cpal::Device> {
    let host = cpal::default_host();

    if let Some(name) = preferred {
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                if device.name().map(|n| n == name).unwrap_or(false) {
                    return Ok(device);
                }
            }
        }
        // Configured device is unplugged (a wireless headset, typically).
        // Falling back beats failing silently.
        crate::logln!("input device {name:?} not found, using system default");
    }

    host.default_input_device()
        .ok_or_else(|| anyhow!("no input device available -- check Windows microphone privacy settings"))
}

fn record_until_stop(
    cmd_rx: &Receiver<Cmd>,
    device: Option<String>,
    tap: Option<AudioTap>,
) -> Result<Vec<f32>> {
    let device = open_device(device)?;
    let supported = device.default_input_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let buf = Arc::new(Mutex::new(Vec::<f32>::with_capacity(
        sample_rate as usize * 8,
    )));
    let err_fn = |e| crate::logln!("audio stream error: {e}");

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
                    push_mono(&buf, data, channels, |s| (s as f32 - 32768.0) / 32768.0)
                },
                err_fn,
                None,
            )?,
            other => return Err(anyhow!("unsupported sample format {other:?}")),
        }
    };

    stream.play()?;

    // Wait for the release edge, feeding the tap in fixed 100ms chunks so the
    // live session transcribes during speech rather than after it.
    let mut sent_frames = 0usize;
    let mut resampler = StreamResampler::new(sample_rate);

    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(40)) {
            Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Cmd::Start { .. }) => continue,
            Err(RecvTimeoutError::Timeout) => {
                if let Some(tap) = &tap {
                    if let Some(fresh) = take_pending(&buf, &mut sent_frames, 1) {
                        resampler.push(&fresh);
                        while let Some(chunk) = resampler.take_chunk() {
                            tap(to_pcm16(&chunk));
                        }
                    }
                }
            }
        }
    }

    drop(stream);

    // Flush the tail, including the partial chunk, so the end of the utterance
    // is not lost.
    if let Some(tap) = &tap {
        if let Some(fresh) = take_pending(&buf, &mut sent_frames, 1) {
            resampler.push(&fresh);
        }
        while let Some(chunk) = resampler.take_chunk() {
            tap(to_pcm16(&chunk));
        }
        let rest = resampler.drain();
        if !rest.is_empty() {
            tap(to_pcm16(&rest));
        }
    }

    let raw = buf.lock().unwrap().clone();
    Ok(resample_linear(&raw, sample_rate, TARGET_RATE))
}

/// Continuous resampler for the live stream.
///
/// Resampling each chunk independently resets the interpolation phase and
/// clamps at the chunk edge, putting a small discontinuity every 100ms -- about
/// a hundred of them in a ten-second dictation. The batch path never had this
/// problem because it resamples the whole buffer at once, which is a plausible
/// reason batch succeeds on audio the live stream gives up on.
///
/// This keeps the fractional read position and the unconsumed input across
/// calls, so the output is one continuous stream, and emits fixed 100ms chunks
/// rather than whatever happened to accumulate.
struct StreamResampler {
    ratio: f64,
    offset: f64,
    pending: Vec<f32>,
    out: Vec<f32>,
}

impl StreamResampler {
    /// 100ms at 16 kHz, inside the 1024-2048 frame window the API documents.
    const CHUNK: usize = 1600;

    fn new(from_rate: u32) -> Self {
        StreamResampler {
            ratio: from_rate as f64 / TARGET_RATE as f64,
            offset: 0.0,
            pending: Vec::new(),
            out: Vec::new(),
        }
    }

    fn push(&mut self, input: &[f32]) {
        self.pending.extend_from_slice(input);

        // Needs one sample past the read position to interpolate against.
        while (self.offset.floor() as usize) + 1 < self.pending.len() {
            let i0 = self.offset.floor() as usize;
            let frac = (self.offset - i0 as f64) as f32;
            self.out
                .push(self.pending[i0] * (1.0 - frac) + self.pending[i0 + 1] * frac);
            self.offset += self.ratio;
        }

        // Discard input that is fully behind the read position, keeping the
        // fractional part so the next call continues mid-sample.
        let consumed = self.offset.floor() as usize;
        if consumed > 0 {
            self.pending.drain(..consumed);
            self.offset -= consumed as f64;
        }
    }

    fn take_chunk(&mut self) -> Option<Vec<f32>> {
        (self.out.len() >= Self::CHUNK).then(|| self.out.drain(..Self::CHUNK).collect())
    }

    fn drain(&mut self) -> Vec<f32> {
        self.out.drain(..).collect()
    }
}

/// Takes everything captured since the last call once `min_frames` are
/// available, and advances the cursor.
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

fn push_mono<T: Copy>(
    buf: &Arc<Mutex<Vec<f32>>>,
    data: &[T],
    channels: usize,
    conv: impl Fn(T) -> f32,
) {
    let Ok(mut guard) = buf.lock() else { return };
    if channels <= 1 {
        guard.extend(data.iter().copied().map(&conv));
        return;
    }
    for frame in data.chunks_exact(channels) {
        let sum: f32 = frame.iter().copied().map(&conv).sum();
        guard.push(sum / channels as f32);
    }
}

/// Linear interpolation, no anti-aliasing filter. Adequate at 48k -> 16k for
/// speech; if transcripts degrade, a proper low-pass is the first fix.
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

pub fn to_pcm16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

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
            writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}

/// Scales a quiet recording up toward a healthy level.
///
/// A phone held to the ear points away from the PC microphone, so call audio
/// arrives far quieter than dictation -- measured 0.098 peak against 0.26-0.74
/// for the same mic held normally. Transcription returned nothing at all from
/// it. Only genuinely quiet material is touched, and the ceiling on the gain
/// stops near-silent noise being amplified into something the model tries to
/// interpret as speech.
pub fn normalize(samples: &mut [f32]) -> Option<f32> {
    const TARGET: f32 = 0.65;
    const MAX_GAIN: f32 = 8.0;

    let peak = peak(samples);
    if peak < 0.004 || peak >= 0.35 {
        return None;
    }

    let gain = (TARGET / peak).min(MAX_GAIN);
    for s in samples.iter_mut() {
        *s = (*s * gain).clamp(-1.0, 1.0);
    }
    Some(gain)
}

/// Distinguishes "the model heard nothing" from "the mic was muted".
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}
