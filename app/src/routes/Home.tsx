import { useEffect, useState } from "react";
import { api, type Settings } from "../lib/api";

export function Home() {
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);

  useEffect(() => {
    api.hasApiKey().then(setHasKey).catch(() => setHasKey(false));
    api.getSettings().then(setSettings).catch(() => {});
  }, []);

  return (
    <div className="card">
      <h1>GeminiFlow</h1>
      <p className="hint">
        Hold your dictation key anywhere in Windows, speak, and release — the
        text lands in whatever field has focus.
      </p>

      {hasKey === false && (
        <div className="banner" data-tone="warn">
          <strong>No API key set.</strong> Dictation will not work until you add
          a Gemini API key in Settings.
        </div>
      )}

      <h2>How it works</h2>
      <ul className="list">
        <li>
          <strong>Dictation</strong> — hold{" "}
          <kbd>{hotkeyLabel(settings?.hotkey)}</kbd>, speak, release. A pill
          appears at the bottom of the screen while it listens, so you can see
          it is picking you up without opening this window.
          <div className="meta">
            {settings?.useLive ? (
              <span>
                Streaming: usually under a second, but sessions sometimes fail
                to start and fall back
              </span>
            ) : (
              <span>
                Smart transcription: about 2–4 seconds after you release,
                longer for longer dictations
              </span>
            )}
          </div>
        </li>
        <li>
          <strong>Notes</strong> — not built yet. Coming in a later milestone:
          a toggle key that turns a longer recording into a summary with action
          items, plus a phone-call mode for capturing your half of a call.
        </li>
      </ul>

      <h2 style={{ marginTop: 24 }}>Good to know</h2>
      <p className="hint" style={{ marginBottom: 0 }}>
        Text goes to whichever window had focus when you pressed the key, not
        where you end up afterwards. If GeminiFlow cannot switch back to that
        window it will not paste at all — the transcript stays in your history
        rather than landing somewhere you did not intend.
      </p>
      <p className="hint" style={{ marginBottom: 0, marginTop: 12 }}>
        Your dictation key is swallowed while GeminiFlow runs, so it will not
        work as a modifier in other apps. Text is inserted by briefly placing it
        on the clipboard and sending a paste — your previous clipboard is
        restored a moment later. Dictation cannot type into windows running as
        administrator unless GeminiFlow is also elevated.
      </p>
    </div>
  );
}

function hotkeyLabel(hotkey: string | undefined): string {
  switch (hotkey) {
    case "RightAlt":
      return "Right Alt";
    case "RightShift":
      return "Right Shift";
    case "F24":
      return "F24";
    default:
      return "Right Ctrl";
  }
}
