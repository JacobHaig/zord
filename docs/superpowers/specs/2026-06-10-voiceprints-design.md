# Voiceprints — persistent cross-session speaker identification (Phase 38)

**Date:** 2026-06-10 · **Status:** approved (research-backed; see
`docs/voiceprints-legal.md` for the legal posture)

## Goal

Zord remembers who people are. After a speaker has been named once, future
recordings identify and label them **automatically** — no manual assignment.
"Voiceprint" = a speaker embedding vector (192–512 f32 values, a few KB), not
an audio clip; nothing biometric ever leaves the device.

## Decisions (user-confirmed)

- **New sidebar destination**: a **Speakers** view in the icon rail, under
  Overview and Search — the voiceprint library (who Zord knows, where they
  appeared, rename / **Forget this voice**).
- **Cargo feature flag `voiceprints`** (requires `diarization`) — the
  kill-switch: removing the flag from release builds removes the entire
  capability if the legal picture sours.
- **Legality handled as a doc for now**: `docs/voiceprints-legal.md` records
  the research (BIPA/CUBI/GDPR, the Fireflies/Otter/Apple litigation wave,
  local-only analysis) and the mitigations we build in anyway.
- Phased implementation like prior initiatives (38a–38e below / plan doc).

## How it works (research-backed parameters)

- **Embedding source**: the sherpa-onnx `SpeakerEmbeddingExtractor` we
  already ship (same ONNX models the diarizer downloads). Embeddings are only
  comparable **within one model** — every stored vector carries its model id,
  and matching skips prints from other models.
- **Matching**: plain cosine similarity against per-speaker centroids
  (L2-normalized mean of the rolling sample set). Auto-assign when
  `best ≥ threshold` AND `best − second_best ≥ 0.05` (open-set margin).
  Threshold presets: strict 0.78 / **standard 0.72** / relaxed 0.66.
  Clusters with **< 3 s of net speech are never matched** (reliability floor).
- **Enrollment is implicit, never a chore**:
  1. **Renaming a speaker** after a meeting (the existing flow) enrolls or
     updates that person's voiceprint from the session's cluster centroid.
  2. **Discord sessions auto-enroll**: per-participant tracks come with real
     names — the highest-quality voiceprint source, no clustering involved.
- **Voiceprint = rolling history**: keep the **8 most recent** session
  centroids per person (drift mitigation); the match centroid is their
  normalized mean. Re-enrollment continuously refreshes.
- Opus-compressed sessions are fine: 24–32 kbps is near-transparent to these
  embeddings (research: EER 0.86% → ~1.10% at 24 kbps).

## Architecture

### zord-store (schema + matcher; not feature-gated — no heavy deps)

```sql
CREATE TABLE IF NOT EXISTS voiceprints (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT NOT NULL UNIQUE,
  model      TEXT NOT NULL,            -- embedding model id
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS voiceprint_samples (   -- rolling K=8 per person
  voiceprint_id INTEGER NOT NULL REFERENCES voiceprints(id) ON DELETE CASCADE,
  session_id    TEXT,
  embedding     BLOB NOT NULL,         -- f32 little-endian
  created_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS session_speaker_embeddings ( -- per-session cluster centroids
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  speaker    INTEGER NOT NULL,
  model      TEXT NOT NULL,
  embedding  BLOB NOT NULL,
  PRIMARY KEY (session_id, speaker)
);
-- speaker_names gains a nullable voiceprint_id column (add_late_columns
-- pattern) linking session speakers to identities → powers "appeared in".
```

API: `upsert_voiceprint(name, model, embedding, session) -> id` (insert or
add sample + prune to 8), `voiceprint_centroids(model) -> Vec<(id, name, Vec<f32>)>`,
`delete_voiceprint(id)` (cascade + unlink speaker_names),
`rename_voiceprint(id, name)`, `voiceprint_appearances(id)`,
set/get for `session_speaker_embeddings`. Cosine matcher
(`match_voiceprint(centroids, query, threshold, margin)`) is pure Rust in
zord-core or zord-store — unit-tested with synthetic vectors.

### zord-diarize (under the existing `sherpa` feature)

- `diarize_with_embeddings(...) -> (Vec<DiarSegment>, HashMap<i32, Vec<f32>>)`:
  run the existing pipeline for labels, then a standalone
  `SpeakerEmbeddingExtractor` over each cluster's speech windows (longest
  windows first, up to ~30 s per cluster, skip clusters < 3 s), L2-normalized
  mean per cluster. Model = the configured `diarize_embedding_model`.
- `embed_speech(samples, rate) -> Option<Vec<f32>>`: VAD-gather up to 30 s of
  speech from a track and embed — the Discord-track path.

### Engine (zord-gui, feature `voiceprints = ["diarization"]`)

- **After diarization** (`apply_diarization`): persist per-cluster centroids
  to `session_speaker_embeddings`; if voiceprints are enabled, match each
  against the library → auto `set_speaker_name` + link `voiceprint_id`;
  unmatched clusters stay "Speaker N". Notice summarizes ("Recognized Alex
  and Sam").
- **On rename** (`DbCmd::RenameSpeaker`): if a session centroid exists for
  that speaker, enroll/update the voiceprint under the new name (renaming to
  an existing name merges into it).
- **Discord teardown**: embed each per-speaker track via `embed_speech`,
  auto-enroll under the ground-truth name; also auto-link.
- Settings: `voiceprints_enabled: bool` (default **false**),
  `voiceprints_match: String` ("strict"/"standard"/"relaxed"),
  `voiceprints_consented_at: u64` (0 = never; set by the consent dialog —
  the written-authorization analog).

### UI

- **Icon rail**: a "users" icon under Overview and Search → `View::Speakers`
  (rendered only in `voiceprints` builds).
- **Speakers view**: list of known people — name, sample count, last heard,
  sessions appeared in (click → open session); per-row **Rename** and
  **Forget this voice** (confirm dialog; deletes prints + samples + unlinks).
  Empty/disabled states explain enrollment and link the enable flow.
- **Consent gate**: enabling (Speakers view banner or Settings → Speakers)
  shows a one-time plain-language dialog (biometric data, stored locally
  only, never uploaded, forgettable per person) — affirmative click required;
  stamps `voiceprints_consented_at`.
- **Settings → Speakers**: a "Voice identification (beta)" block — enable
  toggle (through consent), match-strictness select, "Forget all voices".

## Edge cases

- Embedding-model switch: old prints carry their model id; matching skips
  them; the Speakers view shows them with a "re-enroll" hint. No migration.
- Short clusters (< 3 s speech): never matched, never enrolled.
- Two enrolled people matching one cluster within the margin: no auto-assign
  (correctness over coverage).
- Feature flag off / setting off: no embeddings are computed or stored;
  existing per-session behavior unchanged. Disabling the setting stops new
  processing but keeps the library; "Forget all voices" purges it.

## Testing

- Store: schema/CRUD/rolling-prune/cascade tests; matcher threshold+margin
  unit tests with synthetic vectors (TDD).
- Diarize: compile-gated; an `#[ignore]`d roundtrip test using a downloaded
  model (same-voice similarity > different-voice).
- Engine: enrollment-on-rename and match-then-label logic factored into
  testable functions where possible.
- Full gate incl. `--features voiceprints` clippy; manual pass: name someone
  in session 1 → record session 2 → they're auto-named; Discord call →
  participants enrolled; Forget → next session shows "Speaker N" again.
