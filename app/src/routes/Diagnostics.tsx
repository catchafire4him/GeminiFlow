import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../lib/api";

/**
 * The application log, in the app.
 *
 * Diagnosing anything here has meant reading `%APPDATA%/GeminiFlow/geminiflow.log`
 * by hand. Surfacing it means a problem can be seen and copied without leaving
 * the window or knowing where the file lives.
 */
export function Diagnostics() {
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [follow, setFollow] = useState(true);
  const boxRef = useRef<HTMLPreElement>(null);

  const load = useCallback(async () => {
    try {
      setText(await api.readLog(800));
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    load();
    // Cheap enough to poll: the log is small and this view is rarely open.
    const id = setInterval(load, 3000);
    return () => clearInterval(id);
  }, [load]);

  useEffect(() => {
    if (follow && boxRef.current) {
      boxRef.current.scrollTop = boxRef.current.scrollHeight;
    }
  }, [text, follow]);

  async function copyAll() {
    await navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }

  return (
    <div className="card">
      <h1>Diagnostics</h1>
      <p className="hint">
        The most recent activity, newest at the bottom. The log is cleared each
        time GeminiFlow starts, so this covers the current run only. Turn on
        <strong> Verbose diagnostics</strong> in Settings before reproducing a
        problem to capture more detail.
      </p>

      <div className="row" style={{ marginBottom: 14, flexWrap: "wrap" }}>
        <button className="primary" onClick={copyAll} disabled={!text}>
          {copied ? "Copied" : "Copy everything"}
        </button>
        <button className="ghost" onClick={load}>
          Refresh
        </button>
        <button className="ghost" onClick={() => api.openDataFolder()}>
          Open folder
        </button>
        <label className="row" style={{ gap: 8, margin: 0 }}>
          <input
            type="checkbox"
            checked={follow}
            style={{ width: "auto" }}
            onChange={(e) => setFollow(e.target.checked)}
          />
          <span style={{ fontWeight: 400 }}>Follow new lines</span>
        </label>
      </div>

      {error ? (
        <div className="banner" data-tone="warn">
          {error}
        </div>
      ) : (
        <pre className="log-view" ref={boxRef}>
          {text || "Nothing logged yet."}
        </pre>
      )}

      <p className="hint" style={{ marginTop: 14, marginBottom: 0 }}>
        Your API key is never written to the log. What you dictated can appear
        in it, though — transcripts show up in error messages and, with verbose
        diagnostics on, in streaming frames. Worth a glance before sharing it.
      </p>
    </div>
  );
}
