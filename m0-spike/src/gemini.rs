//! Batch transcription against the /v1beta/interactions endpoint.
//!
//! Audio goes inline as base64 rather than through the Files API. The docs
//! demonstrate the Files API path, but that is a second HTTP round-trip and
//! this whole spike exists to measure the critical path -- inline is supported
//! for requests under 20 MB, which covers any realistic dictation.

use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::{json, Value};

const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";

pub struct Client {
    http: reqwest::blocking::Client,
    api_key: String,
    model: String,
    vocabulary: Vec<String>,
}

impl Client {
    pub fn new(api_key: String, model: String, vocabulary: Vec<String>) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(45))
            // Separate connect timeout so a hang during TLS setup is
            // distinguishable from the server being slow to answer.
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(300))
            .build()?;
        Ok(Client {
            http,
            api_key,
            model,
            vocabulary,
        })
    }

    /// Establishes the TLS connection ahead of time so the handshake is not
    /// sitting in the critical path after the user releases the key. Called on
    /// the press edge, while they are still talking.
    ///
    /// DEFAULT OFF -- measured to be actively harmful. Google serves this over
    /// HTTP/2, so every request multiplexes onto a single connection; when the
    /// prewarm GET stalled it poisoned that connection and the following POST
    /// hung until the client timeout instead of taking ~2.4s.
    ///
    /// Kept behind `M0_PREWARM=1` only so the TLS-handshake saving can be
    /// measured. If it is ever re-enabled for real it needs its own connection
    /// pool, or HTTP/1.1, so a bad warm cannot take the real request with it.
    pub fn prewarm(&self) {
        if std::env::var("M0_PREWARM").is_err() {
            return;
        }
        let started = std::time::Instant::now();
        let result = self
            .http
            .get("https://generativelanguage.googleapis.com/v1beta/models")
            .header("x-goog-api-key", &self.api_key)
            .timeout(Duration::from_secs(5))
            .send();
        match result {
            Ok(r) => eprintln!(
                "  [prewarm] {} in {} ms",
                r.status(),
                started.elapsed().as_millis()
            ),
            Err(e) => eprintln!(
                "  [prewarm] FAILED in {} ms: {e} -- try M0_NO_PREWARM=1",
                started.elapsed().as_millis()
            ),
        }
    }

    /// Posts a one-second synthetic tone to prove the request shape is valid
    /// and to capture the real response body. Isolates "is my JSON right" from
    /// "is my audio right".
    pub fn self_test(&self) -> Result<()> {
        let samples: Vec<f32> = (0..crate::audio::TARGET_RATE)
            .map(|i| {
                let t = i as f32 / crate::audio::TARGET_RATE as f32;
                (t * 440.0 * std::f32::consts::TAU).sin() * 0.3
            })
            .collect();
        let wav = crate::audio::to_wav(&samples)?;

        println!("self-test: posting 1s synthetic tone ({} KB wav)", wav.len() / 1024);
        let started = std::time::Instant::now();
        let (status, body) = self.post_raw(&wav)?;
        println!("self-test: HTTP {status} in {} ms", started.elapsed().as_millis());
        println!("--- raw response ---");
        println!("{body}");
        println!("--- end ---");
        Ok(())
    }

    fn post_raw(&self, wav: &[u8]) -> Result<(reqwest::StatusCode, String)> {
        let body = self.request_body(wav);
        let resp = self
            .http
            .post(ENDPOINT)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()?;
        let status = resp.status();
        Ok((status, resp.text()?))
    }

    fn request_body(&self, wav: &[u8]) -> Value {
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(wav);
        // Diarization and word timestamps are deliberately OFF: they buy
        // nothing for single-speaker dictation and would drop the audio
        // ceiling from 60 to 30 minutes.
        json!({
            "model": self.model,
            "input": [{
                "type": "audio",
                "data": audio_b64,
                "mime_type": "audio/wav"
            }],
            "generation_config": {
                "transcription_config": {
                    "language_codes": ["en-US"],
                    "custom_vocabulary": self.vocabulary,
                    "mode": "smart"
                }
            }
        })
    }

    pub fn transcribe(&self, wav: &[u8]) -> Result<String> {
        let payload_kb = (wav.len() as f64 * 4.0 / 3.0) / 1024.0; // base64 inflates ~4/3
        eprintln!("  [api] posting ~{payload_kb:.0} KB base64 audio");

        let started = std::time::Instant::now();
        let (status, text) = self.post_raw(wav).map_err(|e| {
            anyhow!(
                "request failed after {} ms: {e}",
                started.elapsed().as_millis()
            )
        })?;
        eprintln!(
            "  [api] HTTP {status} after {} ms",
            started.elapsed().as_millis()
        );

        if !status.is_success() {
            return Err(anyhow!("HTTP {status}: {text}"));
        }

        let value: Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("response was not JSON ({e}): {text}"))?;

        extract_text(&value).ok_or_else(|| {
            anyhow!(
                "could not find transcript in response. Raw body below -- if the \
                 shape differs from the documented `interaction.output_text`, fix \
                 extract_text() to match:\n{}",
                serde_json::to_string_pretty(&value).unwrap_or(text)
            )
        })
    }
}

/// VERIFIED against a live response. The docs summary claimed the transcript
/// sits at `interaction.output_text`; it does not. The real shape is:
///
/// ```jsonc
/// { "status": "completed",
///   "steps": [ { "type": "model_output",
///                "content": [ { "type": "text", "text": "Testing 1 2 3." } ] } ] }
/// ```
///
/// Note that a clip with no speech in it returns no `steps` key at all rather
/// than an empty transcript -- so "no steps" means silence, not an error.
fn extract_text(v: &Value) -> Option<String> {
    let mut out = String::new();

    if let Some(steps) = v.get("steps").and_then(Value::as_array) {
        for step in steps {
            let Some(content) = step.get("content").and_then(Value::as_array) else {
                continue;
            };
            for part in content {
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(text);
                }
            }
        }
    }

    // Fallback to the documented path in case it appears on other model paths.
    if out.is_empty() {
        for path in [["interaction", "output_text"], ["output", "text"]] {
            if let Some(s) = path
                .iter()
                .try_fold(v, |cur, key| cur.get(*key))
                .and_then(Value::as_str)
            {
                out = s.trim().to_string();
                if !out.is_empty() {
                    break;
                }
            }
        }
    }

    (!out.is_empty()).then_some(out)
}

/// Seed terms so the spike can prove vocabulary biasing actually changes the
/// output. Say "use callback in Tauri with WASAPI" and check the spelling.
pub fn default_vocabulary() -> Vec<String> {
    [
        "Tauri", "WASAPI", "cpal", "useCallback", "useEffect", "TypeScript",
        "Rust", "cargo", "async", "await", "struct", "enum", "SQLite",
        "GeminiFlow", "Gemini", "webhook", "middleware", "TSX", "npm",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
