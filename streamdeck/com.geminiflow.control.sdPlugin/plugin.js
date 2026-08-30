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

/// A filled record dot with a ring pulsing outward from it.
///
/// The pulse is written into the drawing itself rather than pushed frame by
/// frame. If Stream Deck plays it, animation costs nothing from here on; if it
/// renders one still frame, the button simply looks solid and we know to
/// animate the slower way.
const pulse = (colour) =>
  `<circle cx="72" cy="58" r="18" fill="${colour}"/>` +
  `<circle cx="72" cy="58" r="18" fill="none" stroke="${colour}" stroke-width="3">` +
  '<animate attributeName="r" values="18;32;18" dur="1.6s" repeatCount="indefinite"/>' +
  '<animate attributeName="opacity" values="0.9;0;0.9" dur="1.6s" repeatCount="indefinite"/>' +
  "</circle>";

/// Three dots brightening in turn.
const working = (colour) =>
  [0, 1, 2]
    .map(
      (i) =>
        `<circle cx="${56 + i * 16}" cy="58" r="6" fill="${colour}" opacity="0.25">` +
        `<animate attributeName="opacity" values="0.25;1;0.25" dur="1.2s"` +
        ` begin="${i * 0.2}s" repeatCount="indefinite"/></circle>`
    )
    .join("");

const LOOKS = {
  offline: { bg: "#1b1d20", art: () => page("#4a4f56"), label: "offline", text: "#5f666e" },
  idle: { bg: "#22262b", art: () => page("#c8ced6"), label: "Note", text: "#c8ced6" },
  arming: { bg: "#33291a", art: () => working("#f0a500"), label: "…", text: "#ffd479" },
  recording: { bg: "#3d1f1f", art: () => pulse("#e5484d"), label: "REC", text: "#ff9ea1" },
  noteRecording: { bg: "#3d1f1f", art: () => pulse("#e5484d"), label: "REC", text: "#ff9ea1" },
  callRecording: { bg: "#33224a", art: () => pulse("#a06ef5"), label: "CALL", text: "#c9aefc" },
  finalizing: { bg: "#33291a", art: () => working("#f0a500"), label: "…", text: "#ffd479" },
  noteProcessing: { bg: "#33291a", art: () => working("#f0a500"), label: "…", text: "#ffd479" },
  injecting: { bg: "#1f3324", art: () => working("#30a46c"), label: "…", text: "#8fe3b4" },
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

/// The button image, as a drawing rather than a bitmap.
///
/// There is nothing here a drawing library would do better, and a string can
/// be built without one.
function face(state) {
  const look = LOOKS[state] || LOOKS.idle;
  return (
    '<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">' +
    `<rect width="144" height="144" rx="18" fill="${look.bg}"/>` +
    look.art() +
    `<text x="72" y="122" font-family="Segoe UI, sans-serif" font-size="20"` +
    ` fill="${look.text}" text-anchor="middle">${look.label}</text>` +
    "</svg>"
  );
}

function imageFor(state) {
  return `data:image/svg+xml;base64,${Buffer.from(face(state)).toString("base64")}`;
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

  const send = (payload) => {
    if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(payload));
  };

  const paint = () => {
    for (const context of buttons) {
      send({
        event: "setImage",
        context,
        payload: { image: imageFor(current), target: 0 },
      });
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
      paint();
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
          payload: { image: imageFor(current), target: 0 },
        });
        break;

      case "willDisappear":
        buttons.delete(message.context);
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
