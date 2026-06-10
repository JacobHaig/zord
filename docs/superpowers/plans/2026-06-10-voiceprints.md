# Voiceprints (Phase 38) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persistent, opt-in, all-local speaker identification — once a person
has been named, future sessions label them automatically by matching speaker
embeddings against a local voiceprint library, surfaced in a new **Speakers**
icon-rail view.

**Architecture:** zord-store gains three tables (voiceprints, rolling
voiceprint_samples, per-session cluster centroids) plus a pure-Rust cosine
matcher; zord-diarize gains a standalone `SpeakerEmbedder` (sherpa-onnx
extractor we already ship); the engine persists per-cluster embeddings after
every diarization, auto-matches when enabled, enrolls on speaker rename and
from Discord ground-truth tracks; the GUI adds `View::Speakers`, a consent
dialog, and a Settings block. Everything user-visible and every embedding
computation sits behind a new `voiceprints` Cargo feature (kill-switch).

**Tech Stack:** sherpa-onnx 1.13.2 `SpeakerEmbeddingExtractor` (already a
dep), rusqlite, plain cosine similarity (no new crates).

Spec: `docs/superpowers/specs/2026-06-10-voiceprints-design.md`.
Legal posture: `docs/voiceprints-legal.md`.
Commits to `develop` per task; full gate before each.

Research-pinned parameters (do not re-derive): match thresholds strict 0.78 /
standard **0.72** / relaxed 0.66 with a 0.05 margin over the runner-up;
minimum 3 s of net speech per cluster to match or enroll; up to 30 s of
speech per embedding; rolling cap of **8** samples per person; embeddings are
only comparable within one embedding model (store + filter by model id).

---

### Task 1 (38a): zord-store — schema, CRUD, rolling samples, cosine matcher

**Files:**
- Modify: `crates/zord-store/src/lib.rs` (schema in `create_schema` ~line 719,
  late column in `add_late_columns` ~line 698, new methods after
  `speaker_names()` ~line 693, tests in the existing `#[cfg(test)]` module)

Everything here is **ungated** (no heavy deps — it's just SQLite + math); the
`voiceprints` feature gates the *producers* (Task 3) and UI (Task 4).

- [ ] **Step 1: Write failing tests** in the store test module:

```rust
#[test]
fn voiceprint_enroll_match_and_forget() {
    let store = Store::open_in_memory().unwrap(); // or the existing temp-file helper used by other tests
    // enroll twice under one name → one voiceprint, two samples
    let id = store.enroll_voiceprint("Alex", "titanet_small", &[1.0, 0.0, 0.0], None).unwrap();
    let id2 = store.enroll_voiceprint("Alex", "titanet_small", &[0.9, 0.1, 0.0], None).unwrap();
    assert_eq!(id, id2);
    let cands = store.voiceprint_centroids("titanet_small").unwrap();
    assert_eq!(cands.len(), 1);
    // centroid is L2-normalized
    let n: f32 = cands[0].2.iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!((n - 1.0).abs() < 1e-4);
    // other-model prints are invisible
    assert!(store.voiceprint_centroids("resnet34").unwrap().is_empty());
    // forget removes it
    store.forget_voiceprint(id).unwrap();
    assert!(store.voiceprint_centroids("titanet_small").unwrap().is_empty());
}

#[test]
fn voiceprint_samples_prune_to_eight() {
    let store = /* fresh store */;
    for i in 0..12 {
        store.enroll_voiceprint("Sam", "m", &[i as f32, 1.0], None).unwrap();
    }
    let infos = store.voiceprints().unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].samples, 8); // oldest pruned
}

#[test]
fn session_speaker_embeddings_roundtrip_and_cascade() {
    let store = /* fresh store with one session "s1" created via the usual create_session call */;
    store.set_session_speaker_embedding("s1", 0, "m", &[0.5, 0.5]).unwrap();
    let (model, emb) = store.session_speaker_embedding("s1", 0).unwrap().unwrap();
    assert_eq!(model, "m");
    assert_eq!(emb, vec![0.5, 0.5]);
    store.delete_session("s1").unwrap(); // whatever the existing delete API is
    assert!(store.session_speaker_embedding("s1", 0).unwrap().is_none());
}

#[test]
fn best_match_respects_threshold_and_margin() {
    let cands = vec![
        (1i64, "Alex".to_string(), vec![1.0, 0.0]),
        (2i64, "Sam".to_string(), vec![0.96, 0.28]), // cos vs query ≈ 0.96
    ];
    // clear winner above threshold
    let m = best_voiceprint_match(&cands, &[1.0, 0.0], 0.72, 0.05);
    // both candidates are within 0.05 of each other → ambiguous → None
    assert!(m.is_none());
    // with the runner-up removed, Alex matches
    let m = best_voiceprint_match(&cands[..1], &[1.0, 0.0], 0.72, 0.05).unwrap();
    assert_eq!(m.0, 1);
    // below threshold → None
    assert!(best_voiceprint_match(&cands[..1], &[0.0, 1.0], 0.72, 0.05).is_none());
}
```

- [ ] **Step 2: Run** `cargo test -p zord-store` — new tests FAIL (methods missing).

- [ ] **Step 3: Schema.** In `create_schema` add (mirroring existing style):

```sql
CREATE TABLE IF NOT EXISTS voiceprints (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT NOT NULL UNIQUE,
  model      TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS voiceprint_samples (
  voiceprint_id INTEGER NOT NULL REFERENCES voiceprints(id) ON DELETE CASCADE,
  session_id    TEXT,
  embedding     BLOB NOT NULL,
  created_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS session_speaker_embeddings (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  speaker    INTEGER NOT NULL,
  model      TEXT NOT NULL,
  embedding  BLOB NOT NULL,
  PRIMARY KEY (session_id, speaker)
);
```

In `add_late_columns` add (with a Phase-38 comment like its neighbors):

```rust
let _ = conn.execute("ALTER TABLE speaker_names ADD COLUMN voiceprint_id INTEGER", []);
```

NOTE: session deletion must cascade — check how sessions are deleted today;
if `PRAGMA foreign_keys` isn't enabled on open, delete
`session_speaker_embeddings` rows explicitly wherever `speaker_names` rows
are deleted for a session.

- [ ] **Step 4: Blob codec + matcher** (free functions in lib.rs, `pub` for engine reuse):

```rust
/// Embeddings are stored as little-endian f32 blobs.
fn embedding_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}
fn blob_to_embedding(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return -1.0; }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 { -1.0 } else { dot / (na * nb) }
}

/// Pick the enrolled voiceprint for `query`: best cosine must clear
/// `threshold` AND beat the runner-up by `margin` (open-set safety).
pub fn best_voiceprint_match(
    cands: &[(i64, String, Vec<f32>)],
    query: &[f32],
    threshold: f32,
    margin: f32,
) -> Option<(i64, String, f32)> {
    let mut scored: Vec<(usize, f32)> = cands.iter().enumerate()
        .map(|(i, c)| (i, cosine_similarity(&c.2, query)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (best_i, best) = *scored.first()?;
    if best < threshold { return None; }
    if let Some(&(_, second)) = scored.get(1) {
        if best - second < margin { return None; }
    }
    let c = &cands[best_i];
    Some((c.0, c.1.clone(), best))
}
```

- [ ] **Step 5: CRUD methods** on `Store` (after `speaker_names()`):

```rust
const VOICEPRINT_SAMPLE_CAP: i64 = 8;

/// One known person in the voiceprint library (Speakers view row).
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceprintInfo {
    pub id: i64,
    pub name: String,
    pub model: String,
    pub samples: u32,
    pub updated_at: u64,
    /// (session_id, session_title) pairs where this person was identified.
    pub appearances: Vec<(String, String)>,
}

/// Enroll/refresh `name`'s voiceprint with one more embedding sample. Creates
/// the voiceprint if new; switching embedding models restarts its samples
/// (vectors aren't comparable across models). Prunes to the 8 newest samples.
pub fn enroll_voiceprint(&self, name: &str, model: &str, embedding: &[f32], session_id: Option<&str>) -> Result<i64> {
    let now = unix_now(); // reuse the existing timestamp helper, or SystemTime::now epoch secs
    let tx = self.conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO voiceprints (name, model, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(name) DO UPDATE SET updated_at = ?3",
        params![name.trim(), model, now],
    )?;
    let id: i64 = tx.query_row("SELECT id FROM voiceprints WHERE name = ?1", params![name.trim()], |r| r.get(0))?;
    let old_model: String = tx.query_row("SELECT model FROM voiceprints WHERE id = ?1", params![id], |r| r.get(0))?;
    if old_model != model {
        tx.execute("DELETE FROM voiceprint_samples WHERE voiceprint_id = ?1", params![id])?;
        tx.execute("UPDATE voiceprints SET model = ?2 WHERE id = ?1", params![id, model])?;
    }
    tx.execute(
        "INSERT INTO voiceprint_samples (voiceprint_id, session_id, embedding, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, session_id, embedding_to_blob(embedding), now],
    )?;
    tx.execute(
        "DELETE FROM voiceprint_samples WHERE rowid IN (
           SELECT rowid FROM voiceprint_samples WHERE voiceprint_id = ?1
           ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?2)",
        params![id, VOICEPRINT_SAMPLE_CAP],
    )?;
    tx.commit()?;
    Ok(id)
}

/// L2-normalized mean embedding per enrolled person, for `model` only.
pub fn voiceprint_centroids(&self, model: &str) -> Result<Vec<(i64, String, Vec<f32>)>> {
    // SELECT v.id, v.name, s.embedding FROM voiceprints v JOIN voiceprint_samples s ...
    // WHERE v.model = ?1; group in Rust: sum vectors per id, divide by count,
    // then divide by the L2 norm (skip people whose norm is 0).
}

pub fn voiceprints(&self) -> Result<Vec<VoiceprintInfo>> {
    // voiceprints LEFT JOIN sample counts; appearances via
    // SELECT sn.session_id, se.title FROM speaker_names sn JOIN sessions se ON se.id = sn.session_id
    // WHERE sn.voiceprint_id = ?1 ORDER BY se.started_at DESC
    // (use whatever the sessions title/started_at columns are actually named).
}

pub fn rename_voiceprint(&self, id: i64, name: &str) -> Result<()> {
    // If `name` already exists (another id): move this id's samples to the
    // target (then prune target to 8), repoint speaker_names.voiceprint_id,
    // delete the source row — a merge. Otherwise a plain UPDATE.
}

pub fn forget_voiceprint(&self, id: i64) -> Result<()> {
    let tx = self.conn.unchecked_transaction()?;
    tx.execute("UPDATE speaker_names SET voiceprint_id = NULL WHERE voiceprint_id = ?1", params![id])?;
    tx.execute("DELETE FROM voiceprints WHERE id = ?1", params![id])?; // samples cascade
    tx.commit()?;
    Ok(())
}

pub fn forget_all_voiceprints(&self) -> Result<()> { /* same shape, no WHERE */ }

pub fn set_session_speaker_embedding(&self, session_id: &str, speaker: i32, model: &str, embedding: &[f32]) -> Result<()> {
    // INSERT OR REPLACE INTO session_speaker_embeddings
}
pub fn session_speaker_embedding(&self, session_id: &str, speaker: i32) -> Result<Option<(String, Vec<f32>)>> { /* SELECT model, embedding */ }

/// Record which enrolled person a session speaker was matched/enrolled to.
pub fn link_speaker_voiceprint(&self, session_id: &str, speaker: i32, voiceprint_id: i64) -> Result<()> {
    // UPDATE speaker_names SET voiceprint_id = ?3 WHERE session_id = ?1 AND speaker = ?2
    // (row exists because set_speaker_name ran first — call order matters).
}
```

NOTE (cascade caveat from Step 3): if `delete_session` cleans tables
manually, add `DELETE FROM session_speaker_embeddings WHERE session_id = ?1`
beside the `speaker_names` cleanup, and the same inside `clear_speakers`
**only when re-diarizing would recompute them** — actually keep them in
`clear_speakers`: the new diarize pass overwrites via INSERT OR REPLACE, so
do NOT touch them there.

- [ ] **Step 6:** `cargo test -p zord-store` green; fix clippy.
- [ ] **Step 7: Commit** `feat(store): voiceprint library — schema, rolling samples, cosine matcher (Phase 38a)`

---

### Task 2 (38b): zord-diarize — `SpeakerEmbedder` + speech gathering

**Files:**
- Create: `crates/zord-diarize/src/embedder.rs`
- Modify: `crates/zord-diarize/src/lib.rs` — `mod embedder;` + re-exports,
  gated exactly like the existing `diarizer` module (`sherpa` feature)

- [ ] **Step 1: `gather_speech` + failing test** (pure DSP, testable without models):

```rust
/// Energy-gated speech gathering: keep ~0.48 s frames whose RMS clears a
/// floor relative to the loudest frame, up to `max_secs` of audio. Good
/// enough to skip silence on a single-speaker track (Discord per-participant
/// tracks); NOT a diarizer.
pub fn gather_speech(samples: &[f32], rate: u32, max_secs: u32) -> Vec<f32> {
    let frame = (rate as usize / 1000) * 480; // 480 ms
    if frame == 0 || samples.is_empty() { return Vec::new(); }
    let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
    let peak = samples.chunks(frame).map(rms).fold(0.0f32, f32::max);
    let floor = (peak * 0.1).max(1e-4);
    let mut out = Vec::new();
    let cap = (rate as usize) * max_secs as usize;
    for chunk in samples.chunks(frame) {
        if rms(chunk) >= floor {
            out.extend_from_slice(chunk);
            if out.len() >= cap { out.truncate(cap); break; }
        }
    }
    out
}

#[test]
fn gather_speech_skips_silence_and_caps() {
    let rate = 16_000;
    let mut s = vec![0.0f32; rate as usize];           // 1 s silence
    s.extend((0..rate).map(|i| (i as f32 * 0.05).sin() * 0.3)); // 1 s tone
    let speech = gather_speech(&s, rate, 30);
    let secs = speech.len() as f32 / rate as f32;
    assert!(secs > 0.7 && secs < 1.3, "kept ~the tone second, got {secs}");
    assert!(gather_speech(&s, rate, 0).is_empty() || gather_speech(&s, rate, 0).len() == 0);
}
```

- [ ] **Step 2:** test green (`cargo test -p zord-diarize --features sherpa` —
  match however the existing diarize tests run; if the crate tests need
  `sherpa`, keep `gather_speech` outside the gated module so it tests in the
  default build).

- [ ] **Step 3: `SpeakerEmbedder`** (sherpa-gated; mirrors `LiveLabeler::new`,
  diarizer.rs:380-399):

```rust
/// Standalone speaker-embedding extractor for voiceprints (Phase 38): turns
/// speech into the same vectors the diarizer clusters with, so per-cluster
/// centroids can be matched against the persistent library.
pub struct SpeakerEmbedder {
    extractor: SpeakerEmbeddingExtractor,
}

impl SpeakerEmbedder {
    pub fn load(model: EmbeddingModel) -> Result<Self> {
        let emb = embedding_path(model)?;
        if !emb.exists() {
            anyhow::bail!("speaker-embedding model is not downloaded yet");
        }
        let extractor = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: to_cfg_path(&emb),
            ..Default::default()
        })
        .ok_or_else(|| anyhow!("failed to create the embedding extractor"))?;
        Ok(Self { extractor })
    }

    /// Embed one mono utterance; `None` if too short for the model. Output is
    /// L2-normalized.
    pub fn embed(&self, samples: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(sample_rate as i32, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) { return None; }
        let mut e = self.extractor.compute(&stream)?;
        l2_normalize(&mut e);
        Some(e)
    }

    /// Per-cluster centroid embeddings for diarized spans: for each speaker,
    /// feed its longest spans (up to `MAX_SPEECH_SECS` = 30 s total) into one
    /// stream and embed. Clusters with under `MIN_SPEECH_SECS` = 3 s of
    /// speech are skipped (research floor — unreliable below that).
    pub fn embed_clusters(
        &self,
        samples: &[f32],
        sample_rate: u32,
        spans: &[DiarSegment],
    ) -> HashMap<i32, Vec<f32>> {
        let ms_to_idx = |ms: u64| ((ms as usize) * sample_rate as usize / 1000).min(samples.len());
        let mut by_speaker: HashMap<i32, Vec<&DiarSegment>> = HashMap::new();
        for sp in spans { by_speaker.entry(sp.speaker).or_default().push(sp); }
        let mut out = HashMap::new();
        for (speaker, mut sps) in by_speaker {
            sps.sort_by_key(|s| std::cmp::Reverse(s.end_ms - s.start_ms));
            let total_ms: u64 = sps.iter().map(|s| s.end_ms - s.start_ms).sum();
            if total_ms < 3_000 { continue; }
            let Some(stream) = self.extractor.create_stream() else { continue };
            let mut fed_ms = 0u64;
            for sp in sps {
                let (a, b) = (ms_to_idx(sp.start_ms), ms_to_idx(sp.end_ms));
                if a >= b { continue; }
                stream.accept_waveform(sample_rate as i32, &samples[a..b]);
                fed_ms += sp.end_ms - sp.start_ms;
                if fed_ms >= 30_000 { break; }
            }
            stream.input_finished();
            if !self.extractor.is_ready(&stream) { continue; }
            if let Some(mut e) = self.extractor.compute(&stream) {
                l2_normalize(&mut e);
                out.insert(speaker, e);
            }
        }
        out
    }
}

fn l2_normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 { v.iter_mut().for_each(|x| *x /= n); }
}
```

(Imports/`embedding_path`/`to_cfg_path` are in diarizer.rs — either move the
two helpers to a shared spot or `use super::diarizer::…`; keep it mechanical.)

Add one `#[ignore]`d model test: embed two different synthetic "voices"
(filtered noise at different spectral tilts won't be meaningful — instead
just assert `embed` returns `Some` of nonzero dim on 5 s of any audio when
the model is present locally).

- [ ] **Step 4:** `cargo clippy -p zord-diarize --features sherpa -- -D warnings` + tests green.
- [ ] **Step 5: Commit** `feat(diarize): standalone SpeakerEmbedder + speech gathering (Phase 38b)`

---

### Task 3 (38c): feature flag, settings, engine wiring

**Files:**
- Modify: `crates/zord-gui/Cargo.toml` — add feature
- Modify: `crates/zord-config/src/lib.rs` — settings + threshold mapping
- Modify: `crates/zord-gui/src/engine.rs` — DbCmds/Events, `apply_diarization`,
  `RenameSpeaker` arm (~line 1599), Discord post-stop enrollment

- [ ] **Step 1: Feature flag** in zord-gui's `[features]` (beside `diarization`):

```toml
# Persistent cross-session speaker identification (voiceprints). Opt-in at
# runtime AND removable at build time (docs/voiceprints-legal.md).
voiceprints = ["diarization"]
```

- [ ] **Step 2: Settings (TDD)** in zord-config, following the
  `compress_after_days` pattern (serde default fns + Default impl + comments):

```rust
/// Voiceprints (Phase 38): match speakers against the local library and
/// auto-name them. Requires the one-time consent flow; off by default.
#[serde(default)]
pub voiceprints_enabled: bool,
/// Match strictness preset: "strict" | "standard" | "relaxed".
#[serde(default = "default_voiceprints_match")]
pub voiceprints_match: String,
/// Unix time the user accepted the voiceprint consent dialog (0 = never).
#[serde(default)]
pub voiceprints_consented_at: u64,
```

plus the mapping + test:

```rust
/// Cosine threshold for a voiceprint match (research-tuned presets).
pub fn voiceprint_threshold(preset: &str) -> f32 {
    match preset { "strict" => 0.78, "relaxed" => 0.66, _ => 0.72 }
}

#[test]
fn voiceprint_defaults_and_thresholds() {
    let s = Settings::default();
    assert!(!s.voiceprints_enabled);
    assert_eq!(s.voiceprints_consented_at, 0);
    assert_eq!(voiceprint_threshold(&s.voiceprints_match), 0.72);
    assert_eq!(voiceprint_threshold("strict"), 0.78);
    assert_eq!(voiceprint_threshold("bogus"), 0.72);
}
```

- [ ] **Step 3: DbCmds + Event** in engine.rs (UNGATED — store is always
  compiled; only producers are gated). Near `RenameSpeaker` in the `DbCmd`
  enum (~line 401):

```rust
/// Voiceprint library (Phase 38): list / rename / forget. Replies with
/// `Event::Voiceprints`.
Voiceprints,
VoiceprintRename { id: i64, name: String },
VoiceprintForget { id: i64 },
VoiceprintForgetAll,
```

`Event` enum gains `Voiceprints(Vec<zord_store::VoiceprintInfo>)`. db_loop
arms: each mutation calls the store method, then sends a refreshed
`Event::Voiceprints(store.voiceprints().unwrap_or_default())`; plain
`Voiceprints` just sends the list.

- [ ] **Step 4: `apply_diarization` hook.** After the assignment write-back
  (the `store.set_segment_speaker` loop, engine.rs:2056-2059) and BEFORE the
  transcript refresh, add:

```rust
#[cfg(feature = "voiceprints")]
apply_voiceprints(store, session_id, &samples, &spans, model, ev);
```

and the new function in the same file:

```rust
/// Phase 38: persist per-cluster embeddings for this session, and (when the
/// user opted in) match them against the voiceprint library to auto-name
/// speakers. Best-effort — failures notice and return, never blocking the
/// diarization result.
#[cfg(feature = "voiceprints")]
fn apply_voiceprints(
    store: &Store,
    session_id: &str,
    samples: &[f32],
    spans: &[zord_diarize::DiarSegment],
    model: zord_diarize::EmbeddingModel,
    ev: &UnboundedSender<Event>,
) {
    let embedder = match zord_diarize::SpeakerEmbedder::load(model) {
        Ok(e) => e,
        Err(e) => { let _ = ev.send(Event::Notice(format!("voiceprints: {e}"))); return; }
    };
    let clusters = embedder.embed_clusters(samples, 16_000, spans);
    for (speaker, emb) in &clusters {
        let _ = store.set_session_speaker_embedding(session_id, *speaker, model.name(), emb);
    }
    let settings = zord_config::Settings::load();
    if !settings.voiceprints_enabled { return; }
    let cands = store.voiceprint_centroids(model.name()).unwrap_or_default();
    if cands.is_empty() { return; }
    let threshold = zord_config::voiceprint_threshold(&settings.voiceprints_match);
    let mut recognized: Vec<String> = Vec::new();
    for (speaker, emb) in &clusters {
        if let Some((vid, name, _score)) = zord_store::best_voiceprint_match(&cands, emb, threshold, 0.05) {
            let _ = store.set_speaker_name(session_id, *speaker, &name);
            let _ = store.link_speaker_voiceprint(session_id, *speaker, vid);
            recognized.push(name);
        }
    }
    if !recognized.is_empty() {
        let _ = ev.send(Event::Notice(format!("Recognized {}.", recognized.join(", "))));
    }
}
```

(`model.name()` — confirm the accessor used at engine.rs:1978; reuse it.
The samples passed are the 16 kHz mono stream `apply_diarization` already
loaded, and `spans` is in scope — only the call site placement matters:
after `clear_speakers` + assignment writes so auto-names aren't wiped, and
before the `Event::Transcript`/`Event::Speakers` refresh so the UI shows
them. Move those refresh sends after the hook if needed.)

- [ ] **Step 5: Enroll on rename.** In the `DbCmd::RenameSpeaker` arm
  (engine.rs:1599-1603), after `set_speaker_name` succeeds:

```rust
#[cfg(feature = "voiceprints")]
{
    let settings = zord_config::Settings::load();
    if settings.voiceprints_enabled && !name.trim().is_empty() {
        if let Ok(Some((model, emb))) = store.session_speaker_embedding(&id, speaker) {
            if let Ok(vid) = store.enroll_voiceprint(name.trim(), &model, &emb, Some(&id)) {
                let _ = store.link_speaker_voiceprint(&id, speaker, vid);
                let _ = ev.send(Event::Voiceprints(store.voiceprints().unwrap_or_default()));
            }
        }
    }
}
```

(Renaming to an existing person's name merges into that voiceprint —
`enroll_voiceprint` upserts by name, which is exactly the desired semantics.)

- [ ] **Step 6: Discord ground-truth enrollment.** In the integration
  post-stop path (where `post_transcribe_inner` walks the session's
  `spk-*.wav`/`me` tracks with their speaker indices), after transcription
  completes, add a gated pass:

```rust
#[cfg(feature = "voiceprints")]
fn enroll_integration_tracks(store: &Store, session_id: &str, tracks: &[(i32, PathBuf)], ev: &UnboundedSender<Event>) {
    let settings = zord_config::Settings::load();
    if !settings.voiceprints_enabled { return; }
    let names = store.speaker_names(session_id).unwrap_or_default(); // ground truth from Discord events
    let model = zord_diarize::EmbeddingModel::parse_or_default(&settings.diarize_embedding_model);
    let Ok(embedder) = zord_diarize::SpeakerEmbedder::load(model) else { return; };
    let mut enrolled = 0;
    for (speaker, path) in tracks {
        let Some(name) = names.get(speaker).filter(|n| !n.starts_with("Speaker ")) else { continue };
        let Ok(samples) = zord_audio::read_audio_mono_16k(path) else { continue };
        let speech = zord_diarize::gather_speech(&samples, 16_000, 30);
        if speech.len() < 3 * 16_000 { continue; } // < 3 s of speech — skip
        let Some(emb) = embedder.embed(&speech, 16_000) else { continue };
        let _ = store.set_session_speaker_embedding(session_id, *speaker, model.name(), &emb);
        if let Ok(vid) = store.enroll_voiceprint(name, model.name(), &emb, Some(session_id)) {
            let _ = store.link_speaker_voiceprint(session_id, *speaker, vid);
            enrolled += 1;
        }
    }
    if enrolled > 0 {
        let _ = ev.send(Event::Notice(format!("Saved voiceprints for {enrolled} Discord speaker(s).")));
        let _ = ev.send(Event::Voiceprints(store.voiceprints().unwrap_or_default()));
    }
}
```

The `tracks` list mirrors what `post_transcribe_inner` already enumerates —
thread it (or re-enumerate the session folder the same way) from the spot
where the integration session finishes transcribing. Skip placeholder
"Speaker N" names (unmapped-SSRC fallback) — they're not identities.
NOTE: the model download may not have happened if the user never diarized;
`SpeakerEmbedder::load` errors cleanly in that case (bail, no notice spam).

- [ ] **Step 7: Gate.** `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo clippy -p zord-gui --features voiceprints -- -D warnings && cargo test --workspace`
- [ ] **Step 8: Commit** `feat(engine): voiceprint matching + enrollment seams (Phase 38c)`

---

### Task 4 (38d): UI — Speakers view, consent flow, settings block

**Files:**
- Create: `crates/zord-gui/src/speakers.rs`
- Modify: `crates/zord-gui/src/main.rs` — `mod speakers;`, `View::Speakers`,
  IconRail button (main.rs:1370-1413), view routing, `Event::Voiceprints`
  handling, `SpeakersSettings` block (main.rs:2652)
- Modify: `crates/zord-gui/src/style.css` — Speakers view styles from tokens

- [ ] **Step 1: View variant + rail icon.** Add `Speakers` to `enum View`
  (main.rs:169). In `IconRail`, under the Search button (so the order is
  Overview → Search → Speakers, per the user's placement):

```rust
if cfg!(feature = "voiceprints") {
    button {
        class: if matches!(&*view.read(), View::Speakers) { "rail-btn active" } else { "rail-btn" },
        title: "Speakers — people Zord can recognize across meetings",
        onclick: move |e| on_open_speakers.call(e),
        {icon("people")}
    }
}
```

New `on_open_speakers: EventHandler<MouseEvent>` prop wired like
`on_open_search` (main.rs:1003): sets `View::Speakers` and sends
`DbCmd::Voiceprints` to refresh the list. Add a `"people"` glyph to the
`icon()` helper (two-heads outline, same stroke style as the others).

- [ ] **Step 2: State + event plumbing.** In MainApp: a
  `voiceprints: Signal<Vec<zord_store::VoiceprintInfo>>`; in the event loop,
  `Event::Voiceprints(v) => voiceprints.set(v)`. Request once at startup too
  (harmless when empty).

- [ ] **Step 3: SpeakersView component** in `speakers.rs` (styled like the
  existing panels; all engine traffic via `EventHandler` props or direct
  `engine.db_tx` sends, following the file's conventions):

States to render:
1. **Feature off (runtime)** — `!settings.voiceprints_enabled`: hero card
   explaining the feature (one short paragraph: "Zord can remember voices —
   stored only on this device — and name people automatically in future
   meetings") + an **Enable** button that opens the consent dialog.
2. **Enabled, empty library** — how enrollment works: "Name a speaker on any
   transcript, or record a Discord call — people you name are remembered."
3. **Enabled, with people** — a card per person:
   name (inline-editable like session rename → `DbCmd::VoiceprintRename`),
   "{samples} voice sample(s) · last updated {date}", appearance chips
   (session title, click → `on_open_session(session_id)` prop reusing the
   sidebar's open handler), and a **Forget this voice** button (confirm
   dialog, danger styling → `DbCmd::VoiceprintForget`). Show a subtle
   "re-enroll needed (model changed)" tag when `info.model` differs from the
   current `diarize_embedding_model` setting.

**Consent dialog** (also in speakers.rs, reused by Settings): modal in the
existing dialog style; title "Remember voices on this device"; body bullets —
what is stored (voice patterns, not recordings), where (only this computer,
never uploaded), legal note (voice patterns are biometric data in some
jurisdictions), control ("Forget any person, or everything, anytime");
buttons Cancel / **I agree — enable**. On accept:

```rust
let mut s = zord_config::Settings::load();
s.voiceprints_enabled = true;
s.voiceprints_consented_at = now_unix();
let _ = s.save();
```

(then refresh whatever settings signal MainApp holds, same as other settings
writes.)

- [ ] **Step 4: Settings → Speakers block.** In `SpeakersSettings`
  (main.rs:2652), append a "Voice identification" group, gated by
  `cfg!(feature = "voiceprints")`:
  - Enable toggle — turning ON with `voiceprints_consented_at == 0` opens the
    consent dialog instead of flipping directly; turning OFF just writes the
    setting (library kept).
  - Match strictness `select`: Strict / Standard / Relaxed →
    `voiceprints_match` ("fewer wrong names" / "balanced" / "names more
    readily").
  - **Forget all voices** button (confirm) → `DbCmd::VoiceprintForgetAll`.
  - A one-line pointer: "Manage individual people in the Speakers view."

- [ ] **Step 5: CSS.** Compose from the 36a tokens: `.speaker-card` (panel
  radius/elevation, pop-in entrance), `.speaker-chip` (appearance pills),
  danger styling on Forget via the existing danger button class. No new
  colors — accent/danger roles only.

- [ ] **Step 6: Gate** incl. `cargo clippy -p zord-gui --features voiceprints,discord,parakeet -- -D warnings`; run the app with `--features voiceprints,parakeet` and click through: enable → consent → empty state.
- [ ] **Step 7: Commit** `feat(gui): Speakers view, consent flow, voiceprint settings (Phase 38d)`

---

### Task 5 (38e): docs, CI, close-out

- [ ] **Step 1: PLAN.md** — Phase 38 entry in section 9 (after Phase 36,
  matching the ✅-style of 37) describing: voiceprint library, implicit
  enrollment (rename + Discord), matching parameters, consent/forget
  controls, `voiceprints` feature kill-switch, pointer to
  `docs/voiceprints-legal.md` + the spec.
- [ ] **Step 2: KICKSTART.md** — add `voiceprints` to the feature-flag table
  (`requires diarization; opt-in speaker memory`); README gets one selling
  bullet under the feature list: "**Remembers voices (opt-in):** name someone
  once and Zord labels them automatically in future meetings — voiceprints
  are local-only and deletable per person."
- [ ] **Step 3: CI** — if the PR workflow's heavy-feature clippy job lists
  features explicitly, add `voiceprints`; releases: add to the
  all-features release build list (it's removable later — that's the point
  of the flag).
- [ ] **Step 4: repo memory** — update `.claude/memory/` (platform/feature
  index entries) noting the voiceprints architecture + the legal memo
  location.
- [ ] **Step 5: Full gate** (workspace clippy `-D warnings`, gui clippy with
  `voiceprints,discord,parakeet`, `cargo test --workspace`), push develop,
  ff-merge main, push both.
- [ ] **Step 6: Manual pass with the user**:
  1. Build with `voiceprints,parakeet`; enable in Settings → consent dialog.
  2. Record session A (mic + desktop), diarize, rename "Speaker 1" → a name
     → Speakers view shows the person with 1 sample.
  3. Record session B with the same person speaking ≥ 3 s → after
     diarization they're auto-named ("Recognized …" notice).
  4. Discord call → participants appear in the Speakers view afterward.
  5. Forget the person → record again → back to "Speaker N".
  6. Build WITHOUT the flag → no rail icon, no settings block, no new rows.

---

## Self-review

- **Spec coverage:** schema/CRUD/matcher (T1), embedder + cluster centroids +
  speech gathering (T2), flag/settings/auto-match/rename-enroll/Discord-enroll
  (T3), Speakers view + consent + settings + forget (T4), docs/CI/legal memo
  pointer (T5). Edge cases from the spec: model-switch → re-enroll tag (T4) +
  sample reset on model change (T1 `enroll_voiceprint`); <3 s clusters skipped
  (T2); ambiguous match → no assign (T1 margin logic); flag-off build → no
  computation, no UI (T3 cfg + T4 cfg!).
- **Placeholder scan:** `voiceprint_centroids` / `voiceprints()` /
  `rename_voiceprint` bodies are described-not-coded (SQL shapes given, exact
  column names verified in-file by the implementer) — acceptable: the queries
  depend on existing column names the engineer must read anyway; behaviors
  are pinned by the tests in T1 Step 1.
- **Type consistency:** `best_voiceprint_match(&[(i64, String, Vec<f32>)],
  &[f32], f32, f32) -> Option<(i64, String, f32)>` used identically in T1/T3;
  `VoiceprintInfo` fields used by T4 (`samples`, `updated_at`, `model`,
  `appearances`) all defined in T1; `enroll_voiceprint` signature matches all
  three call sites (T3 ×2, merge semantics noted).
- **Call-order constraint** (set_speaker_name BEFORE link_speaker_voiceprint)
  flagged at both definition (T1) and call sites (T3 Steps 4-6).
