//! Short tones confirming that a recording started or stopped.
//!
//! Generated in memory rather than shipped as asset files, and played through
//! `PlaySoundW` with `SND_MEMORY` — no files to install, no dependency on a
//! webview being visible, and no audio-playback crate pulled in for two beeps.
//!
//! Two distinct pitches so start and stop are distinguishable without looking:
//! rising for start, lower for stop. Quiet on purpose; this fires every time
//! you dictate and an obtrusive sound would be worse than none.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

const SAMPLE_RATE: u32 = 22_050;

/// Peak amplitude at volume 100. Set so the default of 50 lands on the level
/// that was tuned by ear, leaving room to go louder without distorting.
const MAX_AMPLITUDE: f32 = 0.11;

static ENABLED: AtomicBool = AtomicBool::new(true);
static VOLUME: AtomicU32 = AtomicU32::new(50);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
}

pub fn set_volume(percent: i64) {
    VOLUME.store(percent.clamp(0, 100) as u32, Ordering::SeqCst);
}

#[derive(Clone, Copy)]
pub enum Tone {
    /// Recording has begun.
    Start,
    /// Recording has ended.
    Stop,
}

/// Rendered tones, keyed by (tone, volume).
///
/// Leaked deliberately: SND_ASYNC returns immediately and Windows keeps
/// reading the buffer while it plays, so it has to outlive this call. Each is
/// about 4 KB and only the levels actually used are ever built.
static CACHE: Mutex<Option<HashMap<(u8, u32), &'static [u8]>>> = Mutex::new(None);

pub fn play(tone: Tone) {
    if !ENABLED.load(Ordering::SeqCst) {
        return;
    }
    let volume = VOLUME.load(Ordering::SeqCst);
    if volume == 0 {
        return;
    }

    let (key, from_hz, to_hz) = match tone {
        // C5 -> E5. Lower and closer together than a bright beep; the interval
        // still reads as "up" without being piercing.
        Tone::Start => (0u8, 523.25, 659.25),
        // C5 -> G4, resolving downward.
        Tone::Stop => (1u8, 523.25, 392.0),
    };

    let Ok(mut guard) = CACHE.lock() else { return };
    let cache = guard.get_or_insert_with(HashMap::new);
    let wav = *cache.entry((key, volume)).or_insert_with(|| {
        let amplitude = MAX_AMPLITUDE * (volume as f32 / 100.0);
        Box::leak(tone_wav(from_hz, to_hz, 0.09, amplitude).into_boxed_slice())
    });
    drop(guard);

    unsafe {
        let _ = PlaySoundW(
            PCWSTR(wav.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

/// A WAV of a tone sliding from `from_hz` to `to_hz`.
///
/// The pitch slide is what makes the two cues obviously different rather than
/// merely different in pitch.
///
/// The envelope is a raised cosine across the whole tone rather than a flat
/// body with short fades. That turns it into a swell that rises and falls,
/// which reads as much softer than the same amplitude held steady - there is
/// no point at which it is simply "on".
fn tone_wav(from_hz: f32, to_hz: f32, seconds: f32, amplitude: f32) -> Vec<u8> {
    let total = (SAMPLE_RATE as f32 * seconds) as usize;

    let mut samples = Vec::with_capacity(total);
    let mut phase = 0.0f32;

    for i in 0..total {
        let t = i as f32 / total as f32;
        let hz = from_hz + (to_hz - from_hz) * t;

        // Advancing the phase rather than recomputing sin(2*pi*f*t) keeps the
        // waveform continuous while the frequency changes; the naive form
        // discontinuities and clicks.
        phase += std::f32::consts::TAU * hz / SAMPLE_RATE as f32;

        // Hann window: silent at both ends, peaking in the middle.
        let envelope = 0.5 * (1.0 - (std::f32::consts::TAU * i as f32 / total as f32).cos());

        samples.push((phase.sin() * amplitude * envelope * i16::MAX as f32) as i16);
    }

    encode_wav(&samples)
}

fn encode_wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM header size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
