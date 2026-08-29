import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { onStatus, type StatusEvent } from "../lib/api";
import "./overlay.css";

const BAR_COUNT = 18;

export function Overlay() {
  const [status, setStatus] = useState<StatusEvent>({
    state: "idle",
    detail: null,
    partial: null,
  });
  const [bars, setBars] = useState<number[]>(() => new Array(BAR_COUNT).fill(0));
  const levelRef = useRef(0);

  useEffect(() => {
    const unStatus = onStatus(setStatus);
    const unLevel = listen<number>("engine://level", (e) => {
      levelRef.current = e.payload;
    });
    return () => {
      unStatus.then((f) => f()).catch(() => {});
      unLevel.then((f) => f()).catch(() => {});
    };
  }, []);

  // Scroll the meter left on a timer rather than on each level event, so the
  // animation stays smooth even when audio chunks arrive unevenly.
  useEffect(() => {
    const id = setInterval(() => {
      setBars((prev) => [...prev.slice(1), levelRef.current]);
    }, 60);
    return () => clearInterval(id);
  }, []);

  const recording =
    status.state === "recording" ||
    status.state === "arming" ||
    status.state === "noteRecording" ||
    status.state === "callRecording";
  const working =
    status.state === "finalizing" ||
    status.state === "injecting" ||
    status.state === "noteProcessing";

  return (
    <div className="pill" data-state={status.state}>
      <span className="dot" />

      <div className="body">
        <div className="label">{labelFor(status)}</div>
        {status.partial && (
          <div className="partial">{captionTail(status.partial)}</div>
        )}
      </div>

      {recording && (
        <div className="meter" aria-hidden>
          {bars.map((level, i) => (
            <span
              key={i}
              style={{ transform: `scaleY(${0.12 + level * 0.88})` }}
            />
          ))}
        </div>
      )}

      {working && <div className="spinner" aria-hidden />}
    </div>
  );
}

/**
 * Keeps the most recent speech visible.
 *
 * The partial grows without bound as you talk, and the interesting part is
 * always the end -- clipping the front is what a caption should do. Cut on a
 * word boundary so it does not lop a word in half.
 */
function captionTail(text: string, maxChars = 220): string {
  if (text.length <= maxChars) return text;
  const tail = text.slice(text.length - maxChars);
  const space = tail.indexOf(" ");
  return "… " + (space === -1 ? tail : tail.slice(space + 1));
}

function labelFor(status: StatusEvent): string {
  switch (status.state) {
    case "arming":
      return "Starting…";
    case "recording":
      return "Listening";
    case "finalizing":
      return status.detail ?? "Transcribing…";
    case "injecting":
      return "Inserting";
    case "error":
      return status.detail ?? "Something went wrong";
    case "callRecording":
      return status.detail ?? "Recording call — press again to stop";
    case "noteRecording":
      // Deliberately explicit: a note left running by accident is the failure
      // this label exists to prevent.
      return status.detail ?? "Recording note — press again to stop";
    case "noteProcessing":
      return status.detail ?? "Writing up your note…";
    case "idle":
      return "Done";
  }
}
