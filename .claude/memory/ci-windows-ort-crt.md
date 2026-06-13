---
name: ci-windows-ort-crt
description: ort/onnxruntime CRT clash on Windows — RESOLVED in Phase 51 via ort load-dynamic; semantic/sentiment now ship on all platforms (the runtime DLL is bundled beside the exe)
metadata:
  node_type: memory
  type: reference
---

**Hard Windows constraint (v0.3.1 release, June 2026).** The `semantic`
feature (and `sentiment`) pull `ort` (ONNX Runtime) via fastembed/direct. ort's
prebuilt onnxruntime binary (`download-binaries`) is built against the
**dynamic** MSVC CRT (/MD). Zord's Windows build forces the **static** CRT
(/MT — `crt-static`, `CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded`,
`LLAMA_STATIC_CRT=1`) so sherpa-onnx (parakeet/diarization/voiceprints) and
llama.cpp/whisper.cpp link. /MT and /MD **cannot coexist in one binary** →
`libort_sys` fails with a wall of `LNK2019 unresolved external __imp_*` CRT
math symbols (`nearbyintf`, `rint`, `ilogb`, `scalbn`, `remainderf`, `fmax`,
`modf`, …). macOS has no /MT-/MD dichotomy, so it links fine.

**RESOLVED in Phase 51 via ort `load-dynamic`; `semantic` now ships on ALL
platforms.** ort's `load-dynamic` feature emits ZERO static-link directives, so
the /MT static-CRT exe links clean (no more `libort_sys` LNK2019 wall); the
ONNX Runtime shared lib is loaded at runtime via `libloading`. The C API is
CRT-boundary-safe (opaque handles, explicit allocator), so a /MT exe loading a
/MD DLL is safe. The release workflow ships the matching ONNX Runtime 1.24.2
lib beside every binary (`onnxruntime.dll` on Windows, `libonnxruntime.dylib`
on macOS) and `main.rs::setup_ort_dylib()` points `ORT_DYLIB_PATH` at it. The
Windows `semantic` strip in the Resolve-channel step was REMOVED (Phase 51b);
FEATURES is unified again. `sentiment` (also ort-based) is now safe on Windows
too. Windows DLLs reach the NSIS installer via `Dioxus.toml [bundle] resources`
(staged before `dx bundle`); the updater refreshes the DLL on self-update
(Phase 51c, asset `onnxruntime-windows-x64.dll`).

**Historical context (pre-Phase-51, v0.3.1 stopgap):** the macOS job shipped
the full `FEATURES`; the Windows Resolve-channel step stripped `semantic`
(`F="${F//,semantic/}"`...) because ort's prebuilt `download-binaries` DLL is
/MD and clashed with the /MT static CRT — so semantic search was macOS-only.

Related: [[ci-windows-crt]] (the original sherpa /MT vs llama /MD clash this
extends), [[feature-build-deps]], [[productionization]].
