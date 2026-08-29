import { useEffect, useState } from "react";
import { Home } from "./routes/Home";
import { Notes } from "./routes/Notes";
import { DictationView } from "./routes/Dictation";
import { SettingsView } from "./routes/Settings";
import { api, onStatus, type Settings, type StatusEvent } from "./lib/api";
import { useWindowHotkeys } from "./lib/useWindowHotkeys";

type Tab = "home" | "notes" | "dictation" | "settings";

const TABS: { id: Tab; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "notes", label: "Notes" },
  { id: "dictation", label: "Dictation" },
  { id: "settings", label: "Settings" },
];

export function App() {
  const [tab, setTab] = useState<Tab>("home");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [status, setStatus] = useState<StatusEvent>({
    state: "idle",
    detail: null,
    partial: null,
  });

  // The global hook is not called while this window has focus, so the UI
  // forwards the same keys to the engine.
  useWindowHotkeys(settings);

  useEffect(() => {
    api.getSettings().then(setSettings).catch(() => {});
    api.getState().then(setStatus).catch(() => {});
    const unlisten = onStatus(setStatus);
    return () => {
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  return (
    <div className="shell">
      <header className="topbar">
        <div className="brand">
          <span className="brand-dot" />
          GeminiFlow
        </div>

        <span className="status" data-state={status.state}>
          <span className="pip" />
          {status.partial ?? labelFor(status)}
        </span>

        <nav aria-label="Sections">
          {TABS.map((t) => (
            <button
              key={t.id}
              data-active={tab === t.id}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
        </nav>
      </header>

      {status.state === "error" && status.detail && (
        <div className="error-bar" role="alert">
          <span className="error-mark" aria-hidden>
            !
          </span>
          <span>{status.detail}</span>
          <button
            className="error-dismiss"
            aria-label="Dismiss"
            onClick={() =>
              setStatus({ state: "idle", detail: null, partial: null })
            }
          >
            ×
          </button>
        </div>
      )}

      {tab === "home" && <Home />}
      {tab === "notes" && <Notes />}
      {tab === "dictation" && <DictationView />}
      {tab === "settings" && <SettingsView />}
    </div>
  );
}

function labelFor(status: StatusEvent): string {
  // Errors are shown in full in the bar below, not squeezed into the pill.
  if (status.state === "error") return "Error";
  if (status.detail) return status.detail;
  switch (status.state) {
    case "idle":
      return "Ready";
    case "arming":
      return "Starting…";
    case "recording":
      return "Listening";
    case "finalizing":
      return "Transcribing";
    case "injecting":
      return "Inserting";
    case "callRecording":
      return "Recording call";
    case "noteRecording":
      return "Recording note";
    case "noteProcessing":
      return "Writing up note";
  }
}
