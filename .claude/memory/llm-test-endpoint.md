---
name: llm-test-endpoint
description: Local LM Studio OpenAI-compatible endpoint for testing LLM passes during dev
metadata:
  node_type: memory
  type: reference
---

The user runs an LM Studio server at **http://127.0.0.1:1234** (OpenAI-compatible:
`/v1/models`, `/v1/chat/completions`). Use it to validate prompts/structured-output
for the LLM-driven features (Phase 26 extract/route/merge, summaries, compression)
against a real model instead of guessing or wrestling a local GGUF in tests.

Quick check: `curl -s http://127.0.0.1:1234/v1/models`. A reusable end-to-end test
already exists: `cargo test -p zord-summarize --features remote -- --ignored real_server`
(honors `ZORD_TEST_LLM_URL` / `ZORD_TEST_LLM_MODEL`).

**Why:** the user offered it to guarantee correct results while building LLM features.
**Notes:** models load on demand — big ones may fail to JIT-load (e.g. `google/gemma-4-12b`
errored); `nvidia/nemotron-3-nano-4b` loaded + responded reliably. Pick a model that's
actually loadable when testing. Related: [[asr-diarization-model-watchlist]].
