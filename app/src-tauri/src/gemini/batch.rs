//! Batch transcription via /v1beta/interactions.
//!
//! Measured at ~2.4s, which is why this is the fallback and not the primary
//! dictation path. It is the right tool for notes and calls in M3, where a
//! couple of seconds of processing is invisible.
//!
//! Audio goes inline as base64 rather than through the Files API: that would
//! be a second HTTP round-trip, and inline is supported under 20 MB.

use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::{json, Value};

const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";

pub struct BatchClient {
    http: reqwest::blocking::Client,
}

impl BatchClient {
    pub fn new() -> Result<Self> {
        Ok(BatchClient {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .connect_timeout(Duration::from_secs(10))
                // No connection reuse.
                //
                // Measured: dictations started within a few seconds of the
                // previous one took 20-26s instead of 2-4s, while ones after a
                // longer gap were consistently fast. Google serves this over
                // HTTP/2, so a pooled connection is shared by every request and
                // a degraded one stalls the next request rather than failing.
                // Same failure mode that made connection pre-warming hang for
                // 60s during M0.
                //
                // A fresh connection costs ~150-250ms of TLS handshake, which
                // is a good trade against multi-second stalls. Do NOT also set
                // http1_only(): this endpoint requires HTTP/2 and every request
                // fails outright with a transport error.
                .pool_max_idle_per_host(0)
                .build()?,
        })
    }

    fn send_once(
        &self,
        api_key: &str,
        body: &Value,
    ) -> Result<reqwest::blocking::Response, reqwest::Error> {
        self.http
            .post(ENDPOINT)
            .header("x-goog-api-key", api_key)
            .json(body)
            .send()
    }

    pub fn transcribe(
        &self,
        api_key: &str,
        model: &str,
        wav: &[u8],
        vocabulary: &[String],
        language: &str,
        diarize: bool,
    ) -> Result<String> {
        // Diarization and word timestamps stay off: they buy nothing for
        // single-speaker dictation and drop the audio ceiling from 60 to 30
        // minutes. M3's speakerphone mode will turn them on deliberately.
        // Speaker labels are only worth it when more than one voice is
        // present: turning diarisation on drops the audio ceiling from 60 to
        // 30 minutes.
        // The object form of mode only accepts type "verbatim": smart
        // transcription cannot be combined with diarisation or word
        // timestamps. We were sending {"type":"smart","diarization_mode":..},
        // which is not a combination the API offers.
        //
        // Speaker labels are the whole point of speakerphone mode, so
        // diarisation wins there and those transcripts keep their filler
        // words. Everything else gets smart, which strips "um" and "uh"
        // and false starts.
        let mode = if diarize {
            json!({ "type": "verbatim", "diarization_mode": "speaker" })
        } else {
            json!("smart")
        };

        let body = json!({
            "model": model,
            "input": [{
                "type": "audio",
                "data": base64::engine::general_purpose::STANDARD.encode(wav),
                "mime_type": "audio/wav"
            }],
            "generation_config": {
                "transcription_config": {
                    "language_codes": [language],
                    "custom_vocabulary": vocabulary,
                    "mode": mode
                }
            }
        });

        // Retry once on a transport error (no response at all). Safe to repeat
        // because nothing was processed, and it means a dropped connection
        // costs a moment rather than the whole dictation. HTTP error statuses
        // are NOT retried -- those the server did answer, and repeating them
        // just bills twice.
        let response = match self.send_once(api_key, &body) {
            Ok(response) => response,
            Err(first) => {
                crate::logln!("[batch] request failed ({first}); retrying once");
                std::thread::sleep(Duration::from_millis(400));
                self.send_once(api_key, &body).map_err(|second| {
                    anyhow!("could not reach the transcription service: {second}")
                })?
            }
        };

        let status = response.status();
        let text = response.text()?;

        if !status.is_success() {
            return Err(anyhow!("transcription failed (HTTP {status}): {text}"));
        }

        let value: Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("response was not JSON ({e}): {text}"))?;

        // Empty is legitimate: audio with no speech returns no `steps` key at
        // all rather than an empty transcript.
        Ok(extract_text(&value).unwrap_or_default())
    }
}

/// Verified against live responses. The published docs claim the transcript is
/// at `interaction.output_text`; it is not. The real shape is:
///
/// ```jsonc
/// { "steps": [ { "type": "model_output",
///                "content": [ { "type": "text", "text": "..." } ] } ] }
/// ```
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

    (!out.is_empty()).then_some(out)
}
