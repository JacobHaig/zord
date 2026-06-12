//! Local SQLite storage for sessions and transcript segments, with FTS5
//! full-text search. Everything stays on-device.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;
use zord_core::{
    ItemKind, ItemStatus, Project, ProjectItem, ProjectStatus, Segment, Session, Source, Word,
};

/// Maximum number of raw samples kept per enrolled person. Older samples beyond
/// this cap are pruned on each [`Store::enroll_voiceprint`] call so the rolling
/// bank stays bounded while the centroid improves with newer recordings.
const VOICEPRINT_SAMPLE_CAP: i64 = 8;

/// One known person in the voiceprint library (Speakers view row).
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceprintInfo {
    /// Stable database id.
    pub id: i64,
    /// Display name (unique in the library).
    pub name: String,
    /// Embedding model id; all samples for this person are in this embedding space.
    pub model: String,
    /// Number of raw samples stored (≤ [`VOICEPRINT_SAMPLE_CAP`]).
    pub samples: u32,
    /// When the last sample was enrolled, as Unix epoch seconds.
    pub updated_at: u64,
    /// `(session_id, session_title)` pairs where this person was auto-identified,
    /// ordered newest-first.
    pub appearances: Vec<(String, String)>,
}

/// Process-wide database passphrase. Set once at startup (after unlocking);
/// every `Store::open` applies it as the SQLCipher key. `None` = no encryption.
static DB_KEY: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Tighten a file to owner-only read/write (`0600`) on Unix; no-op elsewhere.
/// Best-effort. Used for the plaintext/encrypted DB backups, which hold a full
/// copy of every transcript.
#[cfg(feature = "encryption")]
fn restrict_to_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

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
            .map_err(|_| {
                anyhow::anyhow!("could not open encrypted database (wrong passphrase?)")
            })?;
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
        // Concurrent writers (GUI db thread + CLI/web) otherwise get an
        // immediate SQLITE_BUSY instead of waiting their turn.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        create_schema(&self.conn)?;
        add_late_columns(&self.conn);
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

    /// Store (or clear, with empty text) the host's free-form notes for a session.
    pub fn set_notes(&self, session_id: &str, notes: &str) -> Result<()> {
        let trimmed = notes.trim();
        self.conn.execute(
            "UPDATE sessions SET notes = ?2 WHERE id = ?1",
            params![session_id, (!trimmed.is_empty()).then_some(notes)],
        )?;
        Ok(())
    }

    /// Fetch a session's host notes, if any.
    pub fn get_notes(&self, session_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT notes FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![session_id], |r| r.get::<_, Option<String>>(0))?;
        Ok(rows.next().transpose()?.flatten())
    }

    /// Find sessions whose notes contain `query` (case-insensitive substring).
    /// Notes are short + few, so a LIKE scan is plenty — no FTS needed. Returns
    /// `(session_id, notes)`, newest session first.
    pub fn search_notes(&self, query: &str) -> Result<Vec<(String, String)>> {
        // Escape LIKE wildcards so the query is matched literally.
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let mut stmt = self.conn.prepare(
            "SELECT id, notes FROM sessions
             WHERE notes IS NOT NULL AND notes LIKE ?1 ESCAPE '\\'
             ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map(params![pattern], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
            params![key, value, i64v(now)],
        )?;
        Ok(())
    }

    /// Fetch an app-wide key/value pair as `(value, updated_at_ms)`, if present.
    pub fn get_meta(&self, key: &str) -> Result<Option<(String, u64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value, updated_at FROM app_meta WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |r| {
            Ok((r.get::<_, String>(0)?, get_u64(r, 1)?))
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

    // ----- Phase 26: rolling project ledger -----------------------------------

    /// Insert a project.
    pub fn create_project(&self, p: &Project) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects (id, name, status, description, created_at, updated_at, last_activity_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![p.id, p.name, p.status.as_str(), p.description, i64v(p.created_at), i64v(p.updated_at), i64v(p.last_activity_at)],
        )?;
        Ok(())
    }

    /// All projects, active first, then most-recently-active.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, status, description, created_at, updated_at, last_activity_at
             FROM projects
             ORDER BY (status = 'active') DESC, last_activity_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_project)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_project(&self, id: &str) -> Result<Option<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, status, description, created_at, updated_at, last_activity_at
             FROM projects WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_project)?;
        Ok(rows.next().transpose()?)
    }

    pub fn rename_project(&self, id: &str, name: &str, now: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET name = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, name, i64v(now)],
        )?;
        Ok(())
    }

    pub fn set_project_status(&self, id: &str, status: ProjectStatus, now: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status.as_str(), i64v(now)],
        )?;
        Ok(())
    }

    pub fn set_project_description(&self, id: &str, desc: Option<&str>, now: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET description = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, desc, i64v(now)],
        )?;
        Ok(())
    }

    /// Bump a project's activity clock when a meeting touches it.
    pub fn touch_project(&self, id: &str, now: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET last_activity_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![id, i64v(now)],
        )?;
        Ok(())
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Add an item under a project.
    pub fn add_item(&self, it: &ProjectItem) -> Result<()> {
        self.conn.execute(
            "INSERT INTO project_items
                (id, project_id, kind, text, owner, status, created_session,
                 updated_session, completed_session, created_at, updated_at, manual)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                it.id,
                it.project_id,
                it.kind.as_str(),
                it.text,
                it.owner,
                it.status.as_str(),
                it.created_session,
                it.updated_session,
                it.completed_session,
                i64v(it.created_at),
                i64v(it.updated_at),
                it.manual as i64,
            ],
        )?;
        Ok(())
    }

    pub fn get_item(&self, id: &str) -> Result<Option<ProjectItem>> {
        let mut stmt = self.conn.prepare(&item_select("WHERE id = ?1"))?;
        let mut rows = stmt.query_map(params![id], row_to_item)?;
        Ok(rows.next().transpose()?)
    }

    /// All items for a project, active first, oldest within a group first.
    pub fn list_items(&self, project_id: &str) -> Result<Vec<ProjectItem>> {
        let mut stmt = self.conn.prepare(&item_select(
            "WHERE project_id = ?1 ORDER BY (status = 'done'), created_at",
        ))?;
        let rows = stmt.query_map(params![project_id], row_to_item)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_item_status(
        &self,
        id: &str,
        status: ItemStatus,
        updated_session: Option<&str>,
        completed_session: Option<&str>,
        now: u64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE project_items
             SET status = ?2, updated_session = ?3, completed_session = ?4, updated_at = ?5
             WHERE id = ?1",
            params![
                id,
                status.as_str(),
                updated_session,
                completed_session,
                i64v(now)
            ],
        )?;
        Ok(())
    }

    pub fn update_item_text(
        &self,
        id: &str,
        text: &str,
        owner: Option<&str>,
        now: u64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE project_items SET text = ?2, owner = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, text, owner, i64v(now)],
        )?;
        Ok(())
    }

    /// Mark an item as hand-edited (protected from automatic folds).
    pub fn set_item_manual(&self, id: &str, manual: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE project_items SET manual = ?2 WHERE id = ?1",
            params![id, manual as i64],
        )?;
        Ok(())
    }

    /// Reassign an item to another project (used by merge / manual move).
    pub fn move_item(&self, item_id: &str, new_project: &str, now: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE project_items SET project_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![item_id, new_project, i64v(now)],
        )?;
        Ok(())
    }

    pub fn delete_item(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM project_items WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Record that a session has been folded into the ledger (with its extract).
    pub fn mark_session_applied(
        &self,
        session_id: &str,
        extract: Option<&str>,
        now: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO session_overview_state (session_id, applied_at, extract)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET applied_at = excluded.applied_at, extract = excluded.extract",
            params![session_id, i64v(now), extract],
        )?;
        Ok(())
    }

    pub fn is_session_applied(&self, session_id: &str) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare("SELECT 1 FROM session_overview_state WHERE session_id = ?1")?;
        Ok(stmt.exists(params![session_id])?)
    }

    /// Sessions not yet folded into the ledger, oldest first (fold order).
    pub fn unapplied_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, title, audio_path, model, overview_folded_ms
             FROM sessions
             WHERE id NOT IN (SELECT session_id FROM session_overview_state)
             ORDER BY started_at",
        )?;
        let rows = stmt.query_map([], row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Wipe the entire ledger (projects, items, history, applied-state) — the
    /// destructive reset behind "Build from history". Drops manual edits.
    pub fn clear_ledger(&self) -> Result<()> {
        self.conn.execute_batch(
            "BEGIN;
             DELETE FROM project_history;
             DELETE FROM project_items;
             DELETE FROM projects;
             DELETE FROM session_overview_state;
             COMMIT;",
        )?;
        Ok(())
    }

    /// Append an audit-log entry (e.g. "completed", "added", "reopened").
    pub fn log_history(
        &self,
        project_id: &str,
        item_id: Option<&str>,
        change: &str,
        session_id: Option<&str>,
        now: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO project_history (project_id, item_id, change, session_id, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project_id, item_id, change, session_id, i64v(now)],
        )?;
        Ok(())
    }

    pub fn create_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, started_at, ended_at, title, audio_path, model, overview_folded_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session.id,
                i64v(session.started_at),
                opt_i64v(session.ended_at),
                session.title,
                session.audio_path,
                session.model,
                opt_i64v(session.overview_folded_ms),
            ],
        )?;
        Ok(())
    }

    /// Stamp a session as folded into the living overview document (Phase 39):
    /// `at_ms` records when the fold ran. Unstamped (`NULL`) sessions are the
    /// ones a fold-all picks up.
    pub fn set_overview_folded(&self, session_id: &str, at_ms: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET overview_folded_ms = ?2 WHERE id = ?1",
            params![session_id, i64v(at_ms)],
        )?;
        Ok(())
    }

    /// Remove all segments for a session (used before re-transcribing).
    pub fn clear_segments(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM segments WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Delete segments whose `t_start_ms` falls within `[start_ms, end_ms)` for
    /// a session (Phase 42d range re-transcription). Only segments that *start*
    /// inside the window are deleted — any segment that straddles the boundary
    /// is left intact on the safe side. FTS stays consistent because the DELETE
    /// trigger on `segments` keeps `segments_fts` in sync.
    ///
    /// This is a focused "honest" delete: it removes what the new transcription
    /// will replace (segments originating in the range) without touching
    /// anything outside. Returns the number of rows deleted.
    pub fn delete_segments_in_range(
        &self,
        session_id: &str,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM segments WHERE session_id = ?1 AND t_start_ms >= ?2 AND t_start_ms < ?3",
            params![session_id, i64v(start_ms), i64v(end_ms)],
        )?;
        Ok(n)
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
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM segments WHERE session_id = ?1", params![id])?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        tx.commit()?;
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

    /// Set or clear a session's kept-audio prefix (cleared when capture-only
    /// WAVs are removed after the post-stop transcription — Phase 25).
    pub fn set_audio_path(&self, id: &str, audio_path: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET audio_path = ?2 WHERE id = ?1",
            params![id, audio_path],
        )?;
        Ok(())
    }

    pub fn end_session(&self, id: &str, ended_at: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?2 WHERE id = ?1",
            params![id, i64v(ended_at)],
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
                i64v(seg.t_start_ms),
                i64v(seg.t_end_ms),
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
            "SELECT id, started_at, ended_at, title, audio_path, model, overview_folded_ms
             FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_session)?;
        Ok(match rows.next() {
            Some(s) => Some(s?),
            None => None,
        })
    }

    /// All sessions, newest first.
    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, title, audio_path, model, overview_folded_ms
             FROM sessions ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_session)?;
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
        self.conn.execute(
            "UPDATE segments SET text = ?2 WHERE id = ?1",
            params![id, text],
        )?;
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
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE segments SET speaker = NULL WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.execute(
            "DELETE FROM speaker_names WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.commit()?;
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
                (
                    r.get::<_, bool>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, bool>(3)?,
                ),
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

    /// Tag which speaker index is the app user themself. Integration sessions
    /// record every participant as a uniform `spk-N` track; this marks the one
    /// matching the configured platform user ID (styling/perspective only).
    pub fn set_me_speaker(&self, session_id: &str, speaker: i32) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET me_speaker = ?2 WHERE id = ?1",
            params![session_id, speaker],
        )?;
        Ok(())
    }

    /// The session's "me" speaker index, if one was tagged (integration
    /// sessions only — `None` for mic/desktop recordings).
    pub fn me_speaker(&self, session_id: &str) -> Result<Option<i32>> {
        let v = self
            .conn
            .query_row(
                "SELECT me_speaker FROM sessions WHERE id = ?1",
                params![session_id],
                |r| r.get::<_, Option<i32>>(0),
            )
            .optional()?
            .flatten();
        Ok(v)
    }

    // -----------------------------------------------------------------------
    // Phase 38: voiceprint library — enroll, match, manage
    // -----------------------------------------------------------------------

    /// Add (or update) a voiceprint entry for `name` / `model`, appending
    /// `embedding` as a new sample. If the stored model differs from `model`
    /// the old samples are dropped and the model is updated before the new
    /// sample is inserted (embeddings are not comparable across models).
    /// The rolling sample bank is capped at [`VOICEPRINT_SAMPLE_CAP`]; the
    /// oldest samples beyond that cap are pruned after each insert.
    /// Returns the voiceprint id (stable across calls for the same name).
    pub fn enroll_voiceprint(
        &self,
        name: &str,
        model: &str,
        embedding: &[f32],
        session_id: Option<&str>,
    ) -> Result<i64> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let tx = self.conn.unchecked_transaction()?;

        // Upsert the voiceprint row.
        tx.execute(
            "INSERT INTO voiceprints (name, model, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(name) DO UPDATE SET updated_at = excluded.updated_at",
            params![name, model, now_secs],
        )?;
        let vp_id: i64 = tx.query_row(
            "SELECT id, model FROM voiceprints WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?;

        // If the stored model differs, old samples are incomparable — wipe them
        // and switch to the new model.
        let stored_model: String = tx.query_row(
            "SELECT model FROM voiceprints WHERE id = ?1",
            params![vp_id],
            |r| r.get(0),
        )?;
        if stored_model != model {
            tx.execute(
                "DELETE FROM voiceprint_samples WHERE voiceprint_id = ?1",
                params![vp_id],
            )?;
            tx.execute(
                "UPDATE voiceprints SET model = ?2, updated_at = ?3 WHERE id = ?1",
                params![vp_id, model, now_secs],
            )?;
        }

        // Insert new sample.
        tx.execute(
            "INSERT INTO voiceprint_samples (voiceprint_id, session_id, embedding, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![vp_id, session_id, embedding_to_blob(embedding), now_secs],
        )?;

        // Prune to the newest VOICEPRINT_SAMPLE_CAP samples.
        tx.execute(
            "DELETE FROM voiceprint_samples
             WHERE voiceprint_id = ?1
               AND rowid IN (
                   SELECT rowid FROM voiceprint_samples
                   WHERE voiceprint_id = ?1
                   ORDER BY created_at DESC, rowid DESC
                   LIMIT -1 OFFSET ?2
               )",
            params![vp_id, VOICEPRINT_SAMPLE_CAP],
        )?;

        tx.commit()?;
        Ok(vp_id)
    }

    /// All voiceprints with sample counts and session appearances, ordered by
    /// most-recently updated first.
    pub fn voiceprints(&self) -> Result<Vec<VoiceprintInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, model,
                    (SELECT COUNT(*) FROM voiceprint_samples WHERE voiceprint_id = voiceprints.id),
                    updated_at
             FROM voiceprints
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, i64>(4)? as u64,
            ))
        })?;
        let mut infos = Vec::new();
        for row in rows {
            let (id, name, model, samples, updated_at) = row?;
            // Collect sessions where this voiceprint was identified, newest first.
            let mut app_stmt = self.conn.prepare(
                "SELECT sn.session_id, COALESCE(s.title, '') \
                 FROM speaker_names sn \
                 JOIN sessions s ON s.id = sn.session_id \
                 WHERE sn.voiceprint_id = ?1 \
                 ORDER BY s.started_at DESC",
            )?;
            let appearances: Vec<(String, String)> = app_stmt
                .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            infos.push(VoiceprintInfo {
                id,
                name,
                model,
                samples,
                updated_at,
                appearances,
            });
        }
        Ok(infos)
    }

    /// Compute the mean (centroid) embedding for every enrolled person that has
    /// samples under `model`, then L2-normalise it. Persons with no samples or
    /// inconsistent embedding dimensions are silently skipped.
    ///
    /// Returns `(voiceprint_id, name, normalised_centroid)` triples; pass the
    /// result directly to [`best_voiceprint_match`].
    pub fn voiceprint_centroids(&self, model: &str) -> Result<Vec<(i64, String, Vec<f32>)>> {
        // Fetch all samples grouped by person.
        let mut stmt = self.conn.prepare(
            "SELECT vp.id, vp.name, vs.embedding
             FROM voiceprints vp
             JOIN voiceprint_samples vs ON vs.voiceprint_id = vp.id
             WHERE vp.model = ?1
             ORDER BY vp.id",
        )?;
        let rows = stmt.query_map(params![model], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;

        // Group samples by person.
        let mut grouped: Vec<(i64, String, Vec<Vec<f32>>)> = Vec::new();
        for row in rows {
            let (id, name, blob) = row?;
            let emb = blob_to_embedding(&blob);
            match grouped.last_mut() {
                Some(last) if last.0 == id => last.2.push(emb),
                _ => grouped.push((id, name, vec![emb])),
            }
        }

        let mut result = Vec::new();
        for (id, name, samples) in grouped {
            if samples.is_empty() {
                continue;
            }
            let dim = samples[0].len();
            // Skip if any sample has an inconsistent dimension.
            if samples.iter().any(|s| s.len() != dim) || dim == 0 {
                continue;
            }
            // Mean.
            let mut centroid = vec![0.0f32; dim];
            for s in &samples {
                for (c, v) in centroid.iter_mut().zip(s) {
                    *c += v;
                }
            }
            let n = samples.len() as f32;
            for c in &mut centroid {
                *c /= n;
            }
            // L2-normalise.
            let norm: f32 = centroid.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm == 0.0 {
                continue;
            }
            for c in &mut centroid {
                *c /= norm;
            }
            result.push((id, name, centroid));
        }
        Ok(result)
    }

    /// Rename a voiceprint. If `name` is already taken by **another** voiceprint,
    /// the two are merged: this entry's samples are moved to the target, the
    /// target is pruned to [`VOICEPRINT_SAMPLE_CAP`] newest samples,
    /// `speaker_names.voiceprint_id` is repointed to the target, and the source
    /// row is deleted. Otherwise the voiceprint is renamed in place and
    /// `updated_at` is bumped.
    pub fn rename_voiceprint(&self, id: i64, name: &str) -> Result<()> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let tx = self.conn.unchecked_transaction()?;

        // Check if the target name belongs to a different existing voiceprint.
        let target_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM voiceprints WHERE name = ?1 AND id <> ?2",
                params![name, id],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(target) = target_id {
            // Merge: move samples, repoint speaker_names, delete source.
            tx.execute(
                "UPDATE voiceprint_samples SET voiceprint_id = ?1 WHERE voiceprint_id = ?2",
                params![target, id],
            )?;
            // Prune merged target to cap.
            tx.execute(
                "DELETE FROM voiceprint_samples
                 WHERE voiceprint_id = ?1
                   AND rowid IN (
                       SELECT rowid FROM voiceprint_samples
                       WHERE voiceprint_id = ?1
                       ORDER BY created_at DESC, rowid DESC
                       LIMIT -1 OFFSET ?2
                   )",
                params![target, VOICEPRINT_SAMPLE_CAP],
            )?;
            tx.execute(
                "UPDATE speaker_names SET voiceprint_id = ?1 WHERE voiceprint_id = ?2",
                params![target, id],
            )?;
            // Delete source — voiceprint_samples already moved so no orphans.
            tx.execute("DELETE FROM voiceprints WHERE id = ?1", params![id])?;
            tx.execute(
                "UPDATE voiceprints SET updated_at = ?2 WHERE id = ?1",
                params![target, now_secs],
            )?;
        } else {
            tx.execute(
                "UPDATE voiceprints SET name = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, name, now_secs],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Remove a single voiceprint and all its samples from the library.
    /// Any `speaker_names` rows that pointed to it have their `voiceprint_id`
    /// cleared so history is preserved. The samples are deleted via cascade
    /// (or explicitly if FK enforcement is off).
    pub fn forget_voiceprint(&self, id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE speaker_names SET voiceprint_id = NULL WHERE voiceprint_id = ?1",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM voiceprint_samples WHERE voiceprint_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM voiceprints WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// Wipe the entire voiceprint library — all people and all samples.
    /// `speaker_names.voiceprint_id` is cleared for every row first.
    pub fn forget_all_voiceprints(&self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE speaker_names SET voiceprint_id = NULL", [])?;
        tx.execute("DELETE FROM voiceprint_samples", [])?;
        tx.execute("DELETE FROM voiceprints", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Persist the computed embedding for a diarized speaker slot. Overwrites
    /// any previously stored embedding for the same `(session_id, speaker)` pair.
    pub fn set_session_speaker_embedding(
        &self,
        session_id: &str,
        speaker: i32,
        model: &str,
        embedding: &[f32],
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO session_speaker_embeddings
             (session_id, speaker, model, embedding)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, speaker, model, embedding_to_blob(embedding)],
        )?;
        Ok(())
    }

    /// Fetch the stored embedding for a diarized speaker slot, if any.
    /// Returns `(model_id, embedding)`.
    pub fn session_speaker_embedding(
        &self,
        session_id: &str,
        speaker: i32,
    ) -> Result<Option<(String, Vec<f32>)>> {
        let result = self
            .conn
            .query_row(
                "SELECT model, embedding FROM session_speaker_embeddings
                 WHERE session_id = ?1 AND speaker = ?2",
                params![session_id, speaker],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        Ok(result.map(|(m, b)| (m, blob_to_embedding(&b))))
    }

    /// Record that a diarized speaker slot belongs to a known voiceprint.
    /// The `speaker_names` row **must already exist** (call [`set_speaker_name`]
    /// first); this method only updates the `voiceprint_id` column.
    pub fn link_speaker_voiceprint(
        &self,
        session_id: &str,
        speaker: i32,
        voiceprint_id: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE speaker_names SET voiceprint_id = ?3
             WHERE session_id = ?1 AND speaker = ?2",
            params![session_id, speaker, voiceprint_id],
        )?;
        Ok(())
    }
}

/// Best-effort `ALTER TABLE`s for columns added after the base schema; each is
/// ignored if it already exists, so older DBs are brought forward in place.
fn add_late_columns(conn: &Connection) {
    // Added in Phase 13 — tolerate older DBs that predate the column.
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN summary TEXT", []);
    // Added in Phase 23 — dense-prose compression of the meeting, kept beside
    // the human summary for cross-meeting synthesis.
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN compressed TEXT", []);
    // Added in Phase 16 — tolerate older DBs that predate the speaker column.
    let _ = conn.execute("ALTER TABLE segments ADD COLUMN speaker INTEGER", []);
    // Per-session expected speaker count for diarization (NULL = auto-detect),
    // set from the control next to "Identify speakers".
    let _ = conn.execute(
        "ALTER TABLE sessions ADD COLUMN diarize_speakers INTEGER",
        [],
    );
    // Free-form per-session notes written by the host (links, action items,
    // reminders). Searchable, and fed to the AI features as authoritative input.
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN notes TEXT", []);
    // Phase 38 — links a per-session speaker slot to a known voiceprint.
    let _ = conn.execute(
        "ALTER TABLE speaker_names ADD COLUMN voiceprint_id INTEGER",
        [],
    );
    // Unified integration tracks — which spk-N index is the app user themself
    // (every Discord participant records as a uniform speaker track; "me" is a
    // tag from the configured user ID, not a separate audio channel).
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN me_speaker INTEGER", []);
    // Phase 39 — when this session was folded into the living overview
    // document (epoch ms). NULL = not folded yet, so fold-all retries it.
    let _ = conn.execute(
        "ALTER TABLE sessions ADD COLUMN overview_folded_ms INTEGER",
        [],
    );
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

            -- Phase 26: the rolling project ledger. `projects` + `project_items`
            -- are the durable Overview state; `session_overview_state` tracks
            -- which sessions have been folded in (idempotency + staleness); the
            -- audit log records why each item changed (provenance).
            CREATE TABLE IF NOT EXISTS projects (
                id               TEXT PRIMARY KEY,
                name             TEXT NOT NULL,
                status           TEXT NOT NULL DEFAULT 'active',
                description      TEXT,
                created_at       INTEGER NOT NULL,
                updated_at       INTEGER NOT NULL,
                last_activity_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_items (
                id                 TEXT PRIMARY KEY,
                project_id         TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                kind               TEXT NOT NULL DEFAULT 'action',
                text               TEXT NOT NULL,
                owner              TEXT,
                status             TEXT NOT NULL DEFAULT 'open',
                created_session    TEXT,
                updated_session    TEXT,
                completed_session  TEXT,
                created_at         INTEGER NOT NULL,
                updated_at         INTEGER NOT NULL,
                manual             INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_items_project ON project_items(project_id);

            -- Which sessions have been folded into the ledger, and the structured
            -- extract used (so a re-transcribed/edited session can be re-folded).
            CREATE TABLE IF NOT EXISTS session_overview_state (
                session_id  TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                applied_at  INTEGER NOT NULL,
                extract     TEXT
            );

            -- Audit trail: one row per item change, naming the session that caused it.
            CREATE TABLE IF NOT EXISTS project_history (
                id          INTEGER PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                item_id     TEXT,
                change      TEXT NOT NULL,
                session_id  TEXT,
                at          INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_history_project ON project_history(project_id, at);

            -- Phase 38: persistent cross-session speaker identity library.
            -- One row per known person; `model` ties the embedding space.
            CREATE TABLE IF NOT EXISTS voiceprints (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                name       TEXT NOT NULL UNIQUE,
                model      TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            -- Rolling sample bank (max 8 per person, oldest pruned).
            -- `session_id` is informational (no FK — the sample outlives its session).
            CREATE TABLE IF NOT EXISTS voiceprint_samples (
                voiceprint_id INTEGER NOT NULL REFERENCES voiceprints(id) ON DELETE CASCADE,
                session_id    TEXT,
                embedding     BLOB NOT NULL,
                created_at    INTEGER NOT NULL
            );

            -- One cached embedding per diarized speaker slot per session,
            -- used by the engine to propose a voiceprint match.
            CREATE TABLE IF NOT EXISTS session_speaker_embeddings (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                speaker    INTEGER NOT NULL,
                model      TEXT NOT NULL,
                embedding  BLOB NOT NULL,
                PRIMARY KEY (session_id, speaker)
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

// rusqlite 0.40 dropped the u64 <-> SQL impls (u64 can exceed SQLite's i64).
// Our domain u64s are epoch-ms timestamps and small counts, always within i64
// range, so we cast losslessly at the SQL boundary with these helpers.
#[inline]
fn i64v(v: u64) -> i64 {
    v as i64
}
#[inline]
fn opt_i64v(v: Option<u64>) -> Option<i64> {
    v.map(|x| x as i64)
}
#[inline]
fn get_u64(r: &rusqlite::Row, idx: usize) -> rusqlite::Result<u64> {
    Ok(r.get::<_, i64>(idx)? as u64)
}
#[inline]
fn get_opt_u64(r: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<u64>> {
    Ok(r.get::<_, Option<i64>>(idx)?.map(|x| x as u64))
}

// ---------------------------------------------------------------------------
// Phase 38: voiceprint helpers — blob codec, cosine similarity, matcher
// ---------------------------------------------------------------------------

/// Serialise a float vector as a little-endian f32 blob for SQLite storage.
fn embedding_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Deserialise a little-endian f32 blob back into a float vector.
fn blob_to_embedding(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Cosine similarity of two equal-length vectors. Returns `-1.0` for
/// zero-length, mismatched-dim, or zero-norm inputs.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return -1.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        -1.0
    } else {
        dot / (na * nb)
    }
}

/// Return the best-matching enrolled voiceprint for `query` from `cands`
/// (id, name, centroid). The winner must both clear `threshold` **and** beat
/// the runner-up by at least `margin`; this open-set safety margin prevents
/// forcing a match when two people sound similar.
///
/// `cands` are `(voiceprint_id, name, centroid_embedding)` triples, as
/// returned by [`Store::voiceprint_centroids`].
pub fn best_voiceprint_match(
    cands: &[(i64, String, Vec<f32>)],
    query: &[f32],
    threshold: f32,
    margin: f32,
) -> Option<(i64, String, f32)> {
    let mut scored: Vec<(usize, f32)> = cands
        .iter()
        .enumerate()
        .map(|(i, c)| (i, cosine_similarity(&c.2, query)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let &(best_i, best) = scored.first()?;
    if best < threshold {
        return None;
    }
    if let Some(&(_, second)) = scored.get(1) {
        if best - second < margin {
            return None;
        }
    }
    let c = &cands[best_i];
    Some((c.0, c.1.clone(), best))
}

/// Build a `Session` from a row selected as `(id, started_at, ended_at, title,
/// audio_path, model)`.
fn row_to_project(r: &rusqlite::Row) -> rusqlite::Result<Project> {
    let status: String = r.get(2)?;
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        status: ProjectStatus::parse(&status),
        description: r.get(3)?,
        created_at: get_u64(r, 4)?,
        updated_at: get_u64(r, 5)?,
        last_activity_at: get_u64(r, 6)?,
    })
}

/// Column list for `project_items`, shared by every item query so the indices
/// `row_to_item` reads stay in sync. `clause` is the trailing WHERE/ORDER BY.
fn item_select(clause: &str) -> String {
    format!(
        "SELECT id, project_id, kind, text, owner, status, created_session, \
         updated_session, completed_session, created_at, updated_at, manual \
         FROM project_items {clause}"
    )
}

fn row_to_item(r: &rusqlite::Row) -> rusqlite::Result<ProjectItem> {
    let kind: String = r.get(2)?;
    let status: String = r.get(5)?;
    let manual: i64 = r.get(11)?;
    Ok(ProjectItem {
        id: r.get(0)?,
        project_id: r.get(1)?,
        kind: ItemKind::parse(&kind),
        text: r.get(3)?,
        owner: r.get(4)?,
        status: ItemStatus::parse(&status),
        created_session: r.get(6)?,
        updated_session: r.get(7)?,
        completed_session: r.get(8)?,
        created_at: get_u64(r, 9)?,
        updated_at: get_u64(r, 10)?,
        manual: manual != 0,
    })
}

fn row_to_session(r: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        started_at: get_u64(r, 1)?,
        ended_at: get_opt_u64(r, 2)?,
        title: r.get(3)?,
        audio_path: r.get(4)?,
        model: r.get(5)?,
        overview_folded_ms: get_opt_u64(r, 6)?,
    })
}

fn row_to_segment(r: &rusqlite::Row) -> rusqlite::Result<Segment> {
    row_to_segment_offset(r, 0)
}

fn decode_words(words_json: Option<String>) -> Vec<Word> {
    words_json
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

fn row_to_segment_offset(r: &rusqlite::Row, off: usize) -> rusqlite::Result<Segment> {
    let id: Option<i64> = r.get(off)?;
    let source_str: String = r.get(off + 1)?;
    let words_json: Option<String> = r.get(off + 5)?;
    let words: Vec<Word> = decode_words(words_json);
    let speaker: Option<i32> = r.get(off + 6)?;
    Ok(Segment {
        id,
        source: match source_str.as_str() {
            "me" => Source::Me,
            _ => Source::Others,
        },
        t_start_ms: get_u64(r, off + 2)?,
        t_end_ms: get_u64(r, off + 3)?,
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
    // The backup is a full plaintext copy of every transcript — never leave it
    // world-readable beside the now-encrypted DB.
    restrict_to_owner(&backup);
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
    restrict_to_owner(&backup);
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
                overview_folded_ms: None,
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

#[cfg(test)]
mod ledger_tests {
    use super::*;

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zord-ledger-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        db
    }

    fn mk_session(s: &Store, id: &str, started_at: u64) {
        s.create_session(&Session {
            id: id.into(),
            started_at,
            ended_at: None,
            title: None,
            audio_path: None,
            model: "m".into(),
            overview_folded_ms: None,
        })
        .unwrap();
    }

    #[test]
    fn notes_roundtrip_and_literal_search() {
        let db = tmp_db("notes");
        let s = Store::open(&db).unwrap();
        mk_session(&s, "s1", 10);
        mk_session(&s, "s2", 20);

        assert!(s.get_notes("s1").unwrap().is_none());
        s.set_notes("s1", "Follow up: https://example.com/spec — 50% done")
            .unwrap();
        s.set_notes("s2", "  ").unwrap(); // whitespace clears
        assert_eq!(
            s.get_notes("s1").unwrap().as_deref(),
            Some("Follow up: https://example.com/spec — 50% done")
        );
        assert!(s.get_notes("s2").unwrap().is_none());

        // Literal substring (URLs aren't tokenized).
        let hits = s.search_notes("example.com/spec").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "s1");
        // `%` is matched literally, not as a LIKE wildcard.
        assert_eq!(s.search_notes("50%").unwrap().len(), 1);
        assert!(s.search_notes("nonexistent").unwrap().is_empty());
        // A bare `%` must not match everything.
        s.set_notes("s2", "no percent here").unwrap();
        assert_eq!(s.search_notes("%").unwrap().len(), 1); // only s1 contains a literal %

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn project_and_item_roundtrip() {
        let db = tmp_db("roundtrip");
        let s = Store::open(&db).unwrap();

        s.create_project(&Project {
            id: "p1".into(),
            name: "Migration".into(),
            status: ProjectStatus::Active,
            description: Some("port to new API".into()),
            created_at: 100,
            updated_at: 100,
            last_activity_at: 100,
        })
        .unwrap();

        let got = s.get_project("p1").unwrap().unwrap();
        assert_eq!(got.name, "Migration");
        assert_eq!(got.status, ProjectStatus::Active);
        assert_eq!(got.description.as_deref(), Some("port to new API"));

        s.add_item(&ProjectItem {
            id: "i1".into(),
            project_id: "p1".into(),
            kind: ItemKind::Action,
            text: "Write the adapter".into(),
            owner: Some("Alex".into()),
            status: ItemStatus::Open,
            created_session: Some("sess-a".into()),
            updated_session: Some("sess-a".into()),
            completed_session: None,
            created_at: 110,
            updated_at: 110,
            manual: false,
        })
        .unwrap();

        let items = s.list_items("p1").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ItemKind::Action);
        assert_eq!(items[0].owner.as_deref(), Some("Alex"));
        assert!(items[0].status.is_active());

        // Transition to done with provenance.
        s.update_item_status("i1", ItemStatus::Done, Some("sess-b"), Some("sess-b"), 200)
            .unwrap();
        let it = s.get_item("i1").unwrap().unwrap();
        assert_eq!(it.status, ItemStatus::Done);
        assert_eq!(it.completed_session.as_deref(), Some("sess-b"));
        assert!(!it.status.is_active());

        // Manual edit protection flag.
        s.update_item_text("i1", "Write the adapter (done)", None, 210)
            .unwrap();
        s.set_item_manual("i1", true).unwrap();
        assert!(s.get_item("i1").unwrap().unwrap().manual);

        // Archiving + ordering: active sorts before archived.
        s.create_project(&Project {
            id: "p2".into(),
            name: "Old thing".into(),
            status: ProjectStatus::Archived,
            description: None,
            created_at: 90,
            updated_at: 90,
            last_activity_at: 300,
        })
        .unwrap();
        let projects = s.list_projects().unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].id, "p1"); // active first despite older activity

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn applied_state_and_unapplied_sessions() {
        let db = tmp_db("applied");
        let s = Store::open(&db).unwrap();

        for (id, t) in [("s1", 10u64), ("s2", 20), ("s3", 30)] {
            s.create_session(&Session {
                id: id.into(),
                started_at: t,
                ended_at: None,
                title: None,
                audio_path: None,
                model: "m".into(),
                overview_folded_ms: None,
            })
            .unwrap();
        }

        assert_eq!(s.unapplied_sessions().unwrap().len(), 3);
        assert!(!s.is_session_applied("s2").unwrap());

        s.mark_session_applied("s2", Some("{\"items\":[]}"), 25)
            .unwrap();
        assert!(s.is_session_applied("s2").unwrap());

        let pending = s.unapplied_sessions().unwrap();
        assert_eq!(
            pending.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            vec!["s1", "s3"] // oldest-first, s2 folded out
        );

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn clear_ledger_wipes_everything() {
        let db = tmp_db("clear");
        let s = Store::open(&db).unwrap();
        s.create_session(&Session {
            id: "s1".into(),
            started_at: 1,
            ended_at: None,
            title: None,
            audio_path: None,
            model: "m".into(),
            overview_folded_ms: None,
        })
        .unwrap();
        s.create_project(&Project {
            id: "p1".into(),
            name: "P".into(),
            status: ProjectStatus::Active,
            description: None,
            created_at: 1,
            updated_at: 1,
            last_activity_at: 1,
        })
        .unwrap();
        s.add_item(&ProjectItem {
            id: "i1".into(),
            project_id: "p1".into(),
            kind: ItemKind::Decision,
            text: "ship it".into(),
            owner: None,
            status: ItemStatus::Done,
            created_session: Some("s1".into()),
            updated_session: Some("s1".into()),
            completed_session: Some("s1".into()),
            created_at: 1,
            updated_at: 1,
            manual: false,
        })
        .unwrap();
        s.log_history("p1", Some("i1"), "added", Some("s1"), 1)
            .unwrap();
        s.mark_session_applied("s1", None, 1).unwrap();

        s.clear_ledger().unwrap();

        assert!(s.list_projects().unwrap().is_empty());
        assert!(s.get_item("i1").unwrap().is_none());
        assert!(!s.is_session_applied("s1").unwrap());
        // Sessions themselves are untouched by a ledger wipe.
        assert_eq!(s.list_sessions().unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }
}

#[cfg(test)]
mod voiceprint_tests {
    use super::*;

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zord-vp-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        db
    }

    fn mk_store(tag: &str) -> (Store, std::path::PathBuf) {
        let db = tmp_db(tag);
        let s = Store::open(&db).unwrap();
        (s, db)
    }

    fn mk_session(s: &Store, id: &str) {
        s.create_session(&Session {
            id: id.into(),
            started_at: 1,
            ended_at: None,
            title: Some(format!("Session {id}")),
            audio_path: None,
            model: "m".into(),
            overview_folded_ms: None,
        })
        .unwrap();
    }

    #[test]
    fn voiceprint_enroll_match_and_forget() {
        let (s, db) = mk_store("enroll");

        // Two enrollments for the same name → same id, two samples.
        let id = s
            .enroll_voiceprint("Alex", "titanet_small", &[1.0, 0.0, 0.0], None)
            .unwrap();
        let id2 = s
            .enroll_voiceprint("Alex", "titanet_small", &[0.9, 0.1, 0.0], None)
            .unwrap();
        assert_eq!(id, id2);

        let cands = s.voiceprint_centroids("titanet_small").unwrap();
        assert_eq!(cands.len(), 1);

        // Centroid must be L2-normalised.
        let norm: f32 = cands[0].2.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm={norm}");

        // Different model is invisible.
        assert!(s.voiceprint_centroids("resnet34").unwrap().is_empty());

        // Forget removes the entry entirely.
        s.forget_voiceprint(id).unwrap();
        assert!(s.voiceprint_centroids("titanet_small").unwrap().is_empty());

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn voiceprint_samples_prune_to_eight() {
        let (s, db) = mk_store("prune");

        for i in 0..12i32 {
            s.enroll_voiceprint("Sam", "m", &[i as f32, 1.0], None)
                .unwrap();
        }

        let infos = s.voiceprints().unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(
            infos[0].samples, 8,
            "expected 8 samples, got {}",
            infos[0].samples
        );

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn session_speaker_embeddings_roundtrip_and_cascade() {
        let (s, db) = mk_store("cascade");
        mk_session(&s, "s1");

        s.set_session_speaker_embedding("s1", 0, "m", &[0.5, 0.5])
            .unwrap();

        let got = s.session_speaker_embedding("s1", 0).unwrap().unwrap();
        assert_eq!(got.0, "m");
        assert_eq!(got.1, vec![0.5_f32, 0.5_f32]);

        // Deleting the session should cascade-delete the embedding row.
        s.delete_session("s1").unwrap();
        assert!(
            s.session_speaker_embedding("s1", 0).unwrap().is_none(),
            "embedding should have been deleted with the session"
        );

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn me_speaker_tag_roundtrip() {
        let (s, db) = mk_store("me-speaker");
        mk_session(&s, "s1");

        // Untagged (mic/desktop sessions, or before the user's stream maps).
        assert_eq!(s.me_speaker("s1").unwrap(), None);
        s.set_me_speaker("s1", 2).unwrap();
        assert_eq!(s.me_speaker("s1").unwrap(), Some(2));
        // Unknown session → None, not an error.
        assert_eq!(s.me_speaker("nope").unwrap(), None);

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn best_match_respects_threshold_and_margin() {
        let cands = vec![
            (1i64, "Alex".to_string(), vec![1.0f32, 0.0]),
            (2i64, "Sam".to_string(), vec![0.96f32, 0.28]), // cos vs [1,0] ≈ 0.96
        ];

        // Both candidates are within 0.05 of each other → ambiguous → None.
        let m = best_voiceprint_match(&cands, &[1.0, 0.0], 0.72, 0.05);
        assert!(m.is_none(), "expected None for ambiguous match");

        // With runner-up removed, Alex matches.
        let m = best_voiceprint_match(&cands[..1], &[1.0, 0.0], 0.72, 0.05).unwrap();
        assert_eq!(m.0, 1);

        // Below threshold → None.
        assert!(
            best_voiceprint_match(&cands[..1], &[0.0, 1.0], 0.72, 0.05).is_none(),
            "expected None below threshold"
        );
    }

    #[test]
    fn rename_voiceprint_plain() {
        let (s, db) = mk_store("rename");
        let id = s
            .enroll_voiceprint("Alice", "m", &[1.0, 0.0], None)
            .unwrap();
        s.rename_voiceprint(id, "Alicia").unwrap();
        let infos = s.voiceprints().unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "Alicia");
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn rename_voiceprint_merge() {
        let (s, db) = mk_store("merge");

        // Two people — merge Bob into Alex.
        let alex_id = s.enroll_voiceprint("Alex", "m", &[1.0, 0.0], None).unwrap();
        let bob_id = s.enroll_voiceprint("Bob", "m", &[0.0, 1.0], None).unwrap();
        assert_ne!(alex_id, bob_id);

        // Give Bob 7 more samples so after merge we exceed cap.
        for i in 0..7i32 {
            s.enroll_voiceprint("Bob", "m", &[i as f32 * 0.1, 1.0], None)
                .unwrap();
        }

        // Renaming Bob → "Alex" should merge into Alex's entry.
        s.rename_voiceprint(bob_id, "Alex").unwrap();

        let infos = s.voiceprints().unwrap();
        assert_eq!(infos.len(), 1, "only Alex should remain after merge");
        assert_eq!(infos[0].name, "Alex");
        // Cap enforced on merged bank.
        assert_eq!(infos[0].samples, 8, "merged bank must be pruned to 8");

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn forget_all_voiceprints_clears_library() {
        let (s, db) = mk_store("forgetall");
        s.enroll_voiceprint("P1", "m", &[1.0], None).unwrap();
        s.enroll_voiceprint("P2", "m", &[0.0], None).unwrap();
        assert_eq!(s.voiceprints().unwrap().len(), 2);

        s.forget_all_voiceprints().unwrap();
        assert!(s.voiceprints().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn link_speaker_voiceprint_roundtrip() {
        let (s, db) = mk_store("link");
        mk_session(&s, "s1");
        let vp_id = s
            .enroll_voiceprint("Jordan", "m", &[1.0, 0.0], None)
            .unwrap();

        // speaker_names row must pre-exist for link to work.
        s.set_speaker_name("s1", 0, "Jordan").unwrap();
        s.link_speaker_voiceprint("s1", 0, vp_id).unwrap();

        // Appearances should show up in voiceprint info.
        let infos = s.voiceprints().unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].appearances.len(), 1);
        assert_eq!(infos[0].appearances[0].0, "s1");

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn model_switch_clears_old_samples() {
        let (s, db) = mk_store("modelswitch");
        for _ in 0..3 {
            s.enroll_voiceprint("Dev", "model_a", &[1.0, 0.0], None)
                .unwrap();
        }
        // Switch to a different model — old samples should be dropped.
        s.enroll_voiceprint("Dev", "model_b", &[0.0, 1.0], None)
            .unwrap();
        let infos = s.voiceprints().unwrap();
        assert_eq!(infos[0].samples, 1, "model switch should clear old samples");
        assert_eq!(infos[0].model, "model_b");
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }
}

#[cfg(test)]
mod range_delete_tests {
    use super::*;
    use zord_core::{Segment, Source};

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zord-rd-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        db
    }

    fn mk_seg(t0: u64, t1: u64) -> Segment {
        Segment {
            id: None,
            source: Source::Me,
            t_start_ms: t0,
            t_end_ms: t1,
            text: "test".into(),
            words: vec![],
            speaker: None,
        }
    }

    /// Segments that start inside [start_ms, end_ms) are deleted;
    /// those that start before or at end_ms are kept.
    #[test]
    fn delete_segments_in_range_basic() {
        let db = tmp_db("basic");
        let s = Store::open(&db).unwrap();
        s.create_session(&Session {
            id: "s1".into(),
            started_at: 0,
            ended_at: None,
            title: None,
            audio_path: None,
            model: "m".into(),
            overview_folded_ms: None,
        })
        .unwrap();

        // Insert 5 segments.
        s.insert_segment("s1", &mk_seg(0, 5_000)).unwrap(); // before → kept
        s.insert_segment("s1", &mk_seg(5_000, 8_000)).unwrap(); // at start → deleted
        s.insert_segment("s1", &mk_seg(7_000, 9_000)).unwrap(); // inside → deleted
        s.insert_segment("s1", &mk_seg(10_000, 12_000)).unwrap(); // at end → kept
        s.insert_segment("s1", &mk_seg(11_000, 14_000)).unwrap(); // after → kept

        let deleted = s.delete_segments_in_range("s1", 5_000, 10_000).unwrap();
        assert_eq!(deleted, 2, "expected 2 segments deleted, got {deleted}");

        let remaining = s.segments("s1").unwrap();
        assert_eq!(remaining.len(), 3, "expected 3 segments remaining");
        // Verify the times: 0, 10_000, 11_000 should survive.
        assert!(remaining.iter().any(|s| s.t_start_ms == 0));
        assert!(remaining.iter().any(|s| s.t_start_ms == 10_000));
        assert!(remaining.iter().any(|s| s.t_start_ms == 11_000));

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// Empty range (start == end) deletes nothing.
    #[test]
    fn delete_segments_empty_range() {
        let db = tmp_db("empty");
        let s = Store::open(&db).unwrap();
        s.create_session(&Session {
            id: "s2".into(),
            started_at: 0,
            ended_at: None,
            title: None,
            audio_path: None,
            model: "m".into(),
            overview_folded_ms: None,
        })
        .unwrap();
        s.insert_segment("s2", &mk_seg(5_000, 8_000)).unwrap();
        let deleted = s.delete_segments_in_range("s2", 5_000, 5_000).unwrap();
        assert_eq!(deleted, 0);
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }
}
