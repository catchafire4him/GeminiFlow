import { useEffect, useState } from "react";
import { api } from "../lib/api";

/**
 * Plays a note's saved recording.
 *
 * The audio comes across as base64 and is turned into a Blob URL rather than
 * loaded from a file path: the webview cannot read arbitrary files off disk,
 * and widening its CSP to allow that would be a poor trade for a play button.
 */
export function AudioPlayer({ noteId }: { noteId: number }) {
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  // Reset when moving between notes, so the previous recording is not left
  // playing under a different note's heading.
  useEffect(() => {
    setUrl(null);
    setError(null);
  }, [noteId]);

  // Object URLs are revoked on unmount and on change; leaking them would pin
  // the whole recording in memory for the life of the window.
  useEffect(() => {
    return () => {
      if (url) URL.revokeObjectURL(url);
    };
  }, [url]);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const base64 = await api.noteAudio(noteId);
      const bytes = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
      setUrl(URL.createObjectURL(new Blob([bytes], { type: "audio/wav" })));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  if (error) {
    return (
      <p className="hint" style={{ marginBottom: 0 }}>
        {error}
      </p>
    );
  }

  // Loaded on demand rather than with the note: pulling several MB across for
  // every note you glance at would make the list feel slow.
  if (!url) {
    return (
      <button className="ghost" onClick={load} disabled={loading}>
        {loading ? "Loading audio…" : "▶ Play recording"}
      </button>
    );
  }

  return <audio className="note-audio" controls autoPlay src={url} />;
}
