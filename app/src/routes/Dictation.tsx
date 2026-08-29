import { useEffect, useState } from "react";
import { api, onDictation, type Dictation } from "../lib/api";

export function DictationView() {
  const [items, setItems] = useState<Dictation[]>([]);
  const [terms, setTerms] = useState("");
  const [savedTerms, setSavedTerms] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api.listDictations(100).then(setItems).catch(() => {});
    api
      .getVocabulary()
      .then((v) => {
        const text = v.join("\n");
        setTerms(text);
        setSavedTerms(text);
      })
      .catch(() => {});

    const unlisten = onDictation((d) => setItems((prev) => [d, ...prev]));
    return () => {
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  const parsed = terms
    .split("\n")
    .map((t) => t.trim())
    .filter(Boolean);
  const dirty = terms !== savedTerms;
  const overLimit = parsed.length > 1000;

  async function saveVocabulary() {
    setSaving(true);
    try {
      await api.saveVocabulary(parsed);
      setSavedTerms(terms);
    } finally {
      setSaving(false);
    }
  }

  async function remove(id: number) {
    await api.deleteDictation(id);
    setItems((prev) => prev.filter((d) => d.id !== id));
  }

  return (
    <div className="grid">
      <div className="card">
        <h2>Custom vocabulary</h2>
        <p className="hint">
          One term per line. These are sent with every transcription to bias
          recognition toward words the model would otherwise mangle — your
          identifiers, library names, coworkers. Up to 1,000.
        </p>
        <textarea
          value={terms}
          onChange={(e) => setTerms(e.target.value)}
          spellCheck={false}
          placeholder={"useCallback\nWASAPI\nTauri"}
        />
        <div className="meta" style={{ marginBottom: 12 }}>
          <span>
            {parsed.length} term{parsed.length === 1 ? "" : "s"}
          </span>
          {overLimit && (
            <span style={{ color: "var(--danger)" }}>
              over the 1,000 limit — extras will be dropped
            </span>
          )}
        </div>
        <button
          className="primary"
          onClick={saveVocabulary}
          disabled={!dirty || saving}
        >
          {saving ? "Saving…" : dirty ? "Save vocabulary" : "Saved"}
        </button>
      </div>

      <div className="card">
        <h2>History</h2>
        {items.length === 0 ? (
          <div className="empty">
            Nothing dictated yet.
            <br />
            Hold your dictation key anywhere and speak.
          </div>
        ) : (
          <ul className="list">
            {items.map((d) => (
              <li key={d.id}>
                {d.text}
                <div className="meta">
                  <span>{new Date(d.createdAt).toLocaleString()}</span>
                  {d.targetApp && <span>{d.targetApp}</span>}
                  {d.latencyMs !== null && <span>{d.latencyMs} ms</span>}
                  {!d.injectedOk && (
                    <span style={{ color: "var(--danger)" }}>
                      insert failed
                    </span>
                  )}
                  <button
                    className="ghost"
                    style={{ padding: "2px 8px", fontSize: 12 }}
                    onClick={() => navigator.clipboard.writeText(d.text)}
                  >
                    Copy
                  </button>
                  <button
                    className="ghost"
                    style={{ padding: "2px 8px", fontSize: 12 }}
                    onClick={() => remove(d.id)}
                  >
                    Delete
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
