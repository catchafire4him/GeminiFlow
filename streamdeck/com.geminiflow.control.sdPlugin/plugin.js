// Bridges two connections: the socket Stream Deck opens for its plugins, and
// GeminiFlow's local control endpoint.
//
// Presses go one way, state comes back the other, and every button is redrawn
// from whatever the app last reported. That is the whole point of the plugin.
// A plain Stream Deck hotkey can start a note, but it cannot know the note
// stopped by itself at the time limit, so the button carries on claiming to
// record. Here the app is the only source of truth, and a button cannot be
// wrong for longer than a message takes to arrive.
//
// Plain JavaScript with one dependency and no build step, so it can be read
// and changed in place.

const WebSocket = require("ws");
const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

// Stream Deck captures stdout into its own per-plugin log, which is the only
// practical way to see what a plugin is doing.
const log = (...parts) => console.log(new Date().toISOString(), ...parts);

// ------------------------------------------------- GeminiFlow control link

const DESCRIPTOR = path.join(process.env.APPDATA || "", "GeminiFlow", "control.json");

/// Port and token, written by the app each time it starts.
///
/// Read on every use rather than cached: the port can change if the preferred
/// one was taken, and the app may not have been running when the plugin
/// started.
function descriptor() {
  try {
    const raw = JSON.parse(fs.readFileSync(DESCRIPTOR, "utf8"));
    if (!raw.port || !raw.token) return null;
    return raw;
  } catch {
    return null;
  }
}

function request(method, route, onBody) {
  const desc = descriptor();
  if (!desc) {
    log("GeminiFlow is not reachable: no control.json. Is external control on?");
    return null;
  }

  const req = http.request(
    {
      host: "127.0.0.1",
      port: desc.port,
      path: route,
      method,
      headers: { Authorization: `Bearer ${desc.token}` },
    },
    (res) => {
      if (!onBody) return res.resume();
      let body = "";
      res.setEncoding("utf8");
      res.on("data", (c) => (body += c));
      res.on("end", () => {
        try {
          onBody(JSON.parse(body));
        } catch {
          // A reply we cannot read is not worth crashing over.
        }
      });
    }
  );
  req.on("error", (e) => log(`request to ${route} failed:`, e.message));
  req.end();
  return req;
}

/// Holds the state stream open, reconnecting whenever it drops.
///
/// The app coming and going is ordinary -- it restarts, and it is often not
/// running yet when Stream Deck starts the plugin -- so a lost connection is
/// normal operation rather than an error worth surfacing.
function followState(onState) {
  let backoff = 1000;
  let retrying = false;

  const retry = () => {
    if (retrying) return;
    retrying = true;
    onState({ state: "offline", connected: false });
    setTimeout(() => {
      retrying = false;
      connect();
    }, backoff);
    // Back off to half a minute so a stopped app is not polled hard forever.
    backoff = Math.min(backoff * 2, 30000);
  };

  const connect = () => {
    const desc = descriptor();
    if (!desc) return retry();

    const req = http.request(
      {
        host: "127.0.0.1",
        port: desc.port,
        path: "/events",
        method: "GET",
        headers: { Authorization: `Bearer ${desc.token}` },
      },
      (res) => {
        if (res.statusCode !== 200) {
          res.resume();
          return retry();
        }
        backoff = 1000;

        let buffer = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => {
          buffer += chunk;
          // Events are separated by a blank line; whatever follows the last
          // one is a partial event and stays in the buffer.
          const events = buffer.split("\n\n");
          buffer = events.pop();
          for (const event of events) {
            for (const line of event.split("\n")) {
              if (!line.startsWith("data:")) continue; // keepalive comments
              try {
                onState({ ...JSON.parse(line.slice(5).trim()), connected: true });
              } catch {
                // A malformed line is not worth dropping the connection for.
              }
            }
          }
        });
        res.on("end", retry);
        res.on("error", retry);
      }
    );
    req.on("error", retry);
    req.end();
  };

  connect();
}

// --------------------------------------------------------------- artwork
//
// Movement is drawn frame by frame from here. Animation written inside a
// drawing does not play -- Stream Deck renders one still frame of it -- and
// animated image files are refused by the plugin interface outright. Sending a
// fresh picture on a timer is what is left, so every drawing takes a phase
// between 0 and 1 and the ticker in main() advances it.

/// Glyphs drawn on a 24-unit grid and scaled into place.
const GLYPHS = {
  page:
    "M6 2h7l5 5v13a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2zm7 1.5V8h4.5" +
    "M7.5 12h9M7.5 15h9M7.5 18h6",
  mic:
    "M12 14a3 3 0 0 0 3-3V5a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3z" +
    "M17 11a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.9V21h2v-3.1A7 7 0 0 0 19 11h-2z",
  phone:
    "M6.6 10.8a15.1 15.1 0 0 0 6.6 6.6l2.2-2.2c.3-.3.7-.4 1-.2 1.1.4 2.3.6 3.6.6" +
    ".6 0 1 .4 1 1V20c0 .6-.4 1-1 1A17 17 0 0 1 3 4c0-.6.4-1 1-1h3.5c.6 0 1 .4 1 1" +
    " 0 1.3.2 2.5.6 3.6.1.3 0 .7-.2 1l-2.3 2.2z",
};

function glyph(name, colour, { x = 48, y = 26, size = 48, stroke = false } = {}) {
  const scale = (size / 24).toFixed(3);
  const paint = stroke
    ? `fill="none" stroke="${colour}" stroke-width="1.8" stroke-linejoin="round" stroke-linecap="round"`
    : `fill="${colour}"`;
  return (
    `<g transform="translate(${x},${y}) scale(${scale})">` +
    `<path d="${GLYPHS[name]}" ${paint}/></g>`
  );
}

/// A record dot with a ring travelling outward from it.
function pulse(colour, phase, cx = 72, cy = 50, base = 16) {
  const r = (base + 14 * phase).toFixed(1);
  const fade = (0.85 * (1 - phase)).toFixed(2);
  return (
    `<circle cx="${cx}" cy="${cy}" r="${base}" fill="${colour}"/>` +
    `<circle cx="${cx}" cy="${cy}" r="${r}" fill="none" stroke="${colour}"` +
    ` stroke-width="3" opacity="${fade}"/>`
  );
}

/// Three dots brightening in turn.
function working(colour, phase, cx = 72, cy = 50) {
  return [0, 1, 2]
    .map((i) => {
      // Each dot runs a third of a cycle behind the one before it.
      const local = (phase + 1 - i / 3) % 1;
      const level = (0.25 + 0.75 * Math.max(0, 1 - local * 2)).toFixed(2);
      return `<circle cx="${cx + (i - 1) * 16}" cy="${cy}" r="6" fill="${colour}" opacity="${level}"/>`;
    })
    .join("");
}

const FAILED_MARK =
  '<circle cx="72" cy="50" r="22" fill="none" stroke="#e5484d" stroke-width="5"/>' +
  '<path d="M72 39v14" stroke="#e5484d" stroke-width="6" stroke-linecap="round"/>' +
  '<circle cx="72" cy="61" r="3.5" fill="#e5484d"/>';

const PALETTE = {
  offline: { bg: "#1b1d20", ink: "#4a4f56", text: "#5f666e" },
  idle: { bg: "#22262b", ink: "#c8ced6", text: "#c8ced6" },
  busy: { bg: "#33291a", ink: "#f0a500", text: "#ffd479" },
  record: { bg: "#3d1f1f", ink: "#e5484d", text: "#ff9ea1" },
  call: { bg: "#33224a", ink: "#a06ef5", text: "#c9aefc" },
  fail: { bg: "#3d1f1f", ink: "#e5484d", text: "#ff9ea1" },
};

// ------------------------------------------------------------ the actions
//
// Each action watches only the states that belong to it. A note recording must
// not turn the dictation button red: a button that lights up for something it
// did not start is worse than one that stays dark.

const ACTIONS = {
  "com.geminiflow.control.dictate": {
    glyph: "mic",
    idleLabel: "Dictate",
    recording: ["recording"],
    busy: ["arming", "finalizing", "injecting"],
    // Dictation is hold-to-talk everywhere else, so the button sends both
    // edges itself and decides from the app's state which one comes next.
    press: (state) =>
      ["recording", "arming", "finalizing", "injecting"].includes(state)
        ? "dictation/stop"
        : "dictation/start",
  },
  "com.geminiflow.control.note": {
    glyph: "page",
    stroke: true,
    idleLabel: "Note",
    recording: ["noteRecording"],
    busy: ["noteProcessing"],
    press: () => "notes/toggle",
  },
  "com.geminiflow.control.call": {
    glyph: "phone",
    idleLabel: "Call",
    recording: ["callRecording"],
    busy: [],
    accent: "call",
    press: () => "call/toggle",
  },
};

/// Which palette an action should wear, given what the app is doing.
function moodFor(action, state) {
  if (state === "offline") return "offline";
  if (state === "error") return "fail";
  if (action.recording.includes(state)) return action.accent || "record";
  if (action.busy.includes(state)) return "busy";
  return "idle";
}

const MOVING = new Set(["record", "call", "busy"]);

function keyFace(action, state, phase) {
  const mood = moodFor(action, state);
  const look = PALETTE[mood] || PALETTE.idle;

  const art =
    mood === "record" || mood === "call"
      ? pulse(look.ink, phase)
      : mood === "busy"
        ? working(look.ink, phase)
        : mood === "fail"
          ? FAILED_MARK
          : glyph(action.glyph, look.ink, { stroke: action.stroke });

  const label =
    mood === "record" || mood === "call"
      ? "REC"
      : mood === "busy"
        ? "…"
        : mood === "fail"
          ? "failed"
          : mood === "offline"
            ? "offline"
            : action.idleLabel;

  return (
    '<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">' +
    `<rect width="144" height="144" rx="18" fill="${look.bg}"/>` +
    art +
    `<text x="72" y="122" font-family="Segoe UI, sans-serif" font-size="20"` +
    ` fill="${look.text}" text-anchor="middle">${label}</text>` +
    "</svg>"
  );
}

// --------------------------------------------------------- the touch strip
//
// 200 x 100, which is one quarter of the strip. A plugin cannot span the whole
// thing: each quarter belongs to one dial.

const STRIP_WORDS = {
  offline: "GeminiFlow offline",
  idle: "Ready",
  arming: "Starting…",
  recording: "Dictating",
  noteRecording: "Recording a note",
  callRecording: "Recording a call",
  finalizing: "Transcribing…",
  noteProcessing: "Writing it up…",
  injecting: "Typing it out…",
  error: "Something failed",
};

function stripMood(state) {
  if (state === "offline") return "offline";
  if (state === "error") return "fail";
  if (state === "callRecording") return "call";
  if (state === "recording" || state === "noteRecording") return "record";
  if (state === "idle") return "idle";
  return "busy";
}

function stripFace(state, phase, seconds, micLevel) {
  const mood = stripMood(state);
  const look = PALETTE[mood] || PALETTE.idle;

  const art =
    mood === "record" || mood === "call"
      ? pulse(look.ink, phase, 30, 40, 11)
      : mood === "busy"
        ? working(look.ink, phase, 30, 40)
        : `<circle cx="30" cy="40" r="11" fill="none" stroke="${look.ink}" stroke-width="3"/>`;

  const elapsed =
    seconds != null
      ? `<text x="186" y="47" font-family="Segoe UI, sans-serif" font-size="24"` +
        ` fill="${look.text}" text-anchor="end">${Math.floor(seconds / 60)}:${String(
          Math.floor(seconds % 60)
        ).padStart(2, "0")}</text>`
      : "";

  // The microphone level along the bottom, so the dial has something to point
  // at while it is being turned.
  const bar =
    micLevel >= 0
      ? '<rect x="14" y="80" width="172" height="6" rx="3" fill="#ffffff" opacity="0.12"/>' +
        `<rect x="14" y="80" width="${((172 * micLevel) / 100).toFixed(1)}" height="6" rx="3"` +
        ` fill="${look.ink}" opacity="0.8"/>`
      : "";

  return (
    '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100">' +
    `<rect width="200" height="100" fill="${look.bg}"/>` +
    art +
    `<text x="52" y="47" font-family="Segoe UI, sans-serif" font-size="17"` +
    ` fill="${look.text}">${STRIP_WORDS[state] || state}</text>` +
    elapsed +
    bar +
    "</svg>"
  );
}

const asImage = (svg) => `data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`;

// ------------------------------------------------------- Stream Deck link

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 2) args[argv[i].replace(/^-+/, "")] = argv[i + 1];
  return args;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const socket = new WebSocket(`ws://127.0.0.1:${args.port}`);

  // Every visible instance, by context. A button on an inactive page has no
  // context to draw to, so they are tracked as they appear and disappear.
  const keys = new Map(); // context -> action uuid
  const strips = new Set(); // encoder contexts

  let current = "offline";
  let phase = 0;
  let ticker = null;
  let startedAt = null; // when the present recording began
  let micLevel = -1;

  const send = (payload) => {
    if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(payload));
  };

  const isRecording = (state) =>
    ["recording", "noteRecording", "callRecording"].includes(state);

  const paint = () => {
    for (const [context, uuid] of keys) {
      const action = ACTIONS[uuid];
      if (!action) continue;
      send({
        event: "setImage",
        context,
        payload: { image: asImage(keyFace(action, current, phase)), target: 0 },
      });
    }

    if (strips.size > 0) {
      const seconds = startedAt ? (Date.now() - startedAt) / 1000 : null;
      const image = asImage(stripFace(current, phase, seconds, micLevel));
      for (const context of strips) {
        send({ event: "setFeedback", context, payload: { canvas: image } });
      }
    }
  };

  // Ten frames a second, the rate Elgato asks plugins to stay within and
  // comfortably smooth for a pulse. The timer exists only while something is
  // actually moving, so idle buttons cost nothing.
  const retime = () => {
    const keysMoving = [...keys.values()].some((uuid) => {
      const action = ACTIONS[uuid];
      return action && MOVING.has(moodFor(action, current));
    });
    const stripMoving = strips.size > 0 && MOVING.has(stripMood(current));
    const wanted = keysMoving || stripMoving;

    if (wanted && !ticker) {
      ticker = setInterval(() => {
        phase = (phase + 0.1) % 1;
        paint();
      }, 100);
    } else if (!wanted && ticker) {
      clearInterval(ticker);
      ticker = null;
      phase = 0;
    }
  };

  const refreshMic = () =>
    request("GET", "/mic", (body) => {
      if (typeof body.volume === "number" && body.volume !== micLevel) {
        micLevel = body.volume;
        paint();
      }
    });

  socket.on("open", () => {
    send({ event: args.registerEvent, uuid: args.pluginUUID });
    log("registered with Stream Deck");

    followState((status) => {
      const next = status.connected ? status.state : "offline";
      if (next === current) return;

      const was = isRecording(current);
      current = next;
      if (isRecording(current) && !was) startedAt = Date.now();
      if (!isRecording(current)) startedAt = null;

      log("state:", current);
      phase = 0;
      paint();
      retime();
    });

    refreshMic();
  });

  socket.on("message", (raw) => {
    let message;
    try {
      message = JSON.parse(raw.toString());
    } catch {
      return;
    }

    const { event, context, action } = message;

    switch (event) {
      case "willAppear":
        if (action === "com.geminiflow.control.status") strips.add(context);
        else keys.set(context, action);
        // Paint at once, so a button appearing mid-recording shows the truth
        // rather than waiting for the next change.
        paint();
        retime();
        break;

      case "willDisappear":
        strips.delete(context);
        keys.delete(context);
        retime();
        break;

      case "keyUp": {
        // On release, matching how every other Stream Deck button behaves.
        const spec = ACTIONS[action];
        if (spec) request("POST", `/action/${spec.press(current)}`);
        break;
      }

      case "dialDown":
      case "touchTap":
        request("POST", "/action/notes/toggle");
        break;

      case "dialRotate": {
        const ticks = (message.payload && message.payload.ticks) || 0;
        if (micLevel < 0) {
          refreshMic();
          break;
        }
        micLevel = Math.max(0, Math.min(100, micLevel + ticks * 2));
        // Redrawn before the request goes out, so the strip tracks the dial
        // instead of lagging a round trip behind it.
        paint();
        request("POST", `/mic/volume/${micLevel}`);
        break;
      }
    }
  });

  socket.on("close", () => {
    log("Stream Deck closed the connection; exiting");
    process.exit(0);
  });
  socket.on("error", (e) => log("Stream Deck socket error:", e.message));
}

main();
