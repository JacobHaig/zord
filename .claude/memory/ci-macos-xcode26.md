---
name: ci-macos-xcode26
description: macOS release CI must run on macos-15 (Xcode 26 / macOS 26 SDK) or the screencapturekit Swift bridge fails to compile
metadata:
  node_type: memory
  type: reference
---

**Symptom.** The macOS release build (`dx bundle … zord-gui`) fails during
compilation in the `apple-metal` crate's build script with a generic
`Swift build failed with exit code: Some(1)` panic. `dx` (and `cargo`) hide the
underlying `swiftc` output, so it looks opaque.

**Real cause.** `apple-metal` is a transitive Swift bridge pulled in by
`screencapturekit` 6.1 (our system-audio capture) — so it's in **every** build,
not tied to any Cargo feature. `apple-metal` 0.8.7's Swift sources reference
**macOS 26 SDK** symbols (e.g. `MTLSamplerReductionMode`,
`MTLSamplerDescriptor.reductionMode`/`.lodBias`) inside `if #available(macOS 26.0, *)`
blocks. `#available` is a *runtime* gate, but those symbols must still exist at
**compile time**, so the crate only builds with the **macOS 26 SDK (Xcode 26)**.
The real error is `error: cannot find 'MTLSamplerReductionMode' in scope`.

It builds fine locally because the dev machine has Xcode 26 (Swift 6.3.x). It
failed in CI because the **`macos-14` runner only has Xcode 16.x (macOS 15 SDK)**.

**Fix.** Run the macOS release job on **`macos-15`**, which ships Xcode
26.0–26.3, and select it with `maxim-lobanov/setup-xcode@v1` →
`xcode-version: latest-stable` (resolves to Xcode 26 there). Building against the
macOS 26 SDK with `MACOSX_DEPLOYMENT_TARGET=13.0` is correct — the `#available`
guards keep the binary runnable on macOS 13+. See `.github/workflows/release.yml`.

**Red herring.** The hardcoded `swift build --triple arm64-apple-macosx` in
apple-metal's build.rs is NOT the problem (it builds fine with the right SDK).
Don't chase the triple or downgrade Xcode — older Xcode lacks the macOS 26 SDK
and fails harder.

**If it recurs / to diagnose:** `dx` swallows the swiftc error — surface it with
`cargo build -p apple-metal -vv` on the runner, and probe images with
`ls /Applications | grep -i Xcode`. If a future `apple-metal`/`screencapturekit`
bump needs an even newer SDK, move the runner to whichever image ships it. The
two OS jobs are independent, so a macOS failure never blocks the Windows release.

Related: [[macos-build]], [[feature-flags]], [[verification-limits]].
