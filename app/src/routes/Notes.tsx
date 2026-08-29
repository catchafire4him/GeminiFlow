import { useCallback, useEffect, useState } from "react";
import {
  api,
  noteToMarkdown,
  onNote,
  onSummaryStatus,
  type Note,
  type NoteSummary,
  type Settings,
} from "../lib/api";
import { AudioPlayer } from "../components/AudioPlayer";

export function Notes() {
  const [list, setList] = useState<NoteSummary[]>([]);
  const [selected, setSelected] = useState<Note | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [retrying, setRetrying] = useState(false);
  // Distinguishes "still working on it" from "it gave up", which the
  // needsSummary flag alone cannot express.
  const [summaryState, setSummaryState] = useState<
    Record<number, "summarising" | "failed">
  >({});

  const refresh = useCallback(async (selectId?: number) => {
    const rows = await api.listNotes(200);
    setList(rows);
    const id = selectId ?? rows[0]?.id;
    if (id === undefined) {
      setSelected(null);
      return;
    }
    setSelected(await api.getNote(id));
  }, []);

  useEffect(() => {
    refresh().catch(() => {});
    api.getSettings().then(setSettings).catch(() => {});

    // Notes arrive twice: once when the transcript is saved, again when the
    // summary lands. The second must update the row in place rather than
    // adding a duplicate.
    const un = onNote((note) => {
      const row = {
        id: note.id,
        title: note.title,
        createdAt: note.createdAt,
        durationMs: note.durationMs,
        kind: note.kind,
        actionCount: note.actionItems.length,
        actionDone: note.actionItems.filter((a) => a.done).length,
      };
      setList((prev) =>
        prev.some((n) => n.id === note.id)
          ? prev.map((n) => (n.id === note.id ? row : n))
          : [row, ...prev]
      );
      // Only follow the update if the user is already looking at that note.
      if (!note.needsSummary) {
        setSummaryState((prev) => {
          const next = { ...prev };
          delete next[note.id];
          return next;
        });
      }
      setSelected((current) =>
        current === null || current.id === note.id ? note : current
      );
    });
    const unStatus = onSummaryStatus((s) =>
      setSummaryState((prev) => ({ ...prev, [s.id]: s.state }))
    );

    return () => {
      un.then((f) => f()).catch(() => {});
      unStatus.then((f) => f()).catch(() => {});
    };
  }, [refresh]);

  async function toggle(itemId: number, done: boolean) {
    if (!selected) return;
    // Optimistic: the checkbox should not lag behind a disk write.
    setSelected({
      ...selected,
      actionItems: selected.actionItems.map((a) =>
        a.id === itemId ? { ...a, done } : a
      ),
    });
    await api.setActionDone(itemId, done);
    setList((prev) =>
      prev.map((n) =>
        n.id === selected.id
          ? { ...n, actionDone: n.actionDone + (done ? 1 : -1) }
          : n
      )
    );
  }

  async function remove(id: number) {
    await api.deleteNote(id);
    const rest = list.filter((n) => n.id !== id);
    setList(rest);
    setSelected(rest.length ? await api.getNote(rest[0].id) : null);
  }

  function flash(message: string) {
    setToast(message);
    setTimeout(() => setToast(null), 2400);
  }

  return (
    <div className="grid">
      <div className="card">
        <h2>Recordings</h2>
        {list.length === 0 ? (
          <div className="empty">
            No notes yet.
            <br />
            Press <kbd>{notesHotkeyLabel(settings?.notesHotkey)}</kbd> for a
            note, or <kbd>{callHotkeyLabel(settings?.callHotkey)}</kbd> for a
            phone call. Press again to stop.
          </div>
        ) : (
          <ul className="list">
            {list.map((n) => (
              <li
                key={n.id}
                className={`note-row${selected?.id === n.id ? " selected" : ""}`}
                onClick={async () => setSelected(await api.getNote(n.id))}
              >
                <div className="note-row-title">
                  {n.kind === "call" && <span className="kind-badge">Call</span>}
                  {n.title}
                </div>
                <div className="meta">
                  {summaryState[n.id] === "summarising" && (
                    <span className="summarising">
                      <span className="dot-spin" /> Summarising
                    </span>
                  )}
                  <span>{new Date(n.createdAt).toLocaleString()}</span>
                  {n.durationMs !== null && (
                    <span>{formatDuration(n.durationMs)}</span>
                  )}
                  {n.actionCount > 0 && (
                    <span>
                      {n.actionDone}/{n.actionCount} done
                    </span>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      {selected ? (
        <div className="card">
          {toast && <div className="banner">{toast}</div>}

          {selected.needsSummary && summaryState[selected.id] === "summarising" && (
            <div className="banner">
              <span className="dot-spin" style={{ marginRight: 8 }} />
              <strong>Summarising…</strong> The transcript is saved. The
              summary and action items will fill in when it finishes.
            </div>
          )}

          {selected.needsSummary && summaryState[selected.id] !== "summarising" && (
            <div className="banner" data-tone="warn">
              <strong>Not summarised yet.</strong> The transcript was saved but
              the summariser was unavailable — often a temporary spike.
              <button
                className="ghost"
                style={{ marginLeft: 12, padding: "4px 10px" }}
                disabled={retrying}
                onClick={async () => {
                  setRetrying(true);
                  try {
                    const updated = await api.retryNoteSummary(selected.id);
                    if (updated) {
                      setSelected(updated);
                      setList((prev) =>
                        prev.map((n) =>
                          n.id === updated.id ? { ...n, title: updated.title } : n
                        )
                      );
                    }
                  } catch (e) {
                    flash(String(e));
                  } finally {
                    setRetrying(false);
                  }
                }}
              >
                {retrying ? "Summarising…" : "Summarise now"}
              </button>
            </div>
          )}

          <div className="note-head">
            <h1 style={{ margin: 0 }}>
              {selected.kind === "call" && (
                <span className="kind-badge">Call</span>
              )}
              {selected.title}
              {selected.counterparty && (
                <span className="muted" style={{ fontWeight: 400 }}>
                  {" "}
                  — with {selected.counterparty}
                </span>
              )}
            </h1>
            <span className="muted">
              {new Date(selected.createdAt).toLocaleDateString(undefined, {
                month: "short",
                day: "numeric",
                year: "numeric",
              })}
            </span>
          </div>

          {selected.summary && (
            <div className="summary-card">
              <strong>Summary</strong>
              <p style={{ margin: "6px 0 0" }}>{selected.summary}</p>
            </div>
          )}

          {selected.takeaways.length > 0 && (
            <>
              <h2 style={{ marginTop: 22 }}>Key takeaways</h2>
              <div className="chips">
                {selected.takeaways.map((t, i) => (
                  <span className="chip" key={i}>
                    {t}
                  </span>
                ))}
              </div>
            </>
          )}

          <h2 style={{ marginTop: 22 }}>Action items</h2>
          {selected.actionItems.length === 0 ? (
            <p className="hint">Nothing was committed to in this note.</p>
          ) : (
            <ul className="checklist">
              {selected.actionItems.map((a) => (
                <li key={a.id}>
                  <label>
                    <input
                      type="checkbox"
                      checked={a.done}
                      onChange={(e) => toggle(a.id, e.target.checked)}
                    />
                    <span className={a.done ? "done" : ""}>{a.text}</span>
                  </label>
                </li>
              ))}
            </ul>
          )}

          {selected.openQuestions.length > 0 && (
            <>
              <h2 style={{ marginTop: 22 }}>Check from memory</h2>
              <p className="hint" style={{ marginBottom: 10 }}>
                Points where you were clearly responding to something the
                recording did not capture. Worth resolving while the call is
                fresh.
              </p>
              <ul className="checklist">
                {selected.openQuestions.map((q, i) => (
                  <li key={i}>
                    <span className="open-q">{q}</span>
                  </li>
                ))}
              </ul>
            </>
          )}

          {selected.inferred.length > 0 && (
            <>
              <h2 style={{ marginTop: 22 }}>Inferred, not stated</h2>
              <p className="hint" style={{ marginBottom: 10 }}>
                Read out of your own words rather than heard directly. Treat as
                a guess, not a record.
              </p>
              <ul className="checklist">
                {selected.inferred.map((t, i) => (
                  <li key={i}>
                    <span className="inferred">{t}</span>
                  </li>
                ))}
              </ul>
            </>
          )}

          {selected.notable.trim() && (
            <>
              <h2 style={{ marginTop: 22 }}>Notable</h2>
              <pre className="notable">{selected.notable.trim()}</pre>
            </>
          )}

          {selected.audioPath && (
            <div style={{ marginTop: 22 }}>
              <h2>Recording</h2>
              <AudioPlayer noteId={selected.id} />
            </div>
          )}

          <details style={{ marginTop: 18 }}>
            <summary className="muted" style={{ cursor: "pointer" }}>
              Full transcript
            </summary>
            <p className="hint" style={{ marginTop: 10, marginBottom: 0 }}>
              {selected.transcript}
            </p>
          </details>

          <div className="note-actions">
            <button
              className="ghost"
              onClick={async () => {
                await navigator.clipboard.writeText(noteToMarkdown(selected));
                flash("Markdown copied.");
              }}
            >
              Copy Markdown
            </button>
            <button
              className="primary"
              onClick={async () => {
                try {
                  await api.sendToVsCode(noteToMarkdown(selected));
                  flash("Opened in VS Code.");
                } catch (e) {
                  flash(String(e));
                }
              }}
            >
              Send to VS Code
            </button>
            <button className="danger" onClick={() => remove(selected.id)}>
              Delete
            </button>
          </div>
        </div>
      ) : (
        <div className="card">
          <div className="empty">Select a note, or record one.</div>
        </div>
      )}
    </div>
  );
}

function formatDuration(ms: number): string {
  const total = Math.round(ms / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

function callHotkeyLabel(hotkey: string | undefined): string {
  switch (hotkey) {
    case "CtrlShiftK":
      return "Ctrl+Shift+K";
    case "CtrlShiftSemicolon":
      return "Ctrl+Shift+;";
    case "F22":
      return "F22";
    default:
      return "Ctrl+Shift+'";
  }
}

function notesHotkeyLabel(hotkey: string | undefined): string {
  switch (hotkey) {
    case "CtrlShiftQuote":
      return "Ctrl+Shift+'";
    case "F23":
      return "F23";
    default:
      return "Ctrl+Shift+;";
  }
}
