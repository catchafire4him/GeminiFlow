//! M0.5: the measurement that decides whether dictation is viable.
//!
//! M0 established that the batch call costs ~2.4s, which is far too slow to
//! sit between releasing the key and seeing text. The live model transcribes
//! *while* you speak, so the only latency that should remain after release is
//! finalization -- the gap between your last word and the final transcript.
//!
//! This module exists to put a number on that gap. Everything else here is
//! scaffolding.

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::Message;

const WS_HOST: &str = "wss://generativelanguage.googleapis.com/ws/\
                       google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

/// Give up rather than hang if the server never sends a final.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(15);

pub enum LiveMsg {
    /// 16 kHz mono PCM16.
    Audio(Vec<i16>),
    /// User released the key.
    End,
}

#[derive(Debug, Default)]
pub struct LiveResult {
    pub transcript: String,
    /// THE NUMBER: milliseconds from audioStreamEnd to the final transcript.
    pub finalize_ms: u128,
    /// How long into the utterance the first partial arrived -- shows whether
    /// the overlay would have anything to display while speaking.
    pub first_partial_ms: Option<u128>,
    pub partial_count: usize,
}

pub struct LiveHandle {
    tx: UnboundedSender<LiveMsg>,
    result: Receiver<Result<LiveResult>>,
}

impl LiveHandle {
    /// Cloneable audio sink, handed to the recorder as its tap.
    pub fn sender(&self) -> UnboundedSender<LiveMsg> {
        self.tx.clone()
    }

    /// Signals end of speech. Returns once the server delivers the final
    /// transcript, or the timeout fires.
    pub fn finish(self) -> Result<LiveResult> {
        self.tx
            .send(LiveMsg::End)
            .map_err(|_| anyhow!("live session already closed"))?;
        self.result
            .recv()
            .map_err(|_| anyhow!("live session thread died"))?
    }
}

/// Opens the socket on its own thread with its own single-threaded runtime,
/// so the rest of the spike stays blocking.
pub fn start(api_key: String, model: String, vocabulary: Vec<String>) -> LiveHandle {
    let (tx, rx) = unbounded_channel::<LiveMsg>();
    let (result_tx, result_rx) = channel::<Result<LiveResult>>();

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                let _ = result_tx.send(Err(anyhow!("could not build runtime: {e}")));
                return;
            }
        };
        let outcome = runtime.block_on(session(api_key, model, vocabulary, rx));
        let _ = result_tx.send(outcome);
    });

    LiveHandle {
        tx,
        result: result_rx,
    }
}

fn setup_message(model: &str, vocabulary: &[String]) -> String {
    // Model name needs the `models/` prefix here, unlike the batch endpoint.
    json!({
        "setup": {
            "model": format!("models/{model}"),
            "generationConfig": { "responseModalities": ["TEXT"] },
            "inputAudioTranscription": {
                "languageCodes": ["en-US"],
                "customVocabulary": vocabulary,
                "mode": "SMART"
            }
        }
    })
    .to_string()
}

fn audio_message(pcm: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for sample in pcm {
        bytes.extend_from_slice(&sample.to_le_bytes()); // little-endian, per docs
    }
    json!({
        "realtimeInput": {
            "audio": {
                "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                "mimeType": "audio/pcm;rate=16000"
            }
        }
    })
    .to_string()
}

async fn session(
    api_key: String,
    model: String,
    vocabulary: Vec<String>,
    mut rx: UnboundedReceiver<LiveMsg>,
) -> Result<LiveResult> {
    let verbose = std::env::var("M0_LIVE_VERBOSE").is_ok();
    let url = format!("{WS_HOST}?key={api_key}");

    let connect_started = Instant::now();
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| anyhow!("websocket connect failed: {e}"))?;
    eprintln!(
        "  [live] connected in {} ms",
        connect_started.elapsed().as_millis()
    );

    let (mut write, mut read) = ws.split();
    write
        .send(Message::Text(setup_message(&model, &vocabulary)))
        .await?;

    let mut result = LiveResult::default();
    let speech_started = Instant::now();
    let mut end_sent: Option<Instant> = None;
    let mut chunks_sent = 0usize;
    let mut frames_sent = 0usize;

    loop {
        // Once the user has stopped talking, stop waiting forever for a final.
        let deadline = end_sent.map(|t| t + FINALIZE_TIMEOUT);

        tokio::select! {
            biased;

            incoming = read.next() => {
                let Some(message) = incoming else {
                    break; // socket closed
                };
                let message = message.map_err(|e| anyhow!("websocket read failed: {e}"))?;

                // The Live API replies with BINARY frames containing JSON, not
                // text frames. Handling only Text silently discards every
                // message including setupComplete -- which looks exactly like
                // a server that never answers.
                let text = match message {
                    Message::Text(t) => t,
                    Message::Binary(bytes) => match String::from_utf8(bytes) {
                        Ok(t) => t,
                        Err(_) => {
                            if verbose {
                                eprintln!("  [live] <- (non-utf8 binary frame)");
                            }
                            continue;
                        }
                    },
                    Message::Close(frame) => {
                        return Err(anyhow!(
                            "server closed the connection: {}",
                            frame
                                .map(|f| format!("{} {}", f.code, f.reason))
                                .unwrap_or_else(|| "no reason given".to_string())
                        ));
                    }
                    other => {
                        if verbose {
                            eprintln!("  [live] <- (ignored {other:?})");
                        }
                        continue;
                    }
                };

                if verbose {
                    eprintln!("  [live] <- {text}");
                }

                let value: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("  [live] unparseable frame ({e}): {text}");
                        continue;
                    }
                };

                if let Some(partial) = value
                    .pointer("/serverContent/interimInputTranscription/text")
                    .and_then(Value::as_str)
                {
                    result.partial_count += 1;
                    if result.first_partial_ms.is_none() {
                        result.first_partial_ms =
                            Some(speech_started.elapsed().as_millis());
                    }
                    if !partial.trim().is_empty() {
                        eprintln!("  [live] ... {}", partial.trim());
                    }
                }

                if let Some(final_text) = value
                    .pointer("/serverContent/inputTranscription/text")
                    .and_then(Value::as_str)
                {
                    if !result.transcript.is_empty() && !final_text.starts_with(' ') {
                        result.transcript.push(' ');
                    }
                    result.transcript.push_str(final_text);

                    // Only stop once the user has actually finished speaking --
                    // finals also arrive mid-utterance at natural pauses.
                    if let Some(sent) = end_sent {
                        result.finalize_ms = sent.elapsed().as_millis();
                        break;
                    }
                }

                let turn_done = value
                    .pointer("/serverContent/turnComplete")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || value
                        .pointer("/serverContent/generationComplete")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);

                if turn_done && end_sent.is_some() {
                    if let Some(sent) = end_sent {
                        result.finalize_ms = sent.elapsed().as_millis();
                    }
                    break;
                }
            }

            outgoing = rx.recv(), if end_sent.is_none() => {
                match outgoing {
                    Some(LiveMsg::Audio(pcm)) => {
                        chunks_sent += 1;
                        frames_sent += pcm.len();
                        write.send(Message::Text(audio_message(&pcm))).await?;
                    }
                    Some(LiveMsg::End) => {
                        end_sent = Some(Instant::now());
                        write
                            .send(Message::Text(
                                json!({ "realtimeInput": { "audioStreamEnd": true } })
                                    .to_string(),
                            ))
                            .await?;
                        eprintln!(
                            "  [live] -> audioStreamEnd after {chunks_sent} chunks \
                             ({:.2}s of audio)",
                            frames_sent as f32 / 16_000.0
                        );
                    }
                    None => break,
                }
            }

            _ = sleep_until(deadline) => {
                return Err(anyhow!(
                    "no final transcript within {}s of audioStreamEnd. \
                     Sent {chunks_sent} audio chunks ({:.2}s); saw {} partials. \
                     If chunks is 0 the tap is broken, not the server. \
                     Re-run with M0_LIVE_VERBOSE=1 to dump raw frames.",
                    FINALIZE_TIMEOUT.as_secs(),
                    frames_sent as f32 / 16_000.0,
                    result.partial_count
                ));
            }
        }
    }

    let _ = write.send(Message::Close(None)).await;
    result.transcript = result.transcript.trim().to_string();
    Ok(result)
}

/// Sleeps until `deadline`, or forever when there is no deadline yet.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
        None => std::future::pending::<()>().await,
    }
}
