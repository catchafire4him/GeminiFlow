//! Live streaming transcription over WebSocket. This is the primary dictation
//! path: audio streams while the user speaks, so the only latency left after
//! they release the key is finalization -- measured at 374 ms, versus ~2.4 s
//! for the batch call.

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

/// How long to wait for a final before falling back to the last interim
/// hypothesis. Successful finalisation is consistently under a second, so a
/// long wait here buys nothing and just makes a failure feel broken.
const FINALIZE_TIMEOUT: Duration = Duration::from_millis(3500);

/// How long to keep listening after a final arrives, in case more follow.
///
/// The server flushes buffered speech as several finals once the stream
/// ends. Breaking on the first one drops the rest, which is one of the ways
/// a transcript ends mid-sentence. turnComplete normally arrives well inside
/// this window, so it rarely costs anything.
const FINAL_GRACE: Duration = Duration::from_millis(300);

pub enum LiveMsg {
    /// 16 kHz mono PCM16.
    Audio(Vec<i16>),
    /// The user released the key.
    End,
}

#[derive(Debug, Default)]
pub struct LiveResult {
    pub transcript: String,
    /// Milliseconds from audioStreamEnd to the final transcript.
    pub finalize_ms: u128,
    /// True when no final arrived and the interim hypothesis was used, which
    /// is slightly less accurate than a proper final.
    pub from_partial: bool,
}

pub struct LiveSession {
    tx: UnboundedSender<LiveMsg>,
    result: Receiver<Result<LiveResult>>,
}

impl LiveSession {
    /// Cloneable audio sink, handed to the recorder as its tap.
    pub fn sender(&self) -> UnboundedSender<LiveMsg> {
        self.tx.clone()
    }

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
/// keeping the rest of the engine blocking.
pub fn start(
    api_key: String,
    model: String,
    vocabulary: Vec<String>,
    language: String,
    on_partial: impl Fn(String) + Send + 'static,
) -> LiveSession {
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
        let outcome =
            runtime.block_on(session(api_key, model, vocabulary, language, rx, on_partial));
        let _ = result_tx.send(outcome);
    });

    LiveSession {
        tx,
        result: result_rx,
    }
}

async fn session(
    api_key: String,
    model: String,
    vocabulary: Vec<String>,
    language: String,
    mut rx: UnboundedReceiver<LiveMsg>,
    on_partial: impl Fn(String) + Send + 'static,
) -> Result<LiveResult> {
    let url = format!("{WS_HOST}?key={api_key}");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| anyhow!("could not open the transcription stream: {e}"))?;

    let (mut write, mut read) = ws.split();

    // The model id needs the `models/` prefix here, unlike the batch endpoint.
    let setup = json!({
        "setup": {
            "model": format!("models/{model}"),
            "generationConfig": { "responseModalities": ["TEXT"] },
            "inputAudioTranscription": {
                "languageCodes": [language],
                "customVocabulary": vocabulary,
                "mode": "SMART"
            },
            // Push-to-talk, which is exactly what a hold-to-dictate key is.
            //
            // With the default automatic voice-activity detection the
            // server decides when a turn ends, and it fires on ordinary
            // pauses mid-sentence. Every turn boundary resets the interim
            // hypothesis, so speech before it is lost unless a final
            // happened to cover it -- the observed "cuts off partway
            // through". The key press and release are unambiguous activity
            // boundaries, so we send them ourselves and let pauses stay
            // inside the turn.
            "realtimeInputConfig": {
                "automaticActivityDetection": { "disabled": true },
                "turnCoverage": "TURN_INCLUDES_ALL_INPUT"
            }
        }
    });
    write.send(Message::Text(setup.to_string())).await?;

    let mut result = LiveResult::default();
    let mut end_sent: Option<Instant> = None;
    // When the most recent final arrived, once the release edge has passed.
    let mut last_final_at: Option<Instant> = None;
    let mut chunks_sent = 0usize;
    // The best interim hypothesis seen. If the server never sends a final,
    // this is far better than reporting failure for speech we did transcribe.
    let mut last_partial = String::new();
    // The server ends turns on its own mid-dictation, and each new turn resets
    // the interim text. Keeping only the latest one loses everything spoken
    // before the last turn boundary, which shows up as a transcript that cuts
    // off partway through. Completed segments are banked here.
    let mut banked_interim = String::new();
    // Audio sent before the server acknowledges setup appears to be discarded:
    // failing sessions show setupComplete arriving and then nothing at all, no
    // interim text, no output. Hold chunks until the session is confirmed.
    let mut setup_done = false;
    let mut pending_audio: Vec<Vec<i16>> = Vec::new();

    loop {
        // Once a final has arrived the wait shortens to the grace window;
        // until then the full finalisation timeout applies.
        let deadline = match (end_sent, last_final_at) {
            (Some(_), Some(seen)) => Some(seen + FINAL_GRACE),
            (Some(sent), None) => Some(sent + FINALIZE_TIMEOUT),
            _ => None,
        };

        // Not `biased`: polling the socket first every iteration lets a chatty
        // server starve the audio-send branch on long dictations.
        tokio::select! {
            incoming = read.next() => {
                let Some(message) = incoming else { break };
                let message = message.map_err(|e| anyhow!("stream read failed: {e}"))?;

                // The Live API replies with BINARY frames containing JSON, not
                // text frames. Handling only Text silently discards every
                // message including setupComplete, which is indistinguishable
                // from a server that never answers.
                let text = match message {
                    Message::Text(t) => t,
                    Message::Binary(bytes) => match String::from_utf8(bytes) {
                        Ok(t) => t,
                        Err(_) => continue,
                    },
                    Message::Close(frame) => {
                        return Err(anyhow!(
                            "transcription stream closed: {}",
                            frame
                                .map(|f| format!("{} {}", f.code, f.reason))
                                .unwrap_or_else(|| "no reason given".into())
                        ));
                    }
                    _ => continue,
                };

                let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };

                // Log everything except partials, which are frequent and
                // uninteresting. This is how we find out what the server
                // actually sends when a session fails to finalise.
                if crate::logging::debug_enabled()
                    && value
                        .pointer("/serverContent/interimInputTranscription")
                        .is_none()
                {
                    let compact: String = text.chars().take(300).collect();
                    crate::logln!("[live] <- {compact}");
                }

                if !setup_done && value.get("setupComplete").is_some() {
                    setup_done = true;

                    // Required now that automatic detection is off: with no
                    // explicit start the server treats the audio as
                    // inactivity and transcribes nothing.
                    write
                        .send(Message::Text(
                            json!({ "realtimeInput": { "activityStart": {} } }).to_string(),
                        ))
                        .await?;

                    if !pending_audio.is_empty() {
                        crate::logln!(
                            "[live] setup acknowledged; flushing {} buffered chunks",
                            pending_audio.len()
                        );
                        for pcm in pending_audio.drain(..) {
                            chunks_sent += 1;
                            write.send(Message::Text(audio_message(&pcm))).await?;
                        }
                    }
                }

                if let Some(partial) = value
                    .pointer("/serverContent/interimInputTranscription/text")
                    .and_then(Value::as_str)
                {
                    if !partial.trim().is_empty() {
                        last_partial = partial.to_string();
                    }
                    on_partial(partial.to_string());
                }

                if let Some(final_text) = value
                    .pointer("/serverContent/inputTranscription/text")
                    .and_then(Value::as_str)
                {
                    append_segment(&mut result.transcript, final_text);

                    // Deliberately no break. More finals usually follow the
                    // release edge, and taking only the first truncates the
                    // transcript. The turn ends on turnComplete, or on the
                    // grace window expiring.
                    if let Some(sent) = end_sent {
                        result.finalize_ms = sent.elapsed().as_millis();
                        last_final_at = Some(Instant::now());
                    }
                }

                // The server may consider the turn finished without sending a
                // further transcript -- typical when the last segment was
                // already finalised during a pause before the key was
                // released. Without this the session waits for a final that
                // will never arrive.
                let turn_done = ["turnComplete", "generationComplete"].iter().any(|k| {
                    value
                        .pointer(&format!("/serverContent/{k}"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                });

                if turn_done {
                    if let Some(sent) = end_sent {
                        result.finalize_ms = sent.elapsed().as_millis();
                        break;
                    }
                    // The server ended the turn on its own, before the user let
                    // go -- its silence detection fired mid-dictation. Bank the
                    // interim text now, because the next turn resets it.
                    if !last_partial.trim().is_empty() {
                        append_segment(&mut banked_interim, &last_partial);
                        last_partial.clear();
                    }
                    crate::logln!(
                        "[live] server ended the turn early after {chunks_sent} chunks \
                         ({} chars banked)",
                        banked_interim.trim().len()
                    );
                }
            }

            outgoing = rx.recv(), if end_sent.is_none() => {
                match outgoing {
                    Some(LiveMsg::Audio(pcm)) => {
                        if setup_done {
                            chunks_sent += 1;
                            write.send(Message::Text(audio_message(&pcm))).await?;
                        } else if pending_audio.len() < 200 {
                            // ~20s of buffer. Beyond that the session is not
                            // coming up and dropping audio beats unbounded growth.
                            pending_audio.push(pcm);
                        }
                    }
                    Some(LiveMsg::End) => {
                        if !pending_audio.is_empty() {
                            crate::logln!(
                                "[live] releasing with {} chunks still unsent -- setup was \
                                 never acknowledged",
                                pending_audio.len()
                            );
                        }
                        end_sent = Some(Instant::now());
                        // activityEnd closes the turn opened at setup;
                        // audioStreamEnd then says no more audio is coming.
                        // Both, in that order.
                        write
                            .send(Message::Text(
                                json!({ "realtimeInput": { "activityEnd": {} } }).to_string(),
                            ))
                            .await?;
                        write
                            .send(Message::Text(
                                json!({ "realtimeInput": { "audioStreamEnd": true } })
                                    .to_string(),
                            ))
                            .await?;
                    }
                    None => break,
                }
            }

            _ = sleep_until(deadline) => {
                // Grace window expired after at least one final. An
                // ordinary finish, not a failure -- the server simply did
                // not bother with a closing turnComplete.
                if last_final_at.is_some() {
                    break;
                }
                crate::logln!(
                    "[live] no closing signal within {}s ({chunks_sent} chunks sent, \
                     {} chars finalised, {} chars interim)",
                    FINALIZE_TIMEOUT.as_secs(),
                    result.transcript.trim().len(),
                    last_partial.trim().len()
                );
                if let Some(sent) = end_sent {
                    result.finalize_ms = sent.elapsed().as_millis();
                }
                break;
            }
        }
    }

    let _ = write.send(Message::Close(None)).await;
    result.transcript = result.transcript.trim().to_string();

    // Fall back to the interim hypothesis, banked segments included. The user
    // watched this text appear live, so reporting "no transcript" while
    // holding it would be both wrong and baffling.
    let mut interim = banked_interim.clone();
    append_segment(&mut interim, &last_partial);
    let interim = interim.trim().to_string();

    if result.transcript.is_empty() && !interim.is_empty() {
        crate::logln!(
            "[live] no final arrived; using {} chars of interim text",
            interim.len()
        );
        result.transcript = interim;
        result.from_partial = true;
    } else if !interim.is_empty() && interim.len() > result.transcript.len() * 2 {
        // Finals covered far less than we heard, so turn boundaries ate
        // segments. Worth seeing in the log if truncation is reported again.
        crate::logln!(
            "[live] WARNING finals gave {} chars but interim held {} -- possible truncation",
            result.transcript.len(),
            interim.len()
        );
    }

    if result.transcript.is_empty() {
        return Err(anyhow!(
            "nothing was transcribed ({chunks_sent} audio chunks sent)"
        ));
    }

    Ok(result)
}

/// Joins finalized segments into one transcript.
///
/// Segments arrive already punctuated but without surrounding whitespace, so
/// naive concatenation produces "on the side.I also noticed". Each segment is
/// trimmed and separated explicitly.
fn append_segment(acc: &mut String, segment: &str) {
    let segment = segment.trim();
    if segment.is_empty() {
        return;
    }

    // Defensive: if the server ever sends cumulative rather than incremental
    // finals, appending would duplicate the whole transcript.
    if acc.trim_end().ends_with(segment) {
        return;
    }
    if segment.starts_with(acc.trim()) && !acc.is_empty() {
        *acc = segment.to_string();
        return;
    }

    if !acc.is_empty() && !acc.ends_with(char::is_whitespace) {
        acc.push(' ');
    }
    acc.push_str(segment);
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

/// Sleeps until `deadline`, or forever when there is no deadline yet.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
        None => std::future::pending::<()>().await,
    }
}
