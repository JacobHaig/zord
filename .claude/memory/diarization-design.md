---
name: diarization-design
description: Phase 16 per-speaker diarization — offline-first architecture, why, and how it's wired
metadata:
  node_type: memory
  type: project
---

Phase 16 adds **per-speaker diarization within the "Others" channel** (Others →
Speaker 1/2/3). Channel separation already gives Me-vs-Others; this layers
identity *inside* Others. Feature-gated `diarization` (crate `zord-diarize`,
internal feature `sherpa`), reusing the already-resolved `sherpa-onnx` 1.13 —
**no new heavy dep**.

**Decision: offline-first.** Diarization = embed each speech chunk + **cluster**
embeddings into speakers. Clustering is inherently *global* (must see all
speakers; K unknown until the end), so the accurate, source-of-truth pass is
**offline**, run after recording — which also keeps it off the hot ASR path
(hardware-friendly) and matches the project's "accurate, not real-time" ethos.

**Why not live-only:** online/incremental clustering can't revisit early
decisions, mislabels before centroids stabilize, and adds embedding work during
capture (frame-drop risk). So live is an *optional, provisional* overlay
(`diarize_live`, default OFF) that's always **replaced by the offline pass at
stop**. User asked for both triggers + a live toggle.

**How it's wired:**
- `zord-diarize`: `Diarizer` wraps `OfflineSpeakerDiarization` (pyannote
  segmentation + speaker-embedding + `FastClusteringConfig{num_clusters,threshold:0.5}`).
  `num_clusters:-1` = auto, but auto OVER-SPLITS noisy meeting mixes (a 10-person
  call came out as ~80 "speakers", because the Others channel is the call's
  compressed/echo-cancelled output). The `diarize_num_speakers` setting (0=auto)
  pins a fixed count via `Diarizer::load_with_speakers` — the deterministic fix;
  a bigger embedding model alone does NOT reliably fix the count.
  `LiveLabeler` wraps `SpeakerEmbeddingExtractor`+`SpeakerEmbeddingManager`.
  Two ONNX models (pyannote seg ~6MB + embedding: TitaNet small/large or
  WeSpeaker CAM++) managed via the same download/select/delete model UI
  (`kind=="diarization"` in the engine catalog).
- Audio source: the "Others" 16 kHz mono track is written to WAV during
  recording even when keep-audio is off (temp `<id>.others.wav`, deleted after);
  `zord_audio::read_wav_mono_f32` reads it back. Diarizer output `{start,end,
  speaker}` is mapped onto stored Others segments by **max temporal overlap**.
- Triggers: auto at stop (control thread) + on-demand `DbCmd::Diarize` (spawns a
  worker so the db thread stays responsive) + `zord diarize <session>` CLI.
  On-demand re-reads `session.audio_path` + `.others.wav`, so **re-diarizing
  (e.g. swapping to a bigger model) only works if that WAV was retained.** The
  `diarize_keep_audio` setting (Phase 19) keeps just the Others track even when
  Keep-audio is off, precisely so users can re-run with a different model later;
  otherwise the temp Others WAV is deleted after the first pass. **It now defaults
  ON** (the kept Others WAV lives in the audio dir and is pruned by
  `auto_delete_days`) — before this, the default-off made manual "Identify
  speakers" fail on past recordings (audio already deleted), the #1 cause of the
  "works right after recording, not later" intermittency. apply_diarization
  reads the current `diarize_embedding_model` from settings each run, so a model
  swap takes effect on the next "Identify speakers".
- Storage: nullable `segments.speaker` + per-session `speaker_names` table
  (rename Speaker 1 → Alex). `Segment::speaker_label(&names)` renders Me /
  Speaker N / custom; flows into transcript (per-speaker colors), search, and
  MD/SRT/JSON exports (`render` now takes a `names` map).

**Reliability (fixed):** the on-demand worker is `catch_unwind`-wrapped and ALWAYS
emits a terminal `Event::Speakers` so the GUI "Identifying…" busy flag clears on
any outcome (was: failures emitted only a 5s toast → button stuck). A no-result
run is **non-destructive**: assignments are computed first and `clear_speakers`
(which also drops custom names) runs only when speakers were actually matched —
empty diarizer output / empty mapping leaves existing labels intact with an
actionable notice. `segmentation_present` now requires a non-empty file so a
truncated model.onnx re-downloads. Diarizer can return `Ok` with **zero/collapsed
spans** (short/quiet/single-speaker audio; auto-cluster `num_clusters=-1` +
`threshold` 0.5; sherpa `min_duration_on=0.3/off=0.5`) — surfaced as a clear notice
suggesting a lower threshold or a pinned speaker count, not a silent no-op.

**Runtime caveat:** model-download URLs + ONNX/GPU inference are NOT verified
headlessly — first-run download + accuracy need an on-device check. See
[[verification-limits]], [[feature-flags]], [[data-locations]], [[architecture]].
