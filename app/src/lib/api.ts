import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type EngineState =
  | "idle"
  | "arming"
  | "recording"
  | "finalizing"
  | "injecting"
  | "error"
  | "noteRecording"
  | "noteProcessing"
  | "callRecording";

export interface ActionItem {
  id: number;
  text: string;
  done: boolean;
}

export interface Note {
  id: number;
  title: string;
  summary: string;
  notable: string;
  transcript: string;
  createdAt: string;
  durationMs: number | null;
  audioPath: string | null;
  needsSummary: boolean;
  kind: string;
  counterparty: string;
  inferred: string[];
  openQuestions: string[];
  takeaways: string[];
  actionItems: ActionItem[];
}

export interface NoteSummary {
  id: number;
  title: string;
  createdAt: string;
  durationMs: number | null;
  kind: string;
  actionCount: number;
  actionDone: number;
}

export interface DataStats {
  folder: string;
  databaseBytes: number;
  audioBytes: number;
  audioFiles: number;
  dictationCount: number;
  noteCount: number;
}

export interface OrphanInfo {
  count: number;
  bytes: number;
}

export interface Settings {
  hotkey: string;
  inputDevice: string | null;
  pasteMode: "shiftInsert" | "ctrlV";
  useLive: boolean;
  language: string;
  retentionDays: number;
  trailingSpace: boolean;
  notesHotkey: string;
  noteMaxMinutes: number;
  noteSilenceMinutes: number;
  notesModel: string;
  callHotkey: string;
  speakerphone: boolean;
  prebufferEnabled: boolean;
  prebufferSeconds: number;
  soundsEnabled: boolean;
  launchAtLogin: boolean;
  startHidden: boolean;
  debugLogging: boolean;
}

export interface InputDevice {
  name: string;
  isDefault: boolean;
}

export interface Dictation {
  id: number;
  text: string;
  targetApp: string | null;
  latencyMs: number | null;
  injectedOk: boolean;
  createdAt: string;
}

export interface StatusEvent {
  state: EngineState;
  detail: string | null;
  /** Live partial transcript, when the engine is mid-utterance. */
  partial: string | null;
}

export const api = {
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) =>
    invoke<void>("save_settings", { settings }),

  listInputDevices: () => invoke<InputDevice[]>("list_input_devices"),

  getVocabulary: () => invoke<string[]>("get_vocabulary"),
  saveVocabulary: (terms: string[]) =>
    invoke<void>("save_vocabulary", { terms }),

  listDictations: (limit: number) =>
    invoke<Dictation[]>("list_dictations", { limit }),
  deleteDictation: (id: number) => invoke<void>("delete_dictation", { id }),

  hasApiKey: () => invoke<boolean>("has_api_key"),
  setApiKey: (key: string) => invoke<void>("set_api_key", { key }),
  clearApiKey: () => invoke<void>("clear_api_key"),

  getState: () => invoke<StatusEvent>("get_state"),

  dataStats: () => invoke<DataStats>("data_stats"),
  openDataFolder: () => invoke<void>("open_data_folder"),
  recoverRecordings: () => invoke<number>("recover_recordings"),
  orphanedAudio: () => invoke<OrphanInfo>("orphaned_audio"),
  noteAudio: (id: number) => invoke<string>("note_audio", { id }),
  readLog: (lines: number) => invoke<string>("read_log", { lines }),
  deleteOrphanedAudio: () => invoke<number>("delete_orphaned_audio"),
  clearDictations: () => invoke<number>("clear_dictations"),
  deleteAllNotes: () => invoke<number>("delete_all_notes"),

  listNotes: (limit: number) => invoke<NoteSummary[]>("list_notes", { limit }),
  getNote: (id: number) => invoke<Note | null>("get_note", { id }),
  setActionDone: (id: number, done: boolean) =>
    invoke<void>("set_action_done", { id, done }),
  deleteNote: (id: number) => invoke<void>("delete_note", { id }),
  retryNoteSummary: (id: number) =>
    invoke<Note | null>("retry_note_summary", { id }),
  sendToVsCode: (markdown: string) =>
    invoke<void>("send_to_vscode", { markdown }),
};

export interface SummaryStatus {
  id: number;
  state: "summarising" | "failed";
}

export function onSummaryStatus(
  handler: (s: SummaryStatus) => void
): Promise<UnlistenFn> {
  return listen<SummaryStatus>("engine://note-summary", (e) => handler(e.payload));
}

export function onNote(handler: (n: Note) => void): Promise<UnlistenFn> {
  return listen<Note>("engine://note", (event) => handler(event.payload));
}

/// Markdown for copying or handing to VS Code.
export function noteToMarkdown(note: Note): string {
  const lines = [`# ${note.title}`, ""];
  lines.push(`_${new Date(note.createdAt).toLocaleString()}_`, "");

  if (note.summary) lines.push(note.summary, "");

  if (note.takeaways.length) {
    lines.push("## Key takeaways", "");
    note.takeaways.forEach((t) => lines.push(`- ${t}`));
    lines.push("");
  }

  if (note.actionItems.length) {
    lines.push("## Action items", "");
    note.actionItems.forEach((a) =>
      lines.push(`- [${a.done ? "x" : " "}] ${a.text}`)
    );
    lines.push("");
  }

  if (note.openQuestions.length) {
    lines.push("## Open questions", "");
    note.openQuestions.forEach((q) => lines.push(`- ${q}`));
    lines.push("");
  }

  if (note.inferred.length) {
    lines.push("## Inferred (not directly stated)", "");
    note.inferred.forEach((i) => lines.push(`- ${i}`));
    lines.push("");
  }

  if (note.notable.trim()) {
    lines.push("## Notable", "", "```", note.notable.trim(), "```", "");
  }

  lines.push("<details><summary>Full transcript</summary>", "", note.transcript, "", "</details>");
  return lines.join("\n");
}

export function onStatus(handler: (e: StatusEvent) => void): Promise<UnlistenFn> {
  return listen<StatusEvent>("engine://status", (event) => handler(event.payload));
}

export function onDictation(handler: (d: Dictation) => void): Promise<UnlistenFn> {
  return listen<Dictation>("engine://dictation", (event) => handler(event.payload));
}
