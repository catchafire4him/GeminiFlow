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

const LOOKS = {
  offline: { fill: "#2b2b2b", ring: "#4a4a4a", label: "offline", text: "#7a7a7a" },
  idle: { fill: "#22262b", ring: "#4d5560", label: "Note", text: "#c8ced6" },
  arming: { fill: "#3a3320", ring: "#c9a227", label: "…", text: "#e8d48a" },
  recording: { fill: "#3d1f1f", ring: "#e5484d", label: "REC", text: "#ff9ea1" },
  noteRecording: { fill: "#3d1f1f", ring: "#e5484d", label: "REC", text: "#ff9ea1" },
  callRecording: { fill: "#3a2440", ring: "#c04ae0", label: "CALL", text: "#e6a8f5" },
  finalizing: { fill: "#33291a", ring: "#f0a500", label: "…", text: "#ffd479" },
  noteProcessing: { fill: "#33291a", ring: "#f0a500", label: "…", text: "#ffd479" },
  injecting: { fill: "#1f3324", ring: "#30a46c", label: "…", text: "#8fe3b4" },
  error: { fill: "#3d1f1f", ring: "#e5484d", label: "!", text: "#ff9ea1" },
};

/// The button image, as an SVG.
///
/// SVG rather than a rendered bitmap because there is nothing here a drawing
/// library would do better, and a string can be built without one.
function face(state) {
  const look = LOOKS[state] || LOOKS.idle;
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144">` +
    `<rect width="144" height="144" rx="18" fill="${look.fill}"/>` +
    `<circle cx="72" cy="60" r="26" fill="none" stroke="${look.ring}" stroke-width="7"/>` +
    `<text x="72" y="118" font-family="Segoe UI, sans-serif" font-size="22"` +
    ` fill="${look.text}" text-anchor="middle">${look.label}</text>` +
    `</svg>`
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
