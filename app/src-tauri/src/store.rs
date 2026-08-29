//! SQLite persistence under %APPDATA%/GeminiFlow.
//!
//! The schema carries the M3 tables (notes, calls) from the start so adding
//! them later is not a migration. M1 only writes `dictations`, `vocab_terms`
//! and `settings`.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Dictation {
    pub id: i64,
    pub text: String,
    pub target_app: Option<String>,
    pub latency_ms: Option<i64>,
    pub injected_ok: bool,
    pub created_at: String,
}

/// What the structuring model returns, before it is persisted.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct NoteDraft {
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub takeaways: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<String>,
    #[serde(default)]
    pub notable: String,
    /// Call-only. Empty for ordinary notes.
    #[serde(default)]
    pub counterparty: String,
    #[serde(default)]
    pub inferred: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ActionItem {
    pub id: i64,
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: i64,
    pub title: String,
    pub summary: String,
    pub notable: String,
    pub transcript: String,
    pub created_at: String,
    pub duration_ms: Option<i64>,
    pub audio_path: Option<String>,
    /// 'note' or 'call'.
    pub kind: String,
    pub counterparty: String,
    pub inferred: Vec<String>,
    pub open_questions: Vec<String>,
    /// True when structuring failed and only the raw transcript was saved.
    /// The note is still usable and can be summarised later.
    pub needs_summary: bool,
    pub takeaways: Vec<String>,
    pub action_items: Vec<ActionItem>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: i64,
    pub title: String,
    pub created_at: String,
    pub duration_ms: Option<i64>,
    pub kind: String,
    pub action_count: i64,
    pub action_done: i64,
}

pub fn recordings_dir() -> Result<PathBuf> {
    let dir = data_dir()?.join("recordings");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .ok_or_else(|| anyhow!("could not locate %APPDATA%"))?
        .join("GeminiFlow");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

impl Store {
    pub fn open() -> Result<Store> {
        let path = data_dir()?.join("geminiflow.db");
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Store {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS vocab_terms (
                id      INTEGER PRIMARY KEY AUTOINCREMENT,
                term    TEXT NOT NULL UNIQUE,
                enabled INTEGER NOT NULL DEFAULT 1
            );

            -- kind: 'dictation' | 'note' | 'call'
            CREATE TABLE IF NOT EXISTS recordings (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                kind           TEXT NOT NULL,
                created_at     TEXT NOT NULL,
                duration_ms    INTEGER,
                audio_path     TEXT,
                sample_rate    INTEGER,
                source         TEXT,
                speakerphone   INTEGER NOT NULL DEFAULT 0,
                prebuffered_ms INTEGER NOT NULL DEFAULT 0,
                stop_reason    TEXT
            );

            CREATE TABLE IF NOT EXISTS dictations (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                recording_id INTEGER REFERENCES recordings(id) ON DELETE SET NULL,
                text         TEXT NOT NULL,
                target_app   TEXT,
                injected_ok  INTEGER NOT NULL DEFAULT 1,
                latency_ms   INTEGER,
                created_at   TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_dictations_created
                ON dictations(created_at DESC);

            CREATE TABLE IF NOT EXISTS notes (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                recording_id INTEGER REFERENCES recordings(id) ON DELETE SET NULL,
                kind         TEXT NOT NULL DEFAULT 'note',
                title        TEXT NOT NULL,
                summary      TEXT NOT NULL DEFAULT '',
                notable      TEXT NOT NULL DEFAULT '',
                transcript   TEXT NOT NULL DEFAULT '',
                created_at   TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_notes_created ON notes(created_at DESC);

            CREATE TABLE IF NOT EXISTS takeaways (
                id       INTEGER PRIMARY KEY AUTOINCREMENT,
                note_id  INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                text     TEXT NOT NULL,
                position INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS action_items (
                id       INTEGER PRIMARY KEY AUTOINCREMENT,
                note_id  INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                text     TEXT NOT NULL,
                done     INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS usage (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                day          TEXT NOT NULL,
                model        TEXT NOT NULL,
                seconds      REAL NOT NULL DEFAULT 0,
                est_cost_usd REAL NOT NULL DEFAULT 0
            );
            "#,
        )?;

        // Added after the notes table shipped. CREATE TABLE IF NOT EXISTS will
        // not alter an existing table, so this needs its own ALTER; an error
        // here just means the column is already present.
        for stmt in [
            "ALTER TABLE notes ADD COLUMN needs_summary INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE notes ADD COLUMN counterparty TEXT NOT NULL DEFAULT ''",
            // kind: takeaway | inferred | open_question. Calls need all three;
            // ordinary notes only ever write takeaways.
            "ALTER TABLE takeaways ADD COLUMN kind TEXT NOT NULL DEFAULT 'takeaway'",
        ] {
            let _ = conn.execute(stmt, []);
        }

        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn vocabulary(&self) -> Vec<String> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        let Ok(mut stmt) =
            conn.prepare("SELECT term FROM vocab_terms WHERE enabled = 1 ORDER BY id")
        else {
            return Vec::new();
        };
        stmt.query_map([], |row| row.get::<_, String>(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn set_vocabulary(&self, terms: &[String]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM vocab_terms", [])?;
        {
            let mut stmt =
                tx.prepare("INSERT OR IGNORE INTO vocab_terms (term) VALUES (?1)")?;
            // The API caps custom vocabulary at 1,000 terms.
            for term in terms.iter().take(1000) {
                let trimmed = term.trim();
                if !trimmed.is_empty() {
                    stmt.execute(params![trimmed])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_dictation(
        &self,
        text: &str,
        target_app: Option<&str>,
        latency_ms: Option<i64>,
        injected_ok: bool,
    ) -> Result<Dictation> {
        let created_at = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO dictations (text, target_app, injected_ok, latency_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![text, target_app, injected_ok as i32, latency_ms, created_at],
        )?;
        Ok(Dictation {
            id: conn.last_insert_rowid(),
            text: text.to_string(),
            target_app: target_app.map(str::to_string),
            latency_ms,
            injected_ok,
            created_at,
        })
    }

    pub fn list_dictations(&self, limit: i64) -> Vec<Dictation> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT id, text, target_app, latency_ms, injected_ok, created_at
             FROM dictations ORDER BY id DESC LIMIT ?1",
        ) else {
            return Vec::new();
        };
        stmt.query_map(params![limit], |row| {
            Ok(Dictation {
                id: row.get(0)?,
                text: row.get(1)?,
                target_app: row.get(2)?,
                latency_ms: row.get(3)?,
                injected_ok: row.get::<_, i32>(4)? != 0,
                created_at: row.get(5)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    pub fn delete_dictation(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM dictations WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn insert_note(
        &self,
        draft: &NoteDraft,
        transcript: &str,
        audio_path: Option<&str>,
        duration_ms: i64,
        needs_summary: bool,
        kind: &str,
        speakerphone: bool,
        prebuffered_ms: i64,
    ) -> Result<Note> {
        let created_at = chrono::Utc::now().to_rfc3339();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        tx.execute(
            "INSERT INTO recordings (kind, created_at, duration_ms, audio_path, sample_rate,
                                     source, speakerphone, prebuffered_ms)
             VALUES (?1, ?2, ?3, ?4, 16000, 'mic', ?5, ?6)",
            params![
                kind,
                created_at,
                duration_ms,
                audio_path,
                speakerphone as i32,
                prebuffered_ms
            ],
        )?;
        let recording_id = tx.last_insert_rowid();

        tx.execute(
            "INSERT INTO notes (recording_id, kind, title, summary, notable, transcript,
                                created_at, needs_summary, counterparty)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                recording_id,
                kind,
                draft.title,
                draft.summary,
                draft.notable,
                transcript,
                created_at,
                needs_summary as i32,
                draft.counterparty
            ],
        )?;
        let note_id = tx.last_insert_rowid();

        {
            let mut stmt = tx.prepare(
                "INSERT INTO takeaways (note_id, text, position, kind)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (i, text) in draft.takeaways.iter().enumerate() {
                stmt.execute(params![note_id, text, i as i64, "takeaway"])?;
            }
            for (i, text) in draft.inferred.iter().enumerate() {
                stmt.execute(params![note_id, text, i as i64, "inferred"])?;
            }
            for (i, text) in draft.open_questions.iter().enumerate() {
                stmt.execute(params![note_id, text, i as i64, "open_question"])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO action_items (note_id, text, position) VALUES (?1, ?2, ?3)",
            )?;
            for (i, text) in draft.action_items.iter().enumerate() {
                stmt.execute(params![note_id, text, i as i64])?;
            }
        }

        tx.commit()?;
        drop(conn);

        self.note(note_id)
            .ok_or_else(|| anyhow!("note vanished immediately after being written"))
    }

    pub fn note(&self, id: i64) -> Option<Note> {
        let conn = self.conn.lock().ok()?;

        let mut note = conn
            .query_row(
                "SELECT n.id, n.title, n.summary, n.notable, n.transcript, n.created_at,
                        r.duration_ms, r.audio_path, n.needs_summary, n.kind, n.counterparty
                 FROM notes n LEFT JOIN recordings r ON r.id = n.recording_id
                 WHERE n.id = ?1",
                params![id],
                |row| {
                    Ok(Note {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        summary: row.get(2)?,
                        notable: row.get(3)?,
                        transcript: row.get(4)?,
                        created_at: row.get(5)?,
                        duration_ms: row.get(6)?,
                        audio_path: row.get(7)?,
                        needs_summary: row.get::<_, i32>(8)? != 0,
                        kind: row.get(9)?,
                        counterparty: row.get(10)?,
                        inferred: Vec::new(),
                        open_questions: Vec::new(),
                        takeaways: Vec::new(),
                        action_items: Vec::new(),
                    })
                },
            )
            .ok()?;

        if let Ok(mut stmt) = conn.prepare(
            "SELECT text, kind FROM takeaways WHERE note_id = ?1 ORDER BY position, id",
        ) {
            if let Ok(rows) = stmt.query_map(params![id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }) {
                for (text, kind) in rows.flatten() {
                    match kind.as_str() {
                        "inferred" => note.inferred.push(text),
                        "open_question" => note.open_questions.push(text),
                        _ => note.takeaways.push(text),
                    }
                }
            }
        }

        if let Ok(mut stmt) = conn.prepare(
            "SELECT id, text, done FROM action_items WHERE note_id = ?1 ORDER BY position, id",
        ) {
            note.action_items = stmt
                .query_map(params![id], |row| {
                    Ok(ActionItem {
                        id: row.get(0)?,
                        text: row.get(1)?,
                        done: row.get::<_, i32>(2)? != 0,
                    })
                })
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default();
        }

        Some(note)
    }

    /// Summary rows for the sidebar; full detail is loaded per note.
    pub fn list_notes(&self, limit: i64) -> Vec<NoteSummary> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT n.id, n.title, n.created_at, r.duration_ms, n.kind,
                    (SELECT COUNT(*) FROM action_items a WHERE a.note_id = n.id),
                    (SELECT COUNT(*) FROM action_items a WHERE a.note_id = n.id AND a.done = 1)
             FROM notes n LEFT JOIN recordings r ON r.id = n.recording_id
             ORDER BY n.id DESC LIMIT ?1",
        ) else {
            return Vec::new();
        };
        stmt.query_map(params![limit], |row| {
            Ok(NoteSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                duration_ms: row.get(3)?,
                kind: row.get(4)?,
                action_count: row.get(5)?,
                action_done: row.get(6)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    /// Fills in a summary that failed at record time.
    pub fn apply_summary(&self, id: i64, draft: &NoteDraft) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        tx.execute(
            "UPDATE notes SET title = ?2, summary = ?3, notable = ?4, counterparty = ?5,
                              needs_summary = 0
             WHERE id = ?1",
            params![id, draft.title, draft.summary, draft.notable, draft.counterparty],
        )?;
        tx.execute("DELETE FROM takeaways WHERE note_id = ?1", params![id])?;
        tx.execute("DELETE FROM action_items WHERE note_id = ?1", params![id])?;

        {
            let mut stmt = tx.prepare(
                "INSERT INTO takeaways (note_id, text, position, kind)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (i, text) in draft.takeaways.iter().enumerate() {
                stmt.execute(params![id, text, i as i64, "takeaway"])?;
            }
            for (i, text) in draft.inferred.iter().enumerate() {
                stmt.execute(params![id, text, i as i64, "inferred"])?;
            }
            for (i, text) in draft.open_questions.iter().enumerate() {
                stmt.execute(params![id, text, i as i64, "open_question"])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO action_items (note_id, text, position) VALUES (?1, ?2, ?3)",
            )?;
            for (i, text) in draft.action_items.iter().enumerate() {
                stmt.execute(params![id, text, i as i64])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn transcript_of(&self, id: i64) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT transcript FROM notes WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .ok()
    }

    pub fn set_action_done(&self, id: i64, done: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE action_items SET done = ?2 WHERE id = ?1",
            params![id, done as i32],
        )?;
        Ok(())
    }

    pub fn delete_note(&self, id: i64) -> Result<()> {
        // Logged because notes disappearing with no record of why is a bad
        // place to end up, and we have been there.
        crate::logln!("[data] deleting note {id}");
        let conn = self.conn.lock().unwrap();

        // Remove the audio file too; leaving orphans behind would quietly fill
        // the disk over months of use.
        if let Ok(path) = conn.query_row(
            "SELECT r.audio_path FROM notes n JOIN recordings r ON r.id = n.recording_id
             WHERE n.id = ?1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        ) {
            if let Some(path) = path {
                // Was silently ignored, which let deleted notes leave their
                // audio behind and the folder grow without bound. Observed:
                // eight orphans after deleting eight notes.
                match std::fs::remove_file(&path) {
                    Ok(()) => crate::logln!("[data] removed audio {path}"),
                    Err(e) => crate::logln!("[data] could NOT remove audio {path}: {e}"),
                }
            }
        }

        conn.execute(
            "DELETE FROM recordings WHERE id = (SELECT recording_id FROM notes WHERE id = ?1)",
            params![id],
        )?;
        conn.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Audio files the database still knows about. Anything on disk that is
    /// not in here is an orphan: a recording whose note no longer exists.
    pub fn known_audio_paths(&self) -> Vec<String> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        let Ok(mut stmt) =
            conn.prepare("SELECT audio_path FROM recordings WHERE audio_path IS NOT NULL")
        else {
            return Vec::new();
        };
        stmt.query_map([], |row| row.get::<_, String>(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Recovery keeps the original recording time rather than stamping
    /// everything with the moment of the import, so restored notes sort back
    /// into place instead of arriving as a block of "now".
    pub fn set_note_created(&self, id: i64, created_at: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET created_at = ?2 WHERE id = ?1",
            params![id, created_at],
        )?;
        conn.execute(
            "UPDATE recordings SET created_at = ?2
              WHERE id = (SELECT recording_id FROM notes WHERE id = ?1)",
            params![id, created_at],
        )?;
        Ok(())
    }

    pub fn counts(&self) -> (i64, i64) {
        let Ok(conn) = self.conn.lock() else {
            return (0, 0);
        };
        let dictations = conn
            .query_row("SELECT COUNT(*) FROM dictations", [], |r| r.get(0))
            .unwrap_or(0);
        let notes = conn
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
            .unwrap_or(0);
        (dictations, notes)
    }

    pub fn clear_dictations(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM dictations", [])?)
    }

    /// Removes every note, its recording row, and the audio file on disk.
    /// Orphaned .wav files would otherwise stay behind forever.
    pub fn delete_all_notes(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();

        if let Ok(mut stmt) = conn.prepare(
            "SELECT r.audio_path FROM notes n JOIN recordings r ON r.id = n.recording_id",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, Option<String>>(0)) {
                for path in rows.flatten().flatten() {
                    if let Err(e) = std::fs::remove_file(&path) {
                        crate::logln!("[data] could NOT remove audio {path}: {e}");
                    }
                }
            }
        }

        conn.execute(
            "DELETE FROM recordings WHERE id IN (SELECT recording_id FROM notes)",
            [],
        )?;
        Ok(conn.execute("DELETE FROM notes", [])?)
    }

    /// Startup sweep. `days == 0` keeps everything.
    pub fn prune(&self, days: i64) -> Result<usize> {
        if days <= 0 {
            return Ok(0);
        }
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        let conn = self.conn.lock().unwrap();
        let removed = conn.execute(
            "DELETE FROM dictations WHERE created_at < ?1",
            params![cutoff],
        )?;
        Ok(removed)
    }
}
