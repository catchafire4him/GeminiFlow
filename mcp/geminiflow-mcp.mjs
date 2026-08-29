#!/usr/bin/env node
/**
 * Read-only MCP server over the GeminiFlow database.
 *
 * Deliberately read-only. An agent that can delete or rewrite your notes is a
 * worse trade than one that can only read them, so the database is opened with
 * `readonly: true` and there is no tool that mutates anything.
 *
 * No dependencies: Node 22+ ships `node:sqlite`, so there is nothing to
 * install and no native module to break when Node updates.
 *
 * The app keeps the database in WAL mode, which allows concurrent readers, so
 * this is safe to run while GeminiFlow is writing.
 */

import { DatabaseSync } from "node:sqlite";
import path from "node:path";
import process from "node:process";

const DB_PATH =
  process.env.GEMINIFLOW_DB ??
  path.join(
    process.env.APPDATA ?? path.join(process.env.HOME ?? ".", ".config"),
    "GeminiFlow",
    "geminiflow.db"
  );

let db;
function database() {
  // Opened lazily so a missing database surfaces as a tool error the agent can
  // report, rather than killing the server before it finishes handshaking.
  if (!db) db = new DatabaseSync(DB_PATH, { readOnly: true });
  return db;
}

// ---------------------------------------------------------------- queries --

function noteRows(where = "", params = [], limit = 20) {
  return database()
    .prepare(
      `SELECT n.id, n.kind, n.title, n.summary, n.counterparty, n.created_at,
              n.needs_summary, r.duration_ms
         FROM notes n LEFT JOIN recordings r ON r.id = n.recording_id
        ${where}
        ORDER BY n.id DESC
        LIMIT ?`
    )
    .all(...params, limit);
}

function decorate(row) {
  const lists = database()
    .prepare("SELECT text, kind FROM takeaways WHERE note_id = ? ORDER BY position, id")
    .all(row.id);

  return {
    id: row.id,
    kind: row.kind,
    title: row.title,
    summary: row.summary,
    counterparty: row.counterparty || undefined,
    created: row.created_at,
    durationSeconds: row.duration_ms ? Math.round(row.duration_ms / 1000) : undefined,
    summarised: !row.needs_summary,
    takeaways: lists.filter((t) => t.kind === "takeaway").map((t) => t.text),
    // Call-only, and meaningful: `inferred` was read out of the speaker's own
    // words rather than heard, and openQuestions are the gaps in a one-sided
    // recording. An agent should not treat either as established fact.
    inferred: lists.filter((t) => t.kind === "inferred").map((t) => t.text),
    openQuestions: lists.filter((t) => t.kind === "open_question").map((t) => t.text),
    actionItems: database()
      .prepare(
        "SELECT id, text, done FROM action_items WHERE note_id = ? ORDER BY position, id"
      )
      .all(row.id)
      .map((a) => ({ id: a.id, text: a.text, done: !!a.done })),
  };
}

// ------------------------------------------------------------------ tools --

const TOOLS = {
  recent_notes: {
    description:
      "List the most recent notes and phone calls, newest first. Use this to " +
      "see what the user has been working on or talking about lately.",
    schema: {
      type: "object",
      properties: {
        limit: { type: "number", description: "How many to return (default 10, max 100)" },
        kind: {
          type: "string",
          enum: ["note", "call", "any"],
          description: "Restrict to spoken notes, phone calls, or both (default any)",
        },
      },
    },
    run: ({ limit = 10, kind = "any" }) => {
      const capped = Math.min(Math.max(1, limit), 100);
      const rows =
        kind === "any"
          ? noteRows("", [], capped)
          : noteRows("WHERE n.kind = ?", [kind], capped);
      return rows.map(decorate);
    },
  },

  search_notes: {
    description:
      "Full-text search across note titles, summaries and transcripts. Use " +
      "this to answer questions about what the user said on a topic.",
    schema: {
      type: "object",
      properties: {
        query: { type: "string", description: "Text to look for" },
        limit: { type: "number", description: "Max results (default 10)" },
      },
      required: ["query"],
    },
    run: ({ query, limit = 10 }) => {
      const like = `%${query}%`;
      return noteRows(
        "WHERE n.title LIKE ? OR n.summary LIKE ? OR n.transcript LIKE ?",
        [like, like, like],
        Math.min(Math.max(1, limit), 100)
      ).map(decorate);
    },
  },

  get_note: {
    description:
      "Fetch one note in full, including its complete transcript. Use after " +
      "search or recent_notes when the summary is not enough.",
    schema: {
      type: "object",
      properties: { id: { type: "number", description: "Note id" } },
      required: ["id"],
    },
    run: ({ id }) => {
      const row = noteRows("WHERE n.id = ?", [id], 1)[0];
      if (!row) throw new Error(`no note with id ${id}`);
      const transcript = database()
        .prepare("SELECT transcript FROM notes WHERE id = ?")
        .get(id).transcript;
      return { ...decorate(row), transcript };
    },
  },

  open_action_items: {
    description:
      "Every unticked action item across all notes and calls, newest first. " +
      "Use this to answer what the user still needs to do.",
    schema: {
      type: "object",
      properties: {
        limit: { type: "number", description: "Max items (default 50)" },
      },
    },
    run: ({ limit = 50 }) =>
      database()
        .prepare(
          `SELECT a.id, a.text, n.id AS note_id, n.title, n.kind, n.created_at
             FROM action_items a JOIN notes n ON n.id = a.note_id
            WHERE a.done = 0
            ORDER BY n.id DESC, a.position
            LIMIT ?`
        )
        .all(Math.min(Math.max(1, limit), 500))
        .map((r) => ({
          id: r.id,
          text: r.text,
          fromNote: { id: r.note_id, title: r.title, kind: r.kind, created: r.created_at },
        })),
  },

  recent_dictations: {
    description:
      "Recent dictated text, newest first. This is what the user spoke " +
      "directly into other applications, not saved notes.",
    schema: {
      type: "object",
      properties: { limit: { type: "number", description: "Max results (default 20)" } },
    },
    run: ({ limit = 20 }) =>
      database()
        .prepare(
          `SELECT id, text, target_app, created_at
             FROM dictations ORDER BY id DESC LIMIT ?`
        )
        .all(Math.min(Math.max(1, limit), 200))
        .map((d) => ({
          id: d.id,
          text: d.text,
          intoApp: d.target_app || undefined,
          created: d.created_at,
        })),
  },
};

// ----------------------------------------------------------- MCP plumbing --

function reply(id, result) {
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
}

function replyError(id, message) {
  process.stdout.write(
    JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32603, message } }) + "\n"
  );
}

function handle(message) {
  const { id, method, params } = message;

  // Notifications carry no id and must not be answered.
  if (id === undefined) return;

  switch (method) {
    case "initialize":
      return reply(id, {
        protocolVersion: params?.protocolVersion ?? "2024-11-05",
        capabilities: { tools: {} },
        serverInfo: { name: "geminiflow", version: "0.1.0" },
      });

    case "tools/list":
      return reply(id, {
        tools: Object.entries(TOOLS).map(([name, t]) => ({
          name,
          description: t.description,
          inputSchema: t.schema,
        })),
      });

    case "tools/call": {
      const tool = TOOLS[params?.name];
      if (!tool) return replyError(id, `unknown tool: ${params?.name}`);
      try {
        const result = tool.run(params.arguments ?? {});
        return reply(id, {
          content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
        });
      } catch (e) {
        // Reported as tool output rather than a protocol error so the agent
        // can read the reason and adapt.
        return reply(id, {
          content: [{ type: "text", text: `Error: ${e.message}` }],
          isError: true,
        });
      }
    }

    case "ping":
      return reply(id, {});

    default:
      return replyError(id, `unsupported method: ${method}`);
  }
}

// Line-delimited JSON-RPC on stdin. Buffered because a message can arrive
// split across reads.
let buffer = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let newline;
  while ((newline = buffer.indexOf("\n")) !== -1) {
    const line = buffer.slice(0, newline).trim();
    buffer = buffer.slice(newline + 1);
    if (!line) continue;
    try {
      handle(JSON.parse(line));
    } catch {
      // A malformed line has no id to answer against; skipping beats crashing.
    }
  }
});

process.stdin.on("end", () => process.exit(0));
