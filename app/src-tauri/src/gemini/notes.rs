//! Turns a raw transcript into a structured note.
//!
//! The transcription model only transcribes, so this is a second call to a
//! reasoning model. It uses the Interactions API's `response_format` with a
//! JSON schema, which means the shape is enforced server-side rather than
//! coaxed out of a prompt and parsed hopefully.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::store::NoteDraft;

const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";

/// The default instruction. Stored as a constant for now; the schema and the
/// call site are already separated so it can become an editable template
/// without a migration.
pub const DEFAULT_TEMPLATE: &str = "\
You are turning a spoken working note into something the speaker can act on \
later. The text is a transcript of one person thinking out loud, so it will \
ramble, backtrack and restate things.

Rules:
- Use only what is in the transcript. Never invent details, names, numbers or \
commitments that were not said.
- Write in the speaker's own terms. Do not add advice or commentary.
- `title` is a short noun phrase naming the subject, not a sentence.
- `summary` is two or three sentences on what this note is about.
- `takeaways` are the few points worth remembering. Prefer fewer, sharper ones. \
Empty is fine if nothing stands out.
- `action_items` are only things the speaker said they need to do. Each one \
starts with a verb. If nothing was committed to, return an empty list rather \
than inventing tasks.
- `notable` holds verbatim quotes, numbers, names, code or specifics worth \
keeping exactly as said. Return an empty string if there are none.

Transcript:
";

/// One-sided phone call: the microphone heard only the user's half.
///
/// The failure this prompt exists to prevent: handed half a dialogue, a model
/// will smooth over the gap and reconstruct what the other party "must have"
/// said. A plausible invented commitment is worse than no note at all, because
/// the whole point is to be able to trust it later.
pub const CALL_TEMPLATE: &str = "\
The following is a transcript of ONE SIDE of a phone call -- only the speaker's \
own words were recorded. The other party's speech is NOT present and cannot be \
recovered.

Absolute rules:
- NEVER invent, quote or paraphrase anything the other party said. You did not \
hear them.
- NEVER state a commitment, price, date or fact as agreed unless the speaker \
themselves said it.
- Write from the speaker's perspective, in their own terms.

Fill the fields as follows:
- `title`: short noun phrase naming what the call was about.
- `counterparty`: who they were talking to, ONLY if the speaker named them. \
Empty string otherwise. Never guess from context.
- `summary`: two or three sentences on what this call covered, based only on \
the speaker's half.
- `action_items`: things the speaker said THEY would do. Start each with a \
verb. Empty list if they committed to nothing.
- `takeaways`: facts the speaker stated -- numbers, addresses, dates, prices, \
names. These are the details worth keeping.
- `inferred`: things about the other party's side that the speaker's words make \
unambiguous, such as repeating a date back or confirming a number. Each entry \
must say what it is based on, e.g. \"they proposed Thursday - the speaker \
repeated it back\". Leave empty rather than speculating.
- `open_questions`: points where the speaker clearly responds to something \
unrecoverable, so the user knows what to check against their own memory while \
it is fresh. This is the most useful field on a one-sided call -- do not skip it.
- `notable`: verbatim fragments worth keeping exactly as said.

Transcript (speaker's side only):
";

/// Speakerphone: the mic heard both parties, so it is a normal conversation.
pub const SPEAKERPHONE_TEMPLATE: &str = "\
The following is a transcript of a phone call recorded on speakerphone, so both \
parties are present. Speaker labels may be present.

Rules:
- Use only what is in the transcript. Never invent details or commitments.
- `action_items` are things someone committed to doing; note who if it is clear.
- `counterparty`: the other party's name if stated, else empty.
- `takeaways`: the facts worth keeping -- numbers, dates, decisions.
- `open_questions`: anything left unresolved on the call.
- `inferred`: leave empty; both sides were heard, so nothing needs inferring.
- `notable`: verbatim fragments worth keeping exactly.

Transcript:
";

pub struct NotesClient {
    // Deliberately holds nothing. See `client`.
}

impl NotesClient {
    pub fn new() -> Result<Self> {
        Ok(NotesClient {})
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
    fn client() -> reqwest::Result<reqwest::blocking::Client> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(40))
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(0)
            .build()
    }

    pub fn structure(
        &self,
        api_key: &str,
        model: &str,
        template: &str,
        transcript: &str,
    ) -> Result<NoteDraft> {
        let body = json!({
            "model": model,
            "input": format!("{template}{transcript}"),
            "response_format": {
                "type": "text",
                "mime_type": "application/json",
                "schema": schema(),
            }
        });

        // "High demand" 500s are common on this model and clear quickly, so a
        // couple of spaced retries recover most of them.
        let mut last = String::new();
        let mut response = None;

        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(1500 * attempt));
            }

            let started = std::time::Instant::now();
            let sent = match Self::client() {
                Ok(http) => http
                    .post(ENDPOINT)
                    .header("x-goog-api-key", api_key)
                    .json(&body)
                    .send(),
                Err(e) => Err(e),
            };

            match sent {
                Ok(r) if r.status().is_success() => {
                    crate::logln!(
                        "[notes] summarised in {} ms",
                        started.elapsed().as_millis()
                    );
                    response = Some(r);
                    break;
                }
                Ok(r) => {
                    let status = r.status();
                    last = format!("HTTP {status}: {}", r.text().unwrap_or_default());
                    // Client errors will not fix themselves.
                    if !status.is_server_error() && status.as_u16() != 429 {
                        break;
                    }
                    crate::logln!("[notes] {last} -- retrying ({})", attempt + 1);
                }
                Err(e) => {
                    last = e.to_string();
                    crate::logln!(
                        "[notes] request failed after {} ms ({last}) -- retrying ({})",
                        started.elapsed().as_millis(),
                        attempt + 1
                    );
                }
            }
        }

        let response =
            response.ok_or_else(|| anyhow!("could not summarise the note ({last})"))?;
        let text = response.text()?;

        let value: Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("response was not JSON ({e}): {text}"))?;

        let payload = extract_output(&value).ok_or_else(|| {
            anyhow!(
                "no output in the summarisation response: {}",
                text.chars().take(400).collect::<String>()
            )
        })?;

        serde_json::from_str::<NoteDraft>(&payload).map_err(|e| {
            anyhow!(
                "summary did not match the expected shape ({e}): {}",
                payload.chars().take(400).collect::<String>()
            )
        })
    }
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title":          { "type": "string" },
            "counterparty":   { "type": "string" },
            "summary":        { "type": "string" },
            "takeaways":      { "type": "array", "items": { "type": "string" } },
            "action_items":   { "type": "array", "items": { "type": "string" } },
            "inferred":       { "type": "array", "items": { "type": "string" } },
            "open_questions": { "type": "array", "items": { "type": "string" } },
            "notable":        { "type": "string" }
        },
        "required": ["title", "summary", "takeaways", "action_items", "notable"]
    })
}

/// Structured output is documented to arrive in `output_text`, but the
/// transcription endpoint on the same API puts its text under
/// `steps[].content[].text` instead. Both are tried rather than trusting one.
fn extract_output(v: &Value) -> Option<String> {
    if let Some(text) = v.get("output_text").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            return Some(text.to_string());
        }
    }
    if let Some(text) = v
        .pointer("/interaction/output_text")
        .and_then(Value::as_str)
    {
        if !text.trim().is_empty() {
            return Some(text.to_string());
        }
    }

    let steps = v.get("steps").and_then(Value::as_array)?;
    let mut out = String::new();
    for step in steps {
        let Some(content) = step.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
        }
    }
    (!out.trim().is_empty()).then_some(out)
}
