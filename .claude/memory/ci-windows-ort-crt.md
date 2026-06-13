---
name: ci-windows-ort-crt
description: ort/onnxruntime (semantic, sentiment) cannot ship in the Windows release — its prebuilt needs dynamic CRT, clashing with the static CRT sherpa/llama require
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

**Resolution in release.yml:** the macOS job ships the full `FEATURES` incl.
`semantic`; the **Windows** Resolve-channel step strips `semantic`
(`F="${F//,semantic/}"`...). So **semantic search is macOS-only** in releases
until ort can be built against the static CRT (would need building onnxruntime
from source with /MT, or ort's `load-dynamic` strategy shipping the DLL — both
non-trivial; revisit if Windows semantic search is wanted). `sentiment` is also
ort-based and would hit the same wall — keep it off the Windows build too if
ever added to release FEATURES.

Related: [[ci-windows-crt]] (the original sherpa /MT vs llama /MD clash this
extends), [[feature-build-deps]], [[productionization]].
