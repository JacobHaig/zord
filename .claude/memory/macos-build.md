---
name: macos-build
description: macOS build needs a 13.0 deployment target, a Swift lib search path, and build.rs linking libclang_rt.osx
metadata:
  node_type: memory
  type: project
---

Building on macOS requires non-obvious linker setup (already wired; don't remove):

- `.cargo/config.toml` sets `MACOSX_DEPLOYMENT_TARGET = "13.0"` (ScreenCaptureKit's
  Swift bridge needs macOS 13) and adds a rustflag `-L` to the Command Line
  Tools Swift lib dir (`/Library/Developer/CommandLineTools/usr/lib/swift/macosx`)
  so the Swift back-compat shims resolve on a CLT-only (no full Xcode) machine.
- `crates/zord-gui/build.rs` links `libclang_rt.osx` (resolved via
  `clang -print-resource-dir`) so the explicit-`--target` release link (what
  `dx bundle` does) finds `___isPlatformVersionAtLeast`, used by ggml-metal's
  `@available`.

**Why:** without these, `cargo build` warns/fails at link, and `dx bundle`
fails with undefined Swift/clang-rt symbols.

**How to apply:** macOS 13 is the minimum target (runs on all Apple Silicon).
Whisper uses Metal; build needs CMake + a C/C++ toolchain. Related:
[[feature-build-deps]], [[dx-bundling-gotchas]].
