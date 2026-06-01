---
name: ci-windows-crt
description: Windows release builds must force the static MSVC CRT or sherpa-onnx (/MT) collides with llama.cpp/whisper (/MD) at link time
metadata:
  node_type: memory
  type: reference
---

**Symptom.** Linking the Windows GUI/CLI with both ONNX (diarization/parakeet)
*and* llama.cpp (summaries) features fails with `LNK2038: 'RuntimeLibrary'
mismatch — MT_StaticRelease doesn't match MD_DynamicRelease` plus a flood of
`LNK2005` duplicate `std::` symbols.

**Cause.** `sherpa-onnx`'s prebuilt Windows libs are built against the **static**
MSVC runtime (`/MT`), while Rust and the cmake-built C++ deps (`llama-cpp-sys-2`,
`whisper-rs-sys`) default to the **dynamic** runtime (`/MD`). You cannot mix `/MT`
and `/MD` in one binary. macOS doesn't hit this (single toolchain, no prebuilt
/MT libs).

**Fix (in repo).** Force the whole binary onto the static CRT:
- `.cargo/config.toml` → `[target.x86_64-pc-windows-msvc] rustflags =
  ["-C", "target-feature=+crt-static"]` (Rust + its std).
- Windows release job env → `CMAKE_MSVC_RUNTIME_LIBRARY: MultiThreaded` and
  `CMAKE_POLICY_DEFAULT_CMP0091: NEW` (so the cmake deps build `/MT` too).

Now everything is `/MT` and links. Validated on the v0.2.1 release.

**If a new C++/-sys dep is added on Windows,** make sure it also honors the
static CRT (most `cc`/`cmake`-based crates pick up `crt-static` /
`CMAKE_MSVC_RUNTIME_LIBRARY`); otherwise the same LNK2038 returns.

Related: [[ci-macos-xcode26]], [[feature-flags]], [[verification-limits]].
