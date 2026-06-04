//! Local SQLite storage for sessions and transcript segments, with FTS5
//! full-text search. Everything stays on-device.

use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use zord_core::{Segment, Session, Source, Word};

/// Process-wide database passphrase. Set once at startup (after unlocking);
/// every `Store::open` applies it as the SQLCipher key. `None` = no encryption.
static DB_KEY: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Set or clear the process-wide DB passphrase. Call **before** opening any
/// `Store`. (In non-`encryption` builds the value is stored but never applied.)
pub fn set_db_key(key: Option<String>) {
    if let Ok(mut g) = DB_KEY.write() {
        *g = key;
    }
}

#[cfg(feature = "encryption")]
fn current_key() -> Option<String> {
    DB_KEY.read().ok().and_then(|g| g.clone())
}

/// Apply the SQLCipher key (if any) as the first op after opening. Forces a
/// schema read so a wrong/missing key fails here with a clear error.
#[cfg(feature = "encryption")]
fn apply_key(conn: &Connection) -> Result<()> {
    if let Some(key) = current_key() {
        conn.pragma_update(None, "key", &key)?;
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .map_err(|_| anyhow::anyhow!("could not open encrypted database (wrong passphrase?)"))?;
    }
    Ok(())
}

#[cfg(not(feature = "encryption"))]
fn apply_key(_conn: &Connection) -> Result<()> {
    Ok(())
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) a database at `path`. Applies the process-wide
    /// passphrase (see [`set_db_key`]) before anything else.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        apply_key(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        create_schema(&self.conn)?;
        // Added in Phase 13 — tolerate older DBs that predate the column.
        let _ = self
            .conn
            .execute("ALTER TABLE sessions ADD COLUMN summary TEXT", []);
        // Added in Phase 23 — dense-prose compression of the meeting, kept beside
        // the human summary for cross-meeting synthesis.
        let _ = self
            .conn
            .execute("ALTER TABLE sessions ADD COLUMN compressed TEXT", []);
        // Added in Phase 16 — tolerate older DBs that predate the speaker column.
        let _ = self
            .conn
            .execute("ALTER TABLE segments ADD COLUMN speaker INTEGER", []);
        // Per-session expected speaker count for diarization (NULL = auto-detect),
        // set from the control next to "Identify speakers".
        let _ = self
            .conn
            .execute("ALTER TABLE sessions ADD COLUMN diarize_speakers INTEGER", []);
        Ok(())
    }

    /// Remember the expected speaker count for a session's diarization
    /// (0 clears it back to auto-detect).
    pub fn set_diarize_speakers(&self, session_id: &str, n: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET diarize_speakers = ?2 WHERE id = ?1",
            params![session_id, (n > 0).then_some(n)],
        )?;
        Ok(())
    }

    /// Fetch a session's expected speaker count, if one was set (None = auto).
    pub fn get_diarize_speakers(&self, session_id: &str) -> Result<Option<u32>> {
        let mut stmt = self
            .conn
            .prepare("SELECT diarize_speakers FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![session_id], |r| r.get::<_, Option<u32>>(0))?;
        Ok(rows.next().transpose()?.flatten().filter(|n| *n > 0))
    }

    /// Store (or replace) the AI-generated summary for a session.
    pub fn set_summary(&self, session_id: &str, summary: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET summary = ?2 WHERE id = ?1",
            params![session_id, summary],
        )?;
        Ok(())
    }

    /// Delete a session's stored summary (e.g. when the generation was bad).
    pub fn clear_summary(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET summary = NULL WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Fetch a session's stored summary, if any.
    pub fn get_summary(&self, session_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT summary FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![session_id], |r| r.get::<_, Option<String>>(0))?;
        Ok(rows.next().transpose()?.flatten())
    }

    /// Upsert an app-wide key/value pair (Phase 23), stamping `updated_at` with
    /// the current time in epoch ms.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO app_meta (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now],
        )?;
        Ok(())
    }

    /// Fetch an app-wide key/value pair as `(value, updated_at_ms)`, if present.
    pub fn get_meta(&self, key: &str) -> Result<Option<(String, u64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value, updated_at FROM app_meta WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })?;
        Ok(rows.next().transpose()?)
    }

    /// Store (or replace) the dense-prose compression for a session (Phase 23).
    pub fn set_compressed(&self, session_id: &str, compressed: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET compressed = ?2 WHERE id = ?1",
            params![session_id, compressed],
        )?;
        Ok(())
    }

    /// Delete a session's stored compression (e.g. when the generation was bad).
    pub fn clear_compressed(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET compressed = NULL WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Fetch a session's stored compression, if any.
    pub fn get_compressed(&self, session_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT compressed FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![session_id], |r| r.get::<_, Option<String>>(0))?;
        Ok(rows.next().transpose()?.flatten())
    }

    pub fn create_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, started_at, ended_at, title, audio_path, model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session.id,
                session.started_at,
                session.ended_at,
                session.title,
                session.audio_path,
                session.model,
            ],
        )?;
        Ok(())
    }

    /// Remove all segments for a session (used before re-transcribing).
    pub fn clear_segments(&self, session_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM segments WHERE session_id = ?1", params![session_id])?;
        Ok(())
    }

    /// Rename a session.
    pub fn set_session_title(&self, id: &str, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1",
            params![id, title],
        )?;
        Ok(())
    }

    /// Delete a session and its transcript. Clears segments first so the FTS
    /// index is kept consistent (FK cascade doesn't fire triggers by default).
    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.clear_segments(id)?;
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Update which model is recorded for a session.
    pub fn set_session_model(&self, id: &str, model: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET model = ?2 WHERE id = ?1",
            params![id, model],
        )?;
        Ok(())
    }

    pub fn end_session(&self, id: &str, ended_at: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?2 WHERE id = ?1",
            params![id, ended_at],
        )?;
        Ok(())
    }

    pub fn insert_segment(&self, session_id: &str, seg: &Segment) -> Result<i64> {
        let words_json = if seg.words.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&seg.words)?)
        };
        self.conn.execute(
            "INSERT INTO segments (session_id, source, t_start_ms, t_end_ms, text, words_json, speaker)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id,
                seg.source.as_str(),
                seg.t_start_ms,
                seg.t_end_ms,
                seg.text,
                words_json,
                seg.speaker,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Fetch a single session by id.
    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, title, audio_path, model
             FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok(Session {
                id: r.get(0)?,
                started_at: r.get(1)?,
                ended_at: r.get(2)?,
                title: r.get(3)?,
                audio_path: r.get(4)?,
                model: r.get(5)?,
            })
        })?;
        Ok(match rows.next() {
            Some(s) => Some(s?),
            None => None,
        })
    }

    /// All sessions, newest first.
    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, title, audio_path, model
             FROM sessions ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Session {
                id: r.get(0)?,
                started_at: r.get(1)?,
                ended_at: r.get(2)?,
                title: r.get(3)?,
                audio_path: r.get(4)?,
                model: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// All segments for a session, ordered by time.
    pub fn segments(&self, session_id: &str) -> Result<Vec<Segment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, t_start_ms, t_end_ms, text, words_json, speaker
             FROM segments WHERE session_id = ?1 ORDER BY t_start_ms",
        )?;
        let rows = stmt.query_map(params![session_id], row_to_segment)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Full-text search across all transcripts. Returns (session_id, segment).
    pub fn search(&self, query: &str) -> Result<Vec<(String, Segment)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.session_id, s.id, s.source, s.t_start_ms, s.t_end_ms, s.text, s.words_json, s.speaker
             FROM segments_fts f
             JOIN segments s ON s.id = f.rowid
             WHERE segments_fts MATCH ?1
             ORDER BY rank",
        )?;
        let rows = stmt.query_map(params![query], |r| {
            let session_id: String = r.get(0)?;
            Ok((session_id, row_to_segment_offset(r, 1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Edit a segment's text in place (FTS stays in sync via the UPDATE trigger).
    pub fn update_segment_text(&self, id: i64, text: &str) -> Result<()> {
        self.conn
            .execute("UPDATE segments SET text = ?2 WHERE id = ?1", params![id, text])?;
        Ok(())
    }

    /// Assign (or clear) the diarized speaker index for a single segment.
    pub fn set_segment_speaker(&self, id: i64, speaker: Option<i32>) -> Result<()> {
        self.conn.execute(
            "UPDATE segments SET speaker = ?2 WHERE id = ?1",
            params![id, speaker],
        )?;
        Ok(())
    }

    /// Clear all speaker assignments for a session (used before re-diarizing).
    /// Also drops any custom speaker names so stale labels don't linger.
    pub fn clear_speakers(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE segments SET speaker = NULL WHERE session_id = ?1",
            params![session_id],
        )?;
        self.conn.execute(
            "DELETE FROM speaker_names WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Set or clear a custom display name for a diarized speaker. An empty/blank
    /// name removes the override (reverting to "Speaker N").
    pub fn set_speaker_name(&self, session_id: &str, speaker: i32, name: &str) -> Result<()> {
        if name.trim().is_empty() {
            self.conn.execute(
                "DELETE FROM speaker_names WHERE session_id = ?1 AND speaker = ?2",
                params![session_id, speaker],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO speaker_names (session_id, speaker, name) VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_id, speaker) DO UPDATE SET name = excluded.name",
                params![session_id, speaker, name.trim()],
            )?;
        }
        Ok(())
    }

    /// Per-session presence flags for sidebar badges, in one query:
    /// `id -> (has_summary, has_compressed, has_speakers)`. `has_speakers` is true
    /// when any segment carries a diarized speaker index.
    pub fn session_badges(&self) -> Result<HashMap<String, (bool, bool, bool)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,
                    (summary IS NOT NULL AND TRIM(summary) <> ''),
                    (compressed IS NOT NULL AND TRIM(compressed) <> ''),
                    EXISTS(SELECT 1 FROM segments sg WHERE sg.session_id = sessions.id AND sg.speaker IS NOT NULL)
             FROM sessions",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, bool>(1)?, r.get::<_, bool>(2)?, r.get::<_, bool>(3)?),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }

    /// Custom speaker names for a session, as a `speaker_index -> name` map.
    pub fn speaker_names(&self, session_id: &str) -> Result<HashMap<i32, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT speaker, name FROM speaker_names WHERE session_id = ?1")?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok((r.get::<_, i32>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }
}

/// Create the base tables, indexes, FTS virtual table, and sync triggers
/// (all `IF NOT EXISTS`, so safe to run on every open).
fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id          TEXT PRIMARY KEY,
                started_at  INTEGER NOT NULL,
                ended_at    INTEGER,
                title       TEXT,
                audio_path  TEXT,
                model       TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS segments (
                id          INTEGER PRIMARY KEY,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                source      TEXT NOT NULL,
                t_start_ms  INTEGER NOT NULL,
                t_end_ms    INTEGER NOT NULL,
                text        TEXT NOT NULL,
                words_json  TEXT,
                speaker     INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_segments_session ON segments(session_id, t_start_ms);

            -- Phase 16: custom names for diarized speakers, per session.
            CREATE TABLE IF NOT EXISTS speaker_names (
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                speaker     INTEGER NOT NULL,
                name        TEXT NOT NULL,
                PRIMARY KEY (session_id, speaker)
            );

            -- Phase 23: small app-wide key/value store (e.g. the cross-meeting
            -- Overview rollup + when it was generated). Not session-scoped.
            CREATE TABLE IF NOT EXISTS app_meta (
                key         TEXT PRIMARY KEY,
                value       TEXT NOT NULL,
                updated_at  INTEGER NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS segments_fts USING fts5(
                text,
                content='segments',
                content_rowid='id'
            );

            -- Keep the FTS index in sync with the segments table.
            CREATE TRIGGER IF NOT EXISTS segments_ai AFTER INSERT ON segments BEGIN
                INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
            END;
            CREATE TRIGGER IF NOT EXISTS segments_ad AFTER DELETE ON segments BEGIN
                INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
            END;
            CREATE TRIGGER IF NOT EXISTS segments_au AFTER UPDATE ON segments BEGIN
                INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
                INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
            END;
            "#,
    )?;
    Ok(())
}

fn row_to_segment(r: &rusqlite::Row) -> rusqlite::Result<Segment> {
    row_to_segment_offset(r, 0)
}

fn row_to_segment_offset(r: &rusqlite::Row, off: usize) -> rusqlite::Result<Segment> {
    let id: Option<i64> = r.get(off)?;
    let source_str: String = r.get(off + 1)?;
    let words_json: Option<String> = r.get(off + 5)?;
    let words: Vec<Word> = words_json
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    let speaker: Option<i32> = r.get(off + 6)?;
    Ok(Segment {
        id,
        source: match source_str.as_str() {
            "me" => Source::Me,
            _ => Source::Others,
        },
        t_start_ms: r.get(off + 2)?,
        t_end_ms: r.get(off + 3)?,
        text: r.get(off + 4)?,
        words,
        speaker,
    })
}

// ---------------------------------------------------------------------------
// At-rest encryption (SQLCipher) — only meaningful with the `encryption` feature
// ---------------------------------------------------------------------------

/// Best-effort check of whether the DB file at `path` is encrypted (i.e. can't
/// be read as plain SQLite). Returns false in non-`encryption` builds.
#[cfg(feature = "encryption")]
pub fn is_encrypted(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    if !path.exists() {
        return false;
    }
    match Connection::open(path) {
        // No key applied: a readable schema means it's plaintext.
        Ok(c) => c
            .query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .is_err(),
        Err(_) => true,
    }
}

#[cfg(not(feature = "encryption"))]
pub fn is_encrypted(_path: impl AsRef<Path>) -> bool {
    false
}

/// Encrypt an existing plaintext database in place with `key` (keeps a
/// `<db>.plaintext.bak` backup). Uses SQLCipher's `sqlcipher_export`.
#[cfg(feature = "encryption")]
pub fn encrypt_existing(path: impl AsRef<Path>, key: &str) -> Result<()> {
    use std::path::PathBuf;
    let path = path.as_ref();
    let p = path.display().to_string();
    let backup = PathBuf::from(format!("{p}.plaintext.bak"));
    let enc = PathBuf::from(format!("{p}.enc"));
    std::fs::copy(path, &backup)?;
    let _ = std::fs::remove_file(&enc);

    let conn = Connection::open(path)?; // plaintext source
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE").ok();
    conn.execute(
        "ATTACH DATABASE ?1 AS encrypted KEY ?2",
        params![enc.to_string_lossy(), key],
    )?;
    conn.query_row("SELECT sqlcipher_export('encrypted')", [], |_| Ok(()))?;
    conn.execute("DETACH DATABASE encrypted", [])?;
    drop(conn);

    // Replace plaintext with the encrypted copy; clear stale WAL sidecars.
    std::fs::rename(&enc, path)?;
    let _ = std::fs::remove_file(format!("{p}-wal"));
    let _ = std::fs::remove_file(format!("{p}-shm"));
    tracing::info!("database encrypted (backup at {backup:?})");
    Ok(())
}

/// Decrypt an encrypted database in place back to plaintext (`key` is its
/// current passphrase). Keeps a `<db>.encrypted.bak` backup.
#[cfg(feature = "encryption")]
pub fn decrypt_existing(path: impl AsRef<Path>, key: &str) -> Result<()> {
    use std::path::PathBuf;
    let path = path.as_ref();
    let p = path.display().to_string();
    let backup = PathBuf::from(format!("{p}.encrypted.bak"));
    let plain = PathBuf::from(format!("{p}.plain"));
    std::fs::copy(path, &backup)?;
    let _ = std::fs::remove_file(&plain);

    let conn = Connection::open(path)?;
    conn.pragma_update(None, "key", key)?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
        .map_err(|_| anyhow::anyhow!("wrong passphrase"))?;
    conn.execute(
        "ATTACH DATABASE ?1 AS plaintext KEY ''",
        params![plain.to_string_lossy()],
    )?;
    conn.query_row("SELECT sqlcipher_export('plaintext')", [], |_| Ok(()))?;
    conn.execute("DETACH DATABASE plaintext", [])?;
    drop(conn);

    std::fs::rename(&plain, path)?;
    let _ = std::fs::remove_file(format!("{p}-wal"));
    let _ = std::fs::remove_file(format!("{p}-shm"));
    tracing::info!("database decrypted (backup at {backup:?})");
    Ok(())
}

#[cfg(all(test, feature = "encryption"))]
mod enc_tests {
    use super::*;

    #[test]
    fn keyed_roundtrip() {
        let dir = std::env::temp_dir().join(format!("zord-enc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);

        set_db_key(Some("hunter2".to_string()));
        {
            let s = Store::open(&db).unwrap();
            s.create_session(&Session {
                id: "s1".into(),
                started_at: 1,
                ended_at: None,
                title: None,
                audio_path: None,
                model: "m".into(),
            })
            .unwrap();
        }
        // Correct key reopens fine.
        {
            let s = Store::open(&db).unwrap();
            assert_eq!(s.list_sessions().unwrap().len(), 1);
        }
        // The file is genuinely encrypted.
        assert!(is_encrypted(&db));
        // Wrong key fails.
        set_db_key(Some("wrong".to_string()));
        assert!(Store::open(&db).is_err());
        // No key fails.
        set_db_key(None);
        assert!(Store::open(&db).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
