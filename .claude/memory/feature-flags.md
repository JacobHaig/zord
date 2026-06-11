---
name: feature-flags
description: Optional heavy engines are Cargo features; the default build stays lean and is the only one verified headlessly
metadata:
  node_type: memory
  type: project
---

Heavy/optional capabilities are gated behind Cargo features so the default
build (and Windows CI) stays lean and fast:

- `parakeet` → NVIDIA Parakeet via `sherpa-onnx` (in `zord-transcribe`).
- `encryption` → SQLCipher at-rest DB encryption (`rusqlite/bundled-sqlcipher-vendored-openssl`).
- `llm-local` → AI features (summaries/compress/overview/chat/titles) with the
  built-in LLM via `llama-cpp-2` (crate feature `llama` in `zord-summarize`).
- `llm-remote` → same AI features against a user-provided OpenAI-compatible
  server (crate feature `remote` — pure HTTP, no llama.cpp toolchain). The two
  compose; with only one compiled, it is used regardless of the settings value.
  (Renamed from `summaries` June 2026 — clean break, no alias.)
- `diarization` → per-speaker diarization via `sherpa-onnx` (in `zord-diarize`;
  internal crate feature is `sherpa`, like summarize's `llama`). Reuses the same
  sherpa-onnx version as `parakeet`; cargo unifies the sys crate so both can be
  enabled together. See [[diarization-design]].
- `voiceprints` → cross-session speaker identity (requires `diarization`): per-cluster
  embeddings persisted to `zord-store`; cosine matcher auto-names recognized speakers
  post-diarization; implicit enrollment via rename + Discord ground-truth tracks;
  Speakers rail view + consent dialog + per-person Forget. Build-time kill-switch;
  runtime toggle `voiceprints_enabled` (default off) is the second gate. See [[voiceprints]].

Each consuming binary (`zord-app`, `zord-gui`) has a **passthrough** feature of
the same name that enables the underlying crate feature. Default build never
compiles these deps.

**Why:** the default build must compile fast and not require extra toolchains
(perl/OpenSSL, ONNX, llama.cpp). See [[feature-build-deps]].

**How to apply:** new optional engine = optional dep + crate feature, with the
code gated `#[cfg(feature = "…")]` and a non-feature stub that bails with
"rebuild with --features …". Add passthrough features on the binaries. Always
verify both the default build AND the feature build. Related: [[architecture]],
[[verification-limits]].
