---
name: asr-diarization-model-watchlist
description: Researched verdicts (June 2026) on ASR/diarization model upgrades — what beats what, what fits our runtime, and the triggers to revisit
metadata:
  node_type: memory
  type: reference
---

Research findings (June 2026) on transcription + diarization model upgrades.
Constraint that decides everything: models must run **locally on CPU** via our
runtimes (whisper.cpp, sherpa-onnx, llama.cpp) — PyTorch/NeMo-only = no-go.

**Transcription: Parakeet TDT 0.6B v3 is the practical ceiling.**
- Parakeet v3: 6.32% WER Open ASR English avg, RTFx ~3300 — beats
  whisper-large-v3 (~7.4%) and even Canary-1B-v2 (7.15%) on English
  (arxiv 2509.14128). Canary only wins multilingual (8.1 vs 9.7 over 24 langs).
- Above it but un-runnable for us (LLM-decoder archs, no ONNX/CPU path):
  Canary-Qwen 2.5B (5.63%), Granite Speech 8B (5.85%), Qwen3-ASR-1.7B
  (Jan 2026, Apache 2.0, 52 langs — strongest open model but PyTorch/MLX only,
  and on English it's in Parakeet's band, not above it).
- **Revisit triggers:** sherpa-onnx ships `canary-1b-v2` or `qwen3-asr`
  exports; or llama.cpp grows an audio path for Qwen3-ASR (we already ship
  llama.cpp for summaries).

**Diarization: segmentation model is the lever, not embeddings.**
- Reverb v1/v2 (Rev's pyannote fine-tunes, ~16%/~22% better WDER) are in
  sherpa-onnx's zoo and load through the same pyannote config — added as
  selectable `SegmentationModel` June 2026. Non-commercial license — labeled
  in the UI.
- NVIDIA Sortformer (the famous accurate one): 4-speaker hard cap, ~12-min
  clip cap, CC-BY-NC, streaming-v2 ONNX export broken upstream — rejected.
- pyannote community-1 (Sep 2025, the best open model; fixes
  speaker-counting/over-split): PyTorch-first but its segmentation exports
  cleanly to ONNX (pyannote-audio discussion #1929) — **next candidate**; we'd
  self-host the converted .onnx.

**Why:** repeated "should we add model X?" questions; the answer usually turns
on runtime compatibility, not benchmark rank. Related: [[diarization-design]].
