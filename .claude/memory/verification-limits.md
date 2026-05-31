---
name: verification-limits
description: What can't be exercised in this build env (live audio, GPU LLM/ASR, TCC, Windows) — compile-verify + flag as user steps
metadata:
  node_type: memory
  type: feedback
---

This build environment is headless macOS — several things compile/launch but
cannot be runtime-verified here, and must be flagged as **user steps**:

- **Live dual-channel recording** — needs real mic + system audio + macOS Screen
  Recording (TCC) grant. The bare binary attributes permissions to the terminal;
  a signed `.app` gets its own identity.
- **Parakeet / summaries (LLM) inference** — need large model downloads + GPU;
  only compile + link are verified.
- **Encryption** — verified via `cargo test -p zord-store --features encryption`
  (roundtrip), and a full CLI encrypt→read→decrypt cycle works headlessly.
- **Windows runtime** — backend is implemented + type-checks
  (`cargo check --target x86_64-pc-windows-msvc`) + builds in CI, but never run.
- **Code-signing / notarization** — needs the user's Apple Developer credentials
  (documented in `docs/RELEASE.md`).

**How to apply:** state clearly "builds + launches; runtime is a user step" —
never claim a feature "works" end-to-end when only compilation was verified.
Related: [[commit-and-workflow]], [[feature-build-deps]].
