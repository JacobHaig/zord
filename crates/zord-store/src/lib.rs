//! Local SQLite storage for sessions and transcript segments, with FTS5
//! full-text search. Everything stays on-device.

use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;
use zord_core::{Segment, Session, Source, Word};

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) a database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
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
                words_json  TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_segments_session ON segments(session_id, t_start_ms);

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
            "INSERT INTO segments (session_id, source, t_start_ms, t_end_ms, text, words_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id,
                seg.source.as_str(),
                seg.t_start_ms,
                seg.t_end_ms,
                seg.text,
                words_json,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// All segments for a session, ordered by time.
    pub fn segments(&self, session_id: &str) -> Result<Vec<Segment>> {
        let mut stmt = self.conn.prepare(
            "SELECT source, t_start_ms, t_end_ms, text, words_json
             FROM segments WHERE session_id = ?1 ORDER BY t_start_ms",
        )?;
        let rows = stmt.query_map(params![session_id], row_to_segment)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Full-text search across all transcripts. Returns (session_id, segment).
    pub fn search(&self, query: &str) -> Result<Vec<(String, Segment)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.session_id, s.source, s.t_start_ms, s.t_end_ms, s.text, s.words_json
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
}

fn row_to_segment(r: &rusqlite::Row) -> rusqlite::Result<Segment> {
    row_to_segment_offset(r, 0)
}

fn row_to_segment_offset(r: &rusqlite::Row, off: usize) -> rusqlite::Result<Segment> {
    let source_str: String = r.get(off)?;
    let words_json: Option<String> = r.get(off + 4)?;
    let words: Vec<Word> = words_json
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    Ok(Segment {
        source: match source_str.as_str() {
            "me" => Source::Me,
            _ => Source::Others,
        },
        t_start_ms: r.get(off + 1)?,
        t_end_ms: r.get(off + 2)?,
        text: r.get(off + 3)?,
        words,
    })
}
