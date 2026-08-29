import { useEffect, useState } from "react";
import { api, type DataStats, type InputDevice, type Settings } from "../lib/api";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/// Verified against Google's published model list.
const NOTES_MODELS: { id: string; note?: string }[] = [
  { id: "gemini-3.5-flash-lite", note: "fastest, cheapest" },
  { id: "gemini-3.5-flash" },
  { id: "gemini-3.6-flash" },
  { id: "gemini-3.7-flash", note: "most capable" },
  { id: "gemini-2.5-flash-lite" },
  { id: "gemini-2.5-flash" },
];

export function SettingsView() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [devices, setDevices] = useState<InputDevice[]>([]);
  const [hasKey, setHasKey] = useState(false);
  const [keyDraft, setKeyDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [stats, setStats] = useState<DataStats | null>(null);
  // Destructive actions take two clicks rather than a dialog: the webview's
  // native confirm() is not reliably available, and a second deliberate click
  // is a clearer gate than a modal people dismiss by reflex.
  const [confirming, setConfirming] = useState<string | null>(null);
  const [recovering, setRecovering] = useState(false);

  useEffect(() => {
    api.getSettings().then(setSettings).catch(() => {});
    api.listInputDevices().then(setDevices).catch(() => {});
    api.hasApiKey().then(setHasKey).catch(() => {});
    api.dataStats().then(setStats).catch(() => {});
  }, []);

  async function runDestructive(
    id: string,
    action: () => Promise<number>,
    describe: (n: number) => string
  ) {
    if (confirming !== id) {
      setConfirming(id);
      // Reverts on its own so a half-pressed action does not stay armed.
      setTimeout(() => setConfirming((c) => (c === id ? null : c)), 5000);
      return;
    }
    setConfirming(null);
    try {
      const removed = await action();
      setNote(describe(removed));
      setStats(await api.dataStats());
    } catch (e) {
      setNote(String(e));
    }
  }

  async function patch(changes: Partial<Settings>) {
    if (!settings) return;
    const next = { ...settings, ...changes };
    setSettings(next);
    await api.saveSettings(next);
  }

  async function saveKey() {
    if (!keyDraft.trim()) return;
    setBusy(true);
    try {
      await api.setApiKey(keyDraft.trim());
      setKeyDraft("");
      setHasKey(true);
      setNote("Key saved to Windows Credential Manager.");
    } catch (e) {
      setNote(`Could not save key: ${e}`);
    } finally {
      setBusy(false);
    }
  }

  async function clearKey() {
    await api.clearApiKey();
    setHasKey(false);
    setNote("Key removed.");
  }

  if (!settings) return <div className="card">Loading…</div>;

  return (
    <div className="card">
      <h1>Settings</h1>

      {note && <div className="banner">{note}</div>}

      <h2>Gemini API key</h2>
      <p className="hint">
        Stored in Windows Credential Manager, not in a config file. Get one from
        Google AI Studio.
      </p>
      <div className="field">
        {hasKey ? (
          <div className="row">
            <span className="status" data-state="idle">
              <span className="pip" /> Key is set
            </span>
            <button className="danger" onClick={clearKey}>
              Remove
            </button>
          </div>
        ) : (
          <div className="row">
            <input
              type="password"
              value={keyDraft}
              placeholder="Paste your API key"
              onChange={(e) => setKeyDraft(e.target.value)}
              autoComplete="off"
            />
            <button
              className="primary"
              onClick={saveKey}
              disabled={busy || !keyDraft.trim()}
            >
              Save
            </button>
          </div>
        )}
      </div>

      <h2>Dictation</h2>

      <div className="field">
        <label htmlFor="hotkey">Hotkey</label>
        <select
          id="hotkey"
          value={settings.hotkey}
          onChange={(e) => patch({ hotkey: e.target.value })}
        >
          <option value="RightCtrl">Right Ctrl</option>
          <option value="RightAlt">Right Alt</option>
          <option value="RightShift">Right Shift</option>
          <option value="F24">F24</option>
        </select>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          Held down to dictate. This key is swallowed while GeminiFlow runs and
          will not reach other apps.
        </p>
      </div>

      <div className="field">
        <label htmlFor="device">Microphone</label>
        <select
          id="device"
          value={settings.inputDevice ?? ""}
          onChange={(e) => patch({ inputDevice: e.target.value || null })}
        >
          <option value="">System default</option>
          {devices.map((d) => (
            <option key={d.name} value={d.name}>
              {d.name}
              {d.isDefault ? " (default)" : ""}
            </option>
          ))}
        </select>
      </div>

      <div className="field">
        <label htmlFor="paste">Paste method</label>
        <select
          id="paste"
          value={settings.pasteMode}
          onChange={(e) =>
            patch({ pasteMode: e.target.value as Settings["pasteMode"] })
          }
        >
          <option value="ctrlV">Ctrl+V (recommended)</option>
          <option value="shiftInsert">Shift+Insert (terminals)</option>
          <option value="typeUnicode">Type it out (most compatible)</option>
        </select>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          The first two put the text on the clipboard and send a paste shortcut.
          Ctrl+V works in most apps including Electron ones like Claude and
          Slack; Shift+Insert is the one that works in terminals such as Git
          Bash. If text lands nowhere at all, choose <strong>Type it out</strong>,
          which sends characters directly and needs no clipboard — slower on
          long dictations, but nothing can ignore it.
        </p>
      </div>

      <div className="field">
        <label className="row" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={settings.useLive}
            style={{ width: "auto" }}
            onChange={(e) => patch({ useLive: e.target.checked })}
          />
          <span>Stream audio while speaking</span>
        </label>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          <strong>On</strong> uses the live streaming model: usually ~0.5 s from
          release to text, but the session sometimes fails to start and falls
          back, which costs ~6 s. <strong>Off</strong> uploads the clip after you
          release and uses smart transcription: a steady ~3 s every time, with
          no live preview. Turn it off if you would rather have predictable
          speed than an unreliable fast path.
        </p>
      </div>

      <div className="field">
        <label className="row" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={settings.trailingSpace}
            style={{ width: "auto" }}
            onChange={(e) => patch({ trailingSpace: e.target.checked })}
          />
          <span>Add a space after each dictation</span>
        </label>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          Keeps back-to-back dictations from running together. Turn it off if
          you dictate into code and trailing whitespace is a problem.
        </p>
      </div>

      <h2 style={{ marginTop: 26 }}>Notes</h2>

      <div className="field">
        <label htmlFor="notes-hotkey">Notes hotkey</label>
        <select
          id="notes-hotkey"
          value={settings.notesHotkey}
          onChange={(e) => patch({ notesHotkey: e.target.value })}
        >
          <option value="CtrlShiftSemicolon">Ctrl+Shift+;</option>
          <option value="CtrlShiftQuote">Ctrl+Shift+'</option>
          <option value="F23">F23</option>
        </select>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          Press once to start recording a note, again to stop. Unlike dictation
          this is a toggle — you will not hold a key for ten minutes.
        </p>
      </div>

      <div className="row" style={{ alignItems: "flex-start", gap: 16 }}>
        <div className="field" style={{ flex: 1 }}>
          <label htmlFor="note-max">Stop after (minutes)</label>
          <input
            id="note-max"
            type="number"
            min={1}
            max={180}
            value={settings.noteMaxMinutes}
            onChange={(e) =>
              patch({ noteMaxMinutes: Number(e.target.value) || 60 })
            }
          />
        </div>
        <div className="field" style={{ flex: 1 }}>
          <label htmlFor="note-silence">Stop after silence (minutes)</label>
          <input
            id="note-silence"
            type="number"
            min={1}
            max={60}
            value={settings.noteSilenceMinutes}
            onChange={(e) =>
              patch({ noteSilenceMinutes: Number(e.target.value) || 2 })
            }
          />
        </div>
      </div>
      <p className="hint" style={{ marginTop: -6 }}>
        Safety nets for a toggle you forget to switch off. Whichever comes
        first stops the recording and writes the note up as normal.
      </p>

      <div className="field">
        <label htmlFor="notes-model">Summary model</label>
        <select
          id="notes-model"
          value={settings.notesModel}
          onChange={(e) => patch({ notesModel: e.target.value })}
        >
          {NOTES_MODELS.map((m) => (
            <option key={m.id} value={m.id}>
              {m.id}
              {m.note ? ` — ${m.note}` : ""}
            </option>
          ))}
          {/* A model set previously that is no longer in the list must still
              show, or opening Settings would silently change it. */}
          {!NOTES_MODELS.some((m) => m.id === settings.notesModel) && (
            <option value={settings.notesModel}>{settings.notesModel}</option>
          )}
        </select>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          Turns a note transcript into its summary and action items.
          Transcription always uses the dedicated speech model and is not
          affected by this setting. Worth switching when a model hits sustained
          “high demand” errors — changing is faster than waiting for capacity.
          Takes effect immediately, including for notes already queued.
        </p>
      </div>

      <h2 style={{ marginTop: 26 }}>Phone calls</h2>

      <div className="field">
        <label htmlFor="call-hotkey">Call hotkey</label>
        <select
          id="call-hotkey"
          value={settings.callHotkey}
          onChange={(e) => patch({ callHotkey: e.target.value })}
        >
          <option value="CtrlShiftQuote">Ctrl+Shift+'</option>
          <option value="CtrlShiftK">Ctrl+Shift+K</option>
          <option value="CtrlShiftSemicolon">Ctrl+Shift+;</option>
          <option value="F22">F22</option>
        </select>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          A toggle, like notes. Calls are written up differently: your
          microphone only hears your half, so the summary is told never to
          invent the other person's words, and it lists what it inferred and
          what you should check from memory.
        </p>
      </div>

      <div className="field">
        <label className="row" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={settings.speakerphone}
            style={{ width: "auto" }}
            onChange={(e) => patch({ speakerphone: e.target.checked })}
          />
          <span>I use speakerphone</span>
        </label>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          <strong>This records the other party, not just you.</strong> With it
          off, only your own voice is captured and calls are written up on that
          basis. With it on, the microphone picks up everyone audible in the
          room and labels them by speaker.
        </p>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          Recording someone else may require their consent depending on where
          you and they are. Turn this on when you have it, not by default. It
          also reduces the maximum call length from 60 to 30 minutes, because
          speaker labelling halves the audio the model will accept.
        </p>
      </div>

      <div className="field">
        <label className="row" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={settings.prebufferEnabled}
            style={{ width: "auto" }}
            onChange={(e) => patch({ prebufferEnabled: e.target.checked })}
          />
          <span>Keep the last few seconds of audio ready</span>
        </label>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          Calls start before you can reach the keyboard. With this on,
          GeminiFlow keeps a rolling window of recent microphone audio so
          starting a recording part-way through still captures the opening.
        </p>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          <strong>This holds your microphone open while the app is idle.</strong>{" "}
          The audio stays in memory only, is continuously discarded, and is
          never written to disk unless you actually start a recording. Windows
          will show the microphone-in-use indicator the whole time it is on.
        </p>
      </div>

      {settings.prebufferEnabled && (
        <div className="field">
          <label htmlFor="prebuffer-seconds">Seconds to keep</label>
          <input
            id="prebuffer-seconds"
            type="number"
            min={5}
            max={120}
            value={settings.prebufferSeconds}
            onChange={(e) =>
              patch({ prebufferSeconds: Number(e.target.value) || 30 })
            }
          />
        </div>
      )}

      <h2 style={{ marginTop: 26 }}>Your data</h2>

      <p className="hint">
        Notes, calls, dictation history, settings and vocabulary live in a
        database on this machine; note audio is kept alongside it. Nothing is
        stored anywhere else, and none of it is removed by uninstalling or
        reinstalling the app — that only replaces the program.
      </p>

      {stats && (
        <div className="field">
          <ul className="list">
            <li>
              <strong>{stats.folder}</strong>
              <div className="meta">
                <span>database {formatBytes(stats.databaseBytes)}</span>
                <span>
                  audio {formatBytes(stats.audioBytes)} in {stats.audioFiles}{" "}
                  file{stats.audioFiles === 1 ? "" : "s"}
                </span>
                <span>{stats.noteCount} notes and calls</span>
                <span>{stats.dictationCount} dictations</span>
              </div>
            </li>
          </ul>
        </div>
      )}

      <div className="row" style={{ flexWrap: "wrap", marginBottom: 18 }}>
        <button className="ghost" onClick={() => api.openDataFolder()}>
          Open folder
        </button>

        <button
          className="ghost"
          disabled={recovering}
          onClick={async () => {
            setRecovering(true);
            setNote("Looking for recordings without a note…");
            try {
              const n = await api.recoverRecordings();
              setNote(
                n === 0
                  ? "Nothing to recover — every recording already has a note."
                  : `Recovered ${n} recording${n === 1 ? "" : "s"}. Open each and press Summarise now.`
              );
              setStats(await api.dataStats());
            } catch (e) {
              setNote(String(e));
            } finally {
              setRecovering(false);
            }
          }}
        >
          {recovering ? "Recovering…" : "Recover lost recordings"}
        </button>

        <button
          className="danger"
          onClick={() =>
            runDestructive(
              "dictations",
              () => api.clearDictations(),
              (n) => `Cleared ${n} dictation${n === 1 ? "" : "s"}.`
            )
          }
        >
          {confirming === "dictations"
            ? "Click again to clear dictation history"
            : "Clear dictation history"}
        </button>

        <button
          className="danger"
          onClick={() =>
            runDestructive(
              "notes",
              () => api.deleteAllNotes(),
              (n) => `Deleted ${n} note${n === 1 ? "" : "s"} and their audio.`
            )
          }
        >
          {confirming === "notes"
            ? "Click again to delete every note"
            : "Delete all notes and calls"}
        </button>
      </div>

      <p className="hint">
        <strong>Recover lost recordings</strong> rebuilds notes from audio files
        the database no longer references — useful if notes go missing but the
        recordings are still on disk. Each is re-transcribed and saved
        unsummarised, keeping its original date.
      </p>

      <p className="hint">
        Deleting is permanent — there is no undo and nothing goes to the
        Recycle Bin. Your API key is stored separately in Windows Credential
        Manager and is not affected; remove it with the button at the top.
      </p>

      <h2 style={{ marginTop: 26 }}>Startup</h2>

      <div className="field">
        <label className="row" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={settings.launchAtLogin}
            style={{ width: "auto" }}
            onChange={(e) => patch({ launchAtLogin: e.target.checked })}
          />
          <span>Start GeminiFlow when I sign in to Windows</span>
        </label>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          Launches straight to the tray, so the hotkeys work without opening
          anything. Registered for your account only — no admin rights needed.
        </p>
      </div>

      <div className="field">
        <label className="row" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={settings.startHidden}
            style={{ width: "auto" }}
            onChange={(e) => patch({ startHidden: e.target.checked })}
          />
          <span>Always start hidden</span>
        </label>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          Hides the window on every launch, not just at login. Reopen it from
          the tray icon.
        </p>
      </div>

      <div className="field">
        <label className="row" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={settings.debugLogging}
            style={{ width: "auto" }}
            onChange={(e) => patch({ debugLogging: e.target.checked })}
          />
          <span>Verbose diagnostics</span>
        </label>
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          Writes hotkey counters every few seconds and every streaming frame to
          the log. Worth turning on only when chasing a specific problem.
        </p>
      </div>

      <h2 style={{ marginTop: 26 }}>History</h2>

      <div className="field">
        <label htmlFor="retention">Keep history for (days)</label>
        <input
          id="retention"
          type="number"
          min={0}
          value={settings.retentionDays}
          onChange={(e) =>
            patch({ retentionDays: Number(e.target.value) || 0 })
          }
        />
        <p className="hint" style={{ marginTop: 6, marginBottom: 0 }}>
          0 keeps everything forever.
        </p>
      </div>
    </div>
  );
}
