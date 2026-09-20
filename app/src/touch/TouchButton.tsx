import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type StatusEvent } from "../lib/api";

/// How far a finger may wander before the press is read as a drag.
const DRAG_SLOP = 12;

/// Movement only becomes a drag inside this window. After it, the press is
/// taken to be speech and wandering is ignored -- a hand holding still for
/// thirty seconds will drift, and cancelling someone's dictation because of
/// that would be maddening.
const DRAG_WINDOW_MS = 300;

/// A press shorter than this, with no movement, latches instead of ending.
/// Holding a finger against glass for a minute is unpleasant, so a tap starts
/// the recording and leaves it running until the next tap.
const TAP_MS = 400;

type Phase = "idle" | "pressing" | "latched" | "dragging";

const RECORDING = ["arming", "recording"];
const WORKING = ["finalizing", "injecting"];

export function TouchButton() {
  const [state, setState] = useState<string>("idle");
  const [phase, setPhase] = useState<Phase>("idle");
  const [doomed, setDoomed] = useState(false);

  // Mutable through a gesture, and never worth a re-render on its own.
  const gesture = useRef({
    startX: 0,
    startY: 0,
    grabX: 0,
    grabY: 0,
    at: 0,
    phase: "idle" as Phase,
    pending: null as { x: number; y: number } | null,
    frame: 0,
  });

  const setPhaseBoth = (next: Phase) => {
    gesture.current.phase = next;
    setPhase(next);
  };

  useEffect(() => {
    api.status().then((s) => setState(s.state)).catch(() => {});
    const stop = listen<StatusEvent>("engine://status", (e) => setState(e.payload.state));
    return () => {
      stop.then((f) => f()).catch(() => {});
    };
  }, []);

  // The app is the authority on whether anything is recording. If a latched
  // recording ends on its own -- the runaway guard, or a failure -- the button
  // has to stop claiming to hold it.
  useEffect(() => {
    if (gesture.current.phase === "latched" && !RECORDING.includes(state)) {
      setPhaseBoth("idle");
    }
  }, [state]);

  function onPointerDown(e: React.PointerEvent) {
    e.preventDefault();
    (e.target as Element).setPointerCapture?.(e.pointerId);

    const g = gesture.current;
    g.startX = e.screenX;
    g.startY = e.screenY;
    // Where inside the button the finger landed, so dragging does not snap
    // the button's corner to the fingertip.
    g.grabX = e.clientX;
    g.grabY = e.clientY;
    g.at = Date.now();

    if (g.phase === "latched") {
      // Stays latched for now: this press might be a drag rather than a stop,
      // and that is only known on release.
      return;
    }

    setPhaseBoth("pressing");
    api.touchPress().catch(() => {});
  }

  function onPointerMove(e: React.PointerEvent) {
    const g = gesture.current;
    if (g.phase === "idle") return;

    const dx = e.screenX - g.startX;
    const dy = e.screenY - g.startY;
    const moved = Math.hypot(dx, dy);

    if (g.phase !== "dragging") {
      if (moved < DRAG_SLOP) return;
      const early = Date.now() - g.at < DRAG_WINDOW_MS;
      // A latched button can be picked up at any time, since no one is
      // holding it. A live hold can only become a drag in the first moment.
      if (g.phase === "pressing" && !early) return;

      if (g.phase === "pressing") {
        // Abandoned outright rather than stopped: this press was never meant
        // to be speech, so nothing should be transcribed or pasted.
        api.touchCancel().catch(() => {});
      }
      setPhaseBoth("dragging");
      setDoomed(false);
      api.touchDragging(true).catch(() => {});
    }

    // Coalesced to one move per frame. A pointermove per pixel would be a
    // message per pixel, and the window cannot repaint that fast anyway.
    g.pending = { x: e.screenX - g.grabX, y: e.screenY - g.grabY };
    if (g.frame) return;
    g.frame = requestAnimationFrame(() => {
      g.frame = 0;
      const p = g.pending;
      g.pending = null;
      if (!p) return;
      api
        .touchMove(p.x, p.y)
        .then(setDoomed)
        .catch(() => {});
    });
  }

  function onPointerUp(e: React.PointerEvent) {
    const g = gesture.current;
    const held = Date.now() - g.at;
    const moved = Math.hypot(e.screenX - g.startX, e.screenY - g.startY);

    if (g.frame) {
      cancelAnimationFrame(g.frame);
      g.frame = 0;
    }

    if (g.phase === "dragging") {
      if (doomed) {
        api.touchDismiss().catch(() => {});
      } else {
        api.touchSettle().catch(() => {});
      }
      setDoomed(false);
      // A latched recording survives being moved. Picking the button up is
      // not a request to stop talking.
      setPhaseBoth(RECORDING.includes(state) ? "latched" : "idle");
      return;
    }

    if (g.phase === "latched") {
      if (moved < DRAG_SLOP) {
        api.touchRelease().catch(() => {});
        setPhaseBoth("idle");
      }
      return;
    }

    if (g.phase === "pressing") {
      if (held < TAP_MS && moved < DRAG_SLOP) {
        setPhaseBoth("latched");
        return;
      }
      api.touchRelease().catch(() => {});
      setPhaseBoth("idle");
    }
  }

  function onPointerCancel() {
    const g = gesture.current;
    // Windows can take a touch away mid-gesture. Whatever was happening, end
    // it rather than leave a recording nobody is holding.
    if (g.phase === "pressing") api.touchRelease().catch(() => {});
    if (g.phase === "dragging") api.touchSettle().catch(() => {});
    setDoomed(false);
    setPhaseBoth("idle");
  }

  const recording = RECORDING.includes(state) || phase === "latched";
  const working = WORKING.includes(state);
  const failed = state === "error";

  const look = doomed
    ? "doomed"
    : failed
      ? "failed"
      : recording
        ? "recording"
        : working
          ? "working"
          : "ready";

  return (
    <button
      className={`touch ${look} ${phase === "dragging" ? "dragging" : ""}`}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerCancel}
      onContextMenu={(e) => e.preventDefault()}
      aria-label={recording ? "Stop dictating" : "Hold to dictate"}
    >
      <span className="ring" />
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 14a3 3 0 0 0 3-3V5a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3z" />
        <path d="M17 11a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.9V21h2v-3.1A7 7 0 0 0 19 11h-2z" />
      </svg>
      {phase === "latched" && <span className="latch" />}
    </button>
  );
}
