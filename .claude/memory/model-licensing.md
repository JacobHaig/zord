---
name: model-licensing
description: Zord ships commercially — every downloadable model must be commercially licensed; the non-commercial ones (Qwen2.5-3B, Reverb v1/v2) were removed
metadata:
  node_type: memory
  type: feedback
---

Zord is intended to be **sold**, so every model offered in-app must be licensed
for **commercial use**. Verify the LICENSE before adding any model — benchmark
rank is irrelevant if the license isn't commercial.

**Removed for being NON-COMMERCIAL (do NOT re-add):**
- **Qwen2.5-3B-Instruct** — "Qwen RESEARCH License … FOR NON-COMMERCIAL PURPOSES
  ONLY." Gotcha: its 1.5B/7B/32B siblings ARE Apache-2.0; Qwen carved out only
  3B (and 72B) as research-licensed. It was the old default → migrated to
  `gemma-2-2b-it` (`REMOVED_SUMMARY_MODELS` in zord-config resets a saved id).
- **Reverb v1/v2** diarization segmentation — Rev "Non-Production License" (bars
  commercial activity, paid or free). Only `pyannote-3.0` (MIT) remains;
  `SegmentationModel::parse_or_default` falls a saved "reverb-v*" back to pyannote.

**Commercial-OK tiers (what's allowed to ship):**
- Permissive, no strings: MIT (Whisper, pyannote-3.0, Phi-3.5), Apache-2.0
  (Qwen2.5 1.5/7/32B, Qwen3 8/14B, WeSpeaker, 3D-Speaker).
- Commercial + attribution: CC-BY-4.0 (Parakeet, TitaNet) — needs a credit line.
- Commercial + passthrough terms/AUP: Gemma 2/3 (Gemma Terms), Llama 3.2
  ("Built with Llama", <700M MAU).

Full audit of record: `docs/model-licensing.md`. A commercial release should
ship a NOTICES file (CC-BY credits + Apache NOTICE + Gemma/Llama terms).
Related: [[summary-model-catalog]], [[asr-diarization-model-watchlist]],
[[dep-upgrade-notes]].
