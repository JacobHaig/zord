---
name: voiceprints
description: Phase 38 persistent cross-session speaker identity — architecture, kill-switch, legal, and storage nuances
metadata:
  node_type: memory
  type: project
---

**Architecture.** Three new SQLite tables: `voiceprints` (one row per known
person, linked from `speaker_names.voiceprint_id`), `voiceprint_samples`
(rolling 8 newest embeddings per person — old ones are pruned on upsert),
and `session_speaker_embeddings` (per-cluster embeddings written after every
diarization pass, regardless of the runtime toggle). Matching is plain cosine
similarity via `best_voiceprint_match`: standard threshold 0.72, strict 0.78,
relaxed 0.66; a 0.05 runner-up margin prevents ambiguous matches; cluster must
contribute ≥3 s of speech (skipped in `embed_clusters`); embeddings are
L2-normalized before storage; samples are capped at 30 s per cluster to bound
compute. All of this lives in `zord-diarize::SpeakerEmbedder` (embed side) and
`zord-store` (persistence + matcher). **Gotcha:** voiceprint comparison uses
the stored canonical model name from settings — two runs with different model
names (e.g. a renamed ONNX file vs. its hash-name) will never match. The
`voiceprint_model` column on `voiceprints` rows is purely informational right
now; cross-model matching is not attempted.

**Enrollment paths (all implicit):**
1. User renames a speaker in the transcript view → engine enrolls/updates
   their voiceprint immediately.
2. Discord session stops → engine auto-enrolls every ground-truth participant
   (real Discord names, already known, no prompt).
3. Engine auto-match notice ("Recognized Alex.") is shown post-diarization
   when `voiceprints_enabled` is true and a cluster scores above threshold.

**Kill-switch and consent.** The `voiceprints` Cargo feature (requires
`diarization`) is a build-time kill-switch — the Speakers view, consent
dialog, settings block, engine enrollment, and auto-match are all `#[cfg]`
gated. At runtime, `voiceprints_enabled` (default false) is the second gate;
flipping it on fires a one-time consent dialog (`voiceprints_consented_at`
timestamp). Forget-this-voice (per person) and Forget-all-voices (Settings →
Speakers) delete from `voiceprints` + `voiceprint_samples`. Legal posture:
`docs/voiceprints-legal.md`.

**Storage subtlety.** `session_speaker_embeddings` rows are written after
every diarization regardless of `voiceprints_enabled` — so the library is
populated from day-one recordings the moment the user opts in, without needing
to re-diarize. Matching and enrollment are gated on the runtime toggle; raw
persistence is not.

Related: [[diarization-design]], [[feature-flags]], [[data-locations]].
