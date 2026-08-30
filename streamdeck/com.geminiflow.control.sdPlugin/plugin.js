// Bridges two connections: the socket Stream Deck opens for its plugins, and
// GeminiFlow's local control endpoint.
//
// Presses go one way, state comes back the other, and the button image is
// redrawn from whatever the app last reported. That is the whole point of the
// plugin -- a plain Stream Deck hotkey can start a note, but it cannot know
// that the note stopped by itself and go back to looking idle.
//
// Written in plain JavaScript with one dependency, so it can be read and
// changed in place without a build step.

const WebSocket = require("ws");
const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

// ---------------------------------------------------------------- logging
// Stream Deck captures stdout into its own plugin log, which is the only
// practical way to see what a plugin is doing.
const log = (...parts) => console.log(new Date().toISOString(), ...parts);

// ------------------------------------------------- GeminiFlow control link

const DESCRIPTOR = path.join(
  process.env.APPDATA || "",
  "GeminiFlow",
  "control.json"
);

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

function request(method, route, onResponse) {
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
    onResponse || (() => {})
  );
  req.on("error", (e) => log(`request to ${route} failed:`, e.message));
  req.end();
  return req;
}

/// Holds the state stream open, reconnecting whenever it drops.
///
/// The app is expected to come and go -- it restarts, or is not running yet
/// when Stream Deck starts the plugin -- so losing the connection is normal
/// operation rather than an error worth surfacing.
function followState(onState) {
  let backoff = 1000;

  const connect = () => {
    const req = request("GET", "/events", (res) => {
      if (res.statusCode !== 200) {
        res.resume();
        return retry();
      }
      backoff = 1000;
      onState({ state: "idle", detail: null, connected: true });

      let buffer = "";
      res.setEncoding("utf8");
      res.on("data", (chunk) => {
        buffer += chunk;
        // Events are separated by a blank line; anything after the last one
        // is a partial event and stays in the buffer.
        const events = buffer.split("\n\n");
        buffer = events.pop();
        for (const event of events) {
          for (const line of event.split("\n")) {
            if (!line.startsWith("data:")) continue; // keepalive comments
            try {
              onState({ ...JSON.parse(line.slice(5).trim()), connected: true });
            } catch {
              // A malformed line is not worth tearing the connection down for.
            }
          }
        }
      });
      res.on("end", retry);
      res.on("error", retry);
    });
    if (!req) retry();
  };

  let retrying = false;
  const retry = () => {
    if (retrying) return;
    retrying = true;
    onState({ state: "offline", detail: null, connected: false });
    setTimeout(() => {
      retrying = false;
      connect();
    }, backoff);
    // Back off to half a minute so a stopped app is not polled hard forever.
    backoff = Math.min(backoff * 2, 30000);
  };

  connect();
}

// ------------------------------------------------------------- button face
//
// Movement is drawn frame by frame from here. Animation written inside the
// picture does not play -- Stream Deck renders a single still frame of it --
// and animated image files are not accepted through the plugin interface at
// all. Sending a fresh picture on a timer is what is left, so every drawing
// below takes a phase between 0 and 1 and the ticker in main() advances it.

// A page with a folded corner. Shared by every resting state so the button
// stays recognisably the same object while its colour changes.
const PAGE =
  '<path d="M56 28h24l14 14v44a4 4 0 0 1-4 4H56a4 4 0 0 1-4-4V32a4 4 0 0 1 4-4z"' +
  ' fill="none" stroke="CLR" stroke-width="4" stroke-linejoin="round"/>' +
  '<path d="M80 28v14h14" fill="none" stroke="CLR" stroke-width="4"' +
  ' stroke-linejoin="round"/>' +
  '<path d="M62 58h20M62 68h20M62 78h12" stroke="CLR" stroke-width="4"' +
  ' stroke-linecap="round"/>';

const page = (colour) => PAGE.replaceAll("CLR", colour);

/// A record dot with a ring travelling outward from it.
const pulse = (colour, phase) => {
  const r = (18 + 16 * phase).toFixed(1);
  const fade = (0.85 * (1 - phase)).toFixed(2);
  return (
    `<circle cx="72" cy="58" r="18" fill="${colour}"/>` +
    `<circle cx="72" cy="58" r="${r}" fill="none" stroke="${colour}"` +
    ` stroke-width="3" opacity="${fade}"/>`
  );
};

/// Three dots brightening in turn.
const working = (colour, phase) =>
  [0, 1, 2]
    .map((i) => {
      // Each dot runs a third of a cycle behind the one before it.
      const local = (phase + 1 - i / 3) % 1;
      // Bright at the start of its slot, fading across the rest.
      const level = (0.25 + 0.75 * Math.max(0, 1 - local * 2)).toFixed(2);
      return (
        `<circle cx="${56 + i * 16}" cy="58" r="6" fill="${colour}"` +
        ` opacity="${level}"/>`
      );
    })
    .join("");

const LOOKS = {
  offline: { bg: "#1b1d20", art: () => page("#4a4f56"), label: "offline", text: "#5f666e" },
  idle: { bg: "#22262b", art: () => page("#c8ced6"), label: "Note", text: "#c8ced6" },
  arming: { bg: "#33291a", art: (p) => working("#f0a500", p), label: "…", text: "#ffd479" },
  recording: { bg: "#3d1f1f", art: (p) => pulse("#e5484d", p), label: "REC", text: "#ff9ea1" },
  noteRecording: { bg: "#3d1f1f", art: (p) => pulse("#e5484d", p), label: "REC", text: "#ff9ea1" },
  callRecording: { bg: "#33224a", art: (p) => pulse("#a06ef5", p), label: "CALL", text: "#c9aefc" },
  finalizing: { bg: "#33291a", art: (p) => working("#f0a500", p), label: "…", text: "#ffd479" },
  noteProcessing: { bg: "#33291a", art: (p) => working("#f0a500", p), label: "…", text: "#ffd479" },
  injecting: { bg: "#1f3324", art: (p) => working("#30a46c", p), label: "…", text: "#8fe3b4" },
  error: {
    bg: "#3d1f1f",
    art: () =>
      '<circle cx="72" cy="58" r="24" fill="none" stroke="#e5484d" stroke-width="5"/>' +
      '<path d="M72 46v16" stroke="#e5484d" stroke-width="6" stroke-linecap="round"/>' +
      '<circle cx="72" cy="71" r="3.5" fill="#e5484d"/>',
    label: "failed",
    text: "#ff9ea1",
  },
};

/// States worth spending frames on. Everything else is drawn once and left.
const MOVING = new Set([
  "arming",
  "recording",
  "noteRecording",
  "callRecording",
  "finalizing",
  "noteProcessing",
  "injecting",
]);

function face(state, phase) {
  const look = LOOKS[state] || LOOKS.idle;
  return (
    '<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">' +
    `<rect width="144" height="144" rx="18" fill="${look.bg}"/>` +
    look.art(phase) +
    `<text x="72" y="122" font-family="Segoe UI, sans-serif" font-size="20"` +
    ` fill="${look.text}" text-anchor="middle">${look.label}</text>` +
    "</svg>"
  );
}

function imageFor(state, phase) {
  return `data:image/svg+xml;base64,${Buffer.from(face(state, phase)).toString("base64")}`;
}

// ------------------------------------------------------- Stream Deck link

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 2) {
    args[argv[i].replace(/^-+/, "")] = argv[i + 1];
  }
  return args;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const socket = new WebSocket(`ws://127.0.0.1:${args.port}`);

  // Every visible instance of our action. A button that is not on the active
  // page has no context to draw to, so they are tracked as they appear.
  const buttons = new Set();
  let current = "offline";
  let phase = 0;
  let ticker = null;

  const send = (payload) => {
    if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(payload));
  };

  const paint = () => {
    const image = imageFor(current, phase);
    for (const context of buttons) {
      send({ event: "setImage", context, payload: { image, target: 0 } });
    }
  };

  // Ten frames a second, which is the rate Elgato asks plugins to stay
  // within and comfortably smooth for a pulse. The timer only exists while
  // something is actually moving, so an idle button costs nothing.
  const retime = () => {
    const wanted = MOVING.has(current) && buttons.size > 0;
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

  socket.on("open", () => {
    send({ event: args.registerEvent, uuid: args.pluginUUID });
    log("registered with Stream Deck");

    followState((status) => {
      const next = status.connected ? status.state : "offline";
      if (next === current) return;
      current = next;
      log("state:", current);
      phase = 0;
      paint();
      retime();
    });
  });

  socket.on("message", (raw) => {
    let message;
    try {
      message = JSON.parse(raw.toString());
    } catch {
      return;
    }

    switch (message.event) {
      case "willAppear":
        buttons.add(message.context);
        // Paint immediately so a button that appears mid-recording shows the
        // truth rather than waiting for the next change.
        send({
          event: "setImage",
          context: message.context,
          payload: { image: imageFor(current, phase), target: 0 },
        });
        retime();
        break;

      case "willDisappear":
        buttons.delete(message.context);
        retime();
        break;

      case "keyUp":
        // On release rather than press, matching how every other Stream Deck
        // button behaves.
        request("POST", "/action/notes/toggle");
        break;
    }
  });

  socket.on("close", () => {
    log("Stream Deck closed the connection; exiting");
    process.exit(0);
  });
  socket.on("error", (e) => log("Stream Deck socket error:", e.message));
}

main();
