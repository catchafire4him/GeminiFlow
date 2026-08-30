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
    // Deliberately holds nothing. See `client`.
}

impl BatchClient {
    pub fn new() -> Result<Self> {
        Ok(BatchClient {})
    }

/// A brand new HTTP client for one attempt.
///
/// Not a long-lived client, and not merely an unpooled one. Turning off
/// idle pooling was supposed to stop a bad connection to this host from
/// stalling later requests, and it did not: measured on 29 August, the
/// first request after startup completed in two seconds and every one after
/// it took forty to sixty, on both this client and the transcription one.
/// A plain request to the same host from the same machine at the same time
/// took 75 ms, so neither the network nor the service was at fault.
///
/// Whatever state goes bad lives in the client, so no client outlives the
/// attempt that created it. A fresh connection costs about 75 ms against
/// requests that take seconds, which is a trade worth making twice over.
///
/// Do NOT set http1_only(): this endpoint requires HTTP/2 and every request
/// fails outright with a transport error.
    /// How long one attempt may take, given how much audio it carries.
    ///
    /// This used to be a flat sixty seconds, far longer than any healthy
    /// request, which made a stall enormously expensive: a note that
    /// transcribes in two seconds cost ninety-four when the first attempt
    /// hung, because a full minute went by before anything was retried.
    /// Giving up sooner and starting again is strictly better when the fast
    /// case is this fast.
    ///
    /// It still has to scale: an hour-long note is a genuinely large upload
    /// and deserves longer than a ten-second dictation.
    fn attempt_timeout(wav_bytes: usize) -> Duration {
        let megabytes = wav_bytes as u64 / (1024 * 1024);
        Duration::from_secs((20 + megabytes * 10).min(180))
    }

    fn client(wav_bytes: usize) -> reqwest::Result<reqwest::blocking::Client> {
        reqwest::blocking::Client::builder()
                .timeout(Self::attempt_timeout(wav_bytes))
                .connect_timeout(Duration::from_secs(10))
                // No connection reuse.
                //
                // Correction, 29 August: the comment below claimed Google
                // serves this over HTTP/2 and that a shared connection was
                // the cause. Measured with the same library and settings,
                // every response comes back HTTP/1.1 -- reqwest is built
                // here without its http2 feature, so it was never
                // negotiating HTTP/2 at all. The reasoning was wrong. The
                // setting is kept because fresh connections measure at
                // about 75 ms and nothing depends on reuse, but it is not
                // the fix it was described as, and the stalls it was meant
                // to cure still happen.
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
                .build()
    }

    fn send_once(
        api_key: &str,
        body: &Value,
        wav_bytes: usize,
    ) -> Result<reqwest::blocking::Response, reqwest::Error> {
        Self::client(wav_bytes)?
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
        // Three attempts rather than two, each with its own client and its
        // own connection. Safe to repeat because a request that never
        // returned was never processed; an HTTP error status is a different
        // matter and is not retried here, since the server did answer and
        // asking again just bills twice.
        let mut response = None;
        let mut last = String::new();

        for attempt in 1..=3 {
            let started = std::time::Instant::now();
            match Self::send_once(api_key, &body, wav.len()) {
                Ok(r) => {
                    crate::logln!(
                        "[batch] attempt {attempt} answered in {} ms",
                        started.elapsed().as_millis()
                    );
                    response = Some(r);
                    break;
                }
                Err(e) => {
                    last = e.to_string();
                    crate::logln!(
                        "[batch] attempt {attempt} gave up after {} ms ({last})",
                        started.elapsed().as_millis()
                    );
                    std::thread::sleep(Duration::from_millis(400));
                }
            }
        }

        let response = response.ok_or_else(|| {
            anyhow!("could not reach the transcription service: {last}")
        })?;

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
