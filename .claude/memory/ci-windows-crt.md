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

**CRITICAL follow-on (v0.2.9): `CMAKE_MSVC_RUNTIME_LIBRARY` is NOT enough for
llama.** `llama-cpp-sys-2` (0.1.146) build.rs reads `LLAMA_STATIC_CRT` (build.rs:221)
and unconditionally calls `config.static_crt(static_crt)` (build.rs:650); when the
env var is unset it defaults **false → `/MD`**, which *overrides* the
`CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded` we set. So the link SUCCEEDS (crt-static
masks it) but llama/ggml end up the lone **dynamic-CRT** component while Rust +
whisper + sherpa are static. At **runtime**, a buffer allocated in one CRT heap and
freed across the other corrupts the heap → the windowed process (`windows_subsystem
= "windows"`, no console) **vanishes instantly on every Summarize/Compress**, with
no Rust panic and nothing in the (buffered) log. Worked on macOS only because it
offloads to Metal and never runs the pure-CPU ggml path.

**Fix:** set `LLAMA_STATIC_CRT = "1"` — in the Windows release job env
(release.yml, next to `CMAKE_MSVC_RUNTIME_LIBRARY`) **and** in `.cargo/config.toml`
`[env]` (so local Windows builds match; it's an MSVC-only knob, a no-op on
mac/clang — verified `cargo build -p zord-summarize --features llama` still builds
on macOS). Diagnosed via a 5-agent workflow that also ruled out (with numbers) OOM
(1.5B @ 8K ≈ 1.6–2.5 GB, fits) and AVX/ISA faults (build.rs forces
`GGML_NATIVE=OFF`; windows-msvc default features are SSE2-only, so no AVX baked in).

**If a new C++/-sys dep is added on Windows,** make sure it also honors the
static CRT (most `cc`/`cmake`-based crates pick up `crt-static` /
`CMAKE_MSVC_RUNTIME_LIBRARY`; **llama-cpp-sys-2 needs `LLAMA_STATIC_CRT=1`**);
otherwise either LNK2038 (link time) or a silent dual-CRT heap crash (runtime).

Related: [[ci-macos-xcode26]], [[feature-flags]], [[verification-limits]].
