---
name: feature-build-deps
description: Extra toolchain/runtime each Cargo feature pulls in, and how it's verified
metadata:
  node_type: memory
  type: reference
---

Toolchain needed per feature (default build needs only Rust + CMake + a C/C++ compiler):

- `parakeet` → `sherpa-onnx` build script downloads prebuilt ONNX libs; also
  pulls `tar` + `bzip2` to unpack model archives. Builds in ~30s on macOS.
- `encryption` → SQLCipher + **vendored OpenSSL**, which needs **perl** at build
  time. ~48s build. Roundtrip tested via `cargo test -p zord-store --features encryption`.
- `summaries` → `llama-cpp-2` compiles llama.cpp (Metal on macOS) via CMake; ~69s.
  Downloads a ~1–4.7 GB GGUF at runtime.

Verify a feature with e.g. `cargo build -p zord-gui --features <name>` and
`cargo build -p zord-app --features <name>`. The Windows CI job builds the
default (Whisper-only) config. Related: [[feature-flags]], [[verification-limits]].
