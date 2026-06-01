# Memory Index

- [Architecture](architecture.md) — workspace crates, `TranscribeBackend` trait, GUI engine threading model
- [Feature flags](feature-flags.md) — parakeet/encryption/summaries are opt-in Cargo features; default build is lean
- [Capture design](capture-design.md) — dual-channel Me/Others; per-OS loopback; ML diarization layered within Others
- [Diarization design](diarization-design.md) — Phase 16 per-speaker diarization: offline-first, optional live, sherpa-onnx
- [macOS build](macos-build.md) — deployment target 13, Swift lib path, build.rs links libclang_rt.osx
- [CI macOS Xcode 26](ci-macos-xcode26.md) — release macOS job needs macos-15 (Xcode 26 / macOS 26 SDK) for the screencapturekit Swift bridge
- [CI Windows CRT](ci-windows-crt.md) — Windows release must force static MSVC CRT (sherpa /MT vs llama/whisper /MD link clash)
- [Data locations](data-locations.md) — app-data dir layout (config/db/models/audio/exports) per OS
- [dx bundling gotchas](dx-bundling-gotchas.md) — info_plist REPLACES; app is ZordGui.app; build with --package
- [Feature build deps](feature-build-deps.md) — perl/OpenSSL, ONNX, llama.cpp toolchain per feature
- [Commit & workflow](commit-and-workflow.md) — NO co-author trailers; phased commits; verify before claiming
- [Verification limits](verification-limits.md) — what can't run headlessly (live audio, LLM/ASR, Windows, signing)
- [Canonical docs](docs-canonical.md) — docs/PLAN.md is the phase tracker; README + RELEASE roles
