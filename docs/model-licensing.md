# Model licensing

Zord downloads ML models on first use. Because Zord is intended to ship as a
**commercial product**, every model offered in-app must be licensed for
commercial use. This is the audit of record (verified against each model's
`LICENSE` / source repo, June 2026).

## Offered models — all commercially licensed

| Model | License | Commercial | Obligation |
|-------|---------|------------|------------|
| Whisper (tiny → large-v3, turbo, turbo-q5) | MIT | ✅ | — |
| Parakeet TDT 0.6B v3 | CC-BY-4.0 | ✅ | **attribution** |
| Qwen2.5 1.5B / 7B / 32B Instruct | Apache-2.0 | ✅ | NOTICE |
| Qwen3 8B / 14B | Apache-2.0 | ✅ | NOTICE |
| Gemma 2 2B / Gemma 3 4B | Gemma Terms | ✅ | **pass through Gemma Terms + Prohibited Use Policy** |
| Llama 3.2 3B (Ollama) | Llama 3.2 Community | ✅ | **"Built with Llama" notice; <700M MAU** |
| Phi-3.5 mini (Ollama) | MIT | ✅ | — |
| pyannote segmentation-3.0 | MIT | ✅ | — |
| TitaNet small / large (embeddings) | CC-BY-4.0 | ✅ | **attribution** |
| WeSpeaker CAM++ / ResNet34 (embeddings) | Apache-2.0 | ✅ | NOTICE |
| 3D-Speaker CAM++ (embeddings) | Apache-2.0 | ✅ | NOTICE |

## Removed — NON-COMMERCIAL (do not re-add)

| Model | License | Why |
|-------|---------|-----|
| **Qwen2.5 3B Instruct** | Qwen RESEARCH License | "FOR NON-COMMERCIAL PURPOSES ONLY" (research/evaluation). Its siblings (1.5B/7B/32B) are Apache — only 3B (and 72B) are research-licensed. Was the **old default**; removed + migrated to Gemma 2 2B. |
| **Reverb v1 / v2** (diarization segmentation) | Rev Model Non-Production License | Bars commercial activity, incl. paid or free distribution / hosted service. Replaced by pyannote-3.0 (MIT) fallback. |

## Notes for shipping a commercial build

- **Attribution / NOTICE file.** A release should bundle a `NOTICES` file with:
  the CC-BY-4.0 credits (Parakeet, TitaNet), the Apache-2.0 NOTICE entries, the
  Gemma Terms + Prohibited Use Policy, and (if Llama 3.2 is offered) the
  "Built with Llama" attribution.
- **Gemma / Llama** are commercial-OK but carry *passthrough* terms + an
  acceptable-use policy — not blockers, but the terms must travel with the app.
- **Adding a model?** Verify the LICENSE before adding it to the catalog —
  benchmark rank is irrelevant if the license isn't commercial. See
  `.claude/memory/summary-model-catalog.md` and `dep-upgrade-notes.md`.
