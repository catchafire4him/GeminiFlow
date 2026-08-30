//! Local control surface, so external hardware can drive the app and see what
//! it is doing.
//!
//! Built for a Stream Deck plugin, but nothing here is Stream Deck specific --
//! it is an HTTP endpoint any local process can speak to.
//!
//! Deliberately hand-rolled on `TcpListener` rather than pulling in a web
//! framework. The surface is four routes and a one-way event stream; a
//! framework would be more dependency than feature.
//!
//! Three properties matter more than the API shape:
//!
//! - **Loopback only.** Bound to 127.0.0.1, so nothing outside this machine
//!   can reach it even briefly.
//! - **Token authenticated.** A random token is generated per install and
//!   written next to the database. Another process running as you can read
//!   that file -- so can it read the database -- but nothing can drive the
//!   microphone by guessing a URL.
//! - **Off by default.** It opens a listening socket, and this app's posture
//!   for anything that opens a surface is opt-in.
//!
//! State goes out as Server-Sent Events rather than a WebSocket: the traffic
//! is one-way, and SSE over a raw socket is a header and `data:` lines, where
//! a WebSocket server would need framing, masking and a handshake.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::engine::AppState;

/// Subscribed event streams. Each entry is one connected client.
static CLIENTS: Mutex<Vec<Sender<String>>> = Mutex::new(Vec::new());

/// Bumped every time a dictation is started through this surface, so a
/// stuck-recording guard only ever stops the dictation it was created for.
static DICTATION_GEN: AtomicU64 = AtomicU64::new(0);

/// How long a dictation started over HTTP may run before it is stopped for
/// you.
///
/// A held key cannot get stuck -- letting go is physical. A remote start can:
/// if the plugin crashes between start and stop, nothing else would ever end
/// the recording, and the first anyone would know is a very large upload.
const MAX_REMOTE_DICTATION: Duration = Duration::from_secs(300);

/// Starts the listener. Returns the port actually bound.
pub fn start(state: Arc<AppState>, preferred_port: u16) -> Result<u16> {
    // Port 0 asks the OS for a free one, which is the fallback when the
    // preferred port is taken. The plugin reads the real port out of
    // control.json rather than assuming.
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, preferred_port)))
        .or_else(|first| {
            crate::logln!(
                "[control] port {preferred_port} unavailable ({first}); asking for any free port"
            );
            TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        })
        .map_err(|e| anyhow!("could not open the control port: {e}"))?;

    let port = listener.local_addr()?.port();
    let token = load_or_create_token()?;
    write_descriptor(port, &token)?;

    crate::logln!("[control] listening on 127.0.0.1:{port}");

    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    let state = Arc::clone(&state);
                    let token = token.clone();
                    // A thread per connection. There is one client in
                    // practice, and an event stream occupies its connection
                    // for as long as the plugin runs, so a pool would buy
                    // nothing.
                    std::thread::spawn(move || {
                        if let Err(e) = handle(stream, &state, &token) {
                            crate::logln!("[control] connection ended: {e}");
                        }
                    });
                }
                Err(e) => crate::logln!("[control] accept failed: {e}"),
            }
        }
    });

    Ok(port)
}

/// Pushes a status change to every connected stream.
///
/// Called from the engine on its own threads, so it must never block: a slow
/// or dead client cannot be allowed to stall a dictation. Sends go to
/// unbounded channels owned by the per-connection threads, and any client
/// whose receiver is gone is dropped here.
pub fn broadcast(payload: &str) {
    let Ok(mut clients) = CLIENTS.lock() else { return };
    clients.retain(|tx| tx.send(payload.to_string()).is_ok());
}

fn handle(mut stream: TcpStream, state: &Arc<AppState>, token: &str) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    // Headers, up to the blank line. Only the authorization header is used,
    // but the body length is needed to leave the socket in a sane state.
    let mut authorization = String::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("authorization:") {
            authorization = value.trim().to_string();
        } else if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length.min(64 * 1024)];
        let _ = reader.read_exact(&mut body);
    }

    let supplied = authorization.strip_prefix("bearer ").unwrap_or("");
    if !constant_time_eq(supplied.as_bytes(), token.as_bytes()) {
        // No detail about why. A caller with the wrong token learns only that
        // it was wrong.
        return respond(&mut stream, 401, "application/json", r#"{"error":"unauthorized"}"#);
    }

    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => respond(
            &mut stream,
            200,
            "application/json",
            &json!({ "app": "GeminiFlow", "version": env!("CARGO_PKG_VERSION") }).to_string(),
        ),

        ("GET", "/state") => {
            let status = state
                .status
                .lock()
                .map(|s| s.clone())
                .unwrap_or_default();
            respond(
                &mut stream,
                200,
                "application/json",
                &serde_json::to_string(&status)?,
            )
        }

        ("GET", "/events") => stream_events(stream, state),

        ("POST", p) if p.starts_with("/action/") => {
            let action = p.trim_start_matches("/action/");
            match run_action(action) {
                Ok(()) => {
                    crate::logln!("[control] action {action}");
                    respond(&mut stream, 200, "application/json", r#"{"ok":true}"#)
                }
                Err(e) => respond(
                    &mut stream,
                    400,
                    "application/json",
                    &json!({ "error": e.to_string() }).to_string(),
                ),
            }
        }

        _ => respond(&mut stream, 404, "application/json", r#"{"error":"no such route"}"#),
    }
}

/// The event stream. Holds the connection open for the life of the client.
fn stream_events(mut stream: TcpStream, state: &Arc<AppState>) -> Result<()> {
    let (tx, rx): (Sender<String>, Receiver<String>) = channel();
    if let Ok(mut clients) = CLIENTS.lock() {
        clients.push(tx);
    }

    stream.write_all(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\n\
          Connection: keep-alive\r\n\r\n",
    )?;

    // The current state first, so a client that connects mid-recording paints
    // the right thing instead of waiting for the next change.
    let status = state.status.lock().map(|s| s.clone()).unwrap_or_default();
    stream.write_all(format!("data: {}\n\n", serde_json::to_string(&status)?).as_bytes())?;
    stream.flush()?;

    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(payload) => {
                stream.write_all(format!("data: {payload}\n\n").as_bytes())?;
                stream.flush()?;
            }
            // A comment line. Nothing consumes it, but writing it is how a
            // client that has gone away is noticed: the write fails and this
            // thread ends instead of lingering forever.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                stream.write_all(b": keepalive\n\n")?;
                stream.flush()?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn run_action(action: &str) -> Result<()> {
    match action {
        "dictation/start" => {
            crate::hotkey::request_press();
            arm_dictation_guard();
            Ok(())
        }
        "dictation/stop" => {
            crate::hotkey::request_release();
            Ok(())
        }
        "notes/toggle" => {
            crate::hotkey::request_notes_toggle();
            Ok(())
        }
        "call/toggle" => {
            crate::hotkey::request_call_toggle();
            Ok(())
        }
        other => Err(anyhow!("unknown action {other}")),
    }
}

/// Ends a remotely started dictation that nobody stopped.
fn arm_dictation_guard() {
    let generation = DICTATION_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(MAX_REMOTE_DICTATION);
        // A newer start means this guard is stale; the newer one owns the
        // session now.
        if DICTATION_GEN.load(Ordering::SeqCst) != generation {
            return;
        }
        crate::logln!(
            "[control] dictation ran past {}s without a stop -- ending it",
            MAX_REMOTE_DICTATION.as_secs()
        );
        // Harmless if it already ended: the engine ignores a release with no
        // session open.
        crate::hotkey::request_release();
    });
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &str) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        _ => "Not Found",
    };
    stream.write_all(
        format!(
            "HTTP/1.1 {status} {reason}\r\n\
             Content-Type: {content_type}\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.flush()?;
    Ok(())
}

/// Compares without returning early on the first differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The token, created on first use and reused thereafter.
///
/// Stable across restarts so the plugin does not have to be reconfigured every
/// time the app starts.
fn load_or_create_token() -> Result<String> {
    let path = crate::store::data_dir()?.join("control-token.txt");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if existing.len() >= 32 {
            return Ok(existing);
        }
    }

    let mut bytes = [0u8; 24];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow!("no randomness available: {e}"))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(&path, &token)?;
    crate::logln!("[control] generated a new access token");
    Ok(token)
}

/// Where the plugin finds the port and token.
fn write_descriptor(port: u16, token: &str) -> Result<()> {
    let path = crate::store::data_dir()?.join("control.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({ "port": port, "token": token }))?,
    )?;
    Ok(())
}
