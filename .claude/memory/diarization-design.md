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
  segmentation + speaker-embedding + `FastClusteringConfig{num_clusters:-1}`);
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
  otherwise the temp Others WAV is deleted after the first pass. apply_diarization
  reads the current `diarize_embedding_model` from settings each run, so a model
  swap takes effect on the next "Identify speakers".
- Storage: nullable `segments.speaker` + per-session `speaker_names` table
  (rename Speaker 1 → Alex). `Segment::speaker_label(&names)` renders Me /
  Speaker N / custom; flows into transcript (per-speaker colors), search, and
  MD/SRT/JSON exports (`render` now takes a `names` map).

**Runtime caveat:** model-download URLs + ONNX/GPU inference are NOT verified
headlessly — first-run download + accuracy need an on-device check. See
[[verification-limits]], [[feature-flags]], [[data-locations]], [[architecture]].
