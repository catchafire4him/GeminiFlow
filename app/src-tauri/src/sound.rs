//! Short tones confirming that a recording started or stopped.
//!
//! Generated in memory rather than shipped as asset files, and played through
//! `PlaySoundW` with `SND_MEMORY` — no files to install, no dependency on a
//! webview being visible, and no audio-playback crate pulled in for two beeps.
//!
//! Two distinct pitches so start and stop are distinguishable without looking:
//! rising for start, lower for stop. Quiet on purpose; this fires every time
//! you dictate and an obtrusive sound would be worse than none.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

const SAMPLE_RATE: u32 = 22_050;
const AMPLITUDE: f32 = 0.11;

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
}

#[derive(Clone, Copy)]
pub enum Tone {
    /// Recording has begun.
    Start,
    /// Recording has ended.
    Stop,
}

pub fn play(tone: Tone) {
    if !ENABLED.load(Ordering::SeqCst) {
        return;
    }

    // Held in a static because SND_ASYNC returns immediately and Windows reads
    // the buffer while it plays; a local would be freed out from under it.
    let wav = match tone {
        Tone::Start => {
            static START: OnceLock<Vec<u8>> = OnceLock::new();
            START.get_or_init(|| tone_wav(660.0, 880.0, 0.075))
        }
        Tone::Stop => {
            static STOP: OnceLock<Vec<u8>> = OnceLock::new();
            STOP.get_or_init(|| tone_wav(660.0, 494.0, 0.075))
        }
    };

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
/// merely different in pitch. Amplitude is faded in and out over a few
/// milliseconds because an abrupt start or end produces an audible click that
/// sounds like a fault.
fn tone_wav(from_hz: f32, to_hz: f32, seconds: f32) -> Vec<u8> {
    let total = (SAMPLE_RATE as f32 * seconds) as usize;
    let fade = (SAMPLE_RATE as f32 * 0.008) as usize;

    let mut samples = Vec::with_capacity(total);
    let mut phase = 0.0f32;

    for i in 0..total {
        let t = i as f32 / total as f32;
        let hz = from_hz + (to_hz - from_hz) * t;

        // Advancing the phase rather than recomputing sin(2*pi*f*t) keeps the
        // waveform continuous while the frequency changes; the naive form
        // discontinuities and clicks.
        phase += std::f32::consts::TAU * hz / SAMPLE_RATE as f32;

        let envelope = if i < fade {
            i as f32 / fade as f32
        } else if i > total.saturating_sub(fade) {
            (total - i) as f32 / fade as f32
        } else {
            1.0
        };

        samples.push((phase.sin() * AMPLITUDE * envelope * i16::MAX as f32) as i16);
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
