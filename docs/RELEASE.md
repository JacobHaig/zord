# Releasing Zord (macOS)

Zord ships as a native macOS `.app` (Apple Silicon, macOS 13+). Local
development never needs any of this — `cargo run -p zord-gui` is enough. This
doc covers producing a **distributable, signed, notarized** build.

## 1. Build a bundle locally

```bash
cargo install dioxus-cli --version 0.7.9 --locked   # once
cd crates/zord-gui
dx bundle --release --platform desktop
```

This produces `Zord.app` (and, depending on dx config, a `.dmg`) under
`target/dx/zord-gui/bundle/`. The bundle's `Info.plist` carries
`NSMicrophoneUsageDescription`; entitlements come from `entitlements.plist`.

> **Unsigned bundles** run locally but trip Gatekeeper on other machines
> ("unidentified developer"). For distribution you must sign + notarize.

## 2. What requires *your* Apple Developer account

Signing and notarization need credentials only you have. You need an active
**Apple Developer Program** membership ($99/yr) and:

| Item | Where to get it |
|---|---|
| **Developer ID Application** certificate (`.p12`) | Xcode → Settings → Accounts → Manage Certificates, or developer.apple.com → Certificates. Export as `.p12` with a password. |
| **Signing identity** string | `security find-identity -v -p codesigning` → e.g. `Developer ID Application: Your Name (TEAMID)` |
| **Team ID** | developer.apple.com → Membership |
| **App-specific password** | appleid.apple.com → Sign-In & Security → App-Specific Passwords (for `notarytool`) |

## 3. Sign + notarize manually

```bash
APP="target/dx/zord-gui/bundle/macos/Zord.app"

codesign --deep --force --options runtime \
  --entitlements crates/zord-gui/entitlements.plist \
  --sign "Developer ID Application: Your Name (TEAMID)" "$APP"

ditto -c -k --keepParent "$APP" Zord.zip
xcrun notarytool submit Zord.zip \
  --apple-id "you@example.com" --team-id "TEAMID" \
  --password "app-specific-password" --wait
xcrun stapler staple "$APP"
```

Verify: `spctl -a -vvv "$APP"` should report `accepted / source=Notarized`.

## 4. Automated releases (GitHub Actions)

`.github/workflows/release.yml` builds the bundle on every `v*` tag (and on
manual dispatch). The sign + notarize steps run **only if** these repository
secrets are present — otherwise CI still uploads an unsigned artifact:

| Secret | Value |
|---|---|
| `MACOS_CERTIFICATE_P12` | base64 of your `.p12` (`base64 -i cert.p12 \| pbcopy`) |
| `MACOS_CERTIFICATE_PASSWORD` | the `.p12` export password |
| `MACOS_SIGNING_IDENTITY` | `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | your Apple ID email |
| `APPLE_TEAM_ID` | your Team ID |
| `APPLE_APP_PASSWORD` | the app-specific password |

Cut a release:

```bash
git tag v0.1.0
git push origin v0.1.0
```

## 5. First-run permissions (what users see)

- **Microphone** — prompted automatically on first record (uses the
  `NSMicrophoneUsageDescription` string).
- **Screen Recording** — required for the "Others" / system-audio channel.
  macOS shows its own prompt; the user enables Zord under **System Settings →
  Privacy & Security → Screen Recording** and relaunches. There is no
  Info.plist key for this — it is entirely TCC-managed.

## Windows

The Windows backend (WASAPI loopback + cpal mic) is implemented and the
`windows` job in `.github/workflows/release.yml` builds + bundles a `.msi` on a
`windows-latest` runner for every tag. Runtime testing on a real Windows machine
is still pending (no Windows host in the dev environment). Authenticode signing
is not yet wired up — add a signing step analogous to the macOS one when you
have a code-signing certificate.

## Not yet covered

- **macOS notarization automation** — wired in CI but gated on your Apple
  Developer secrets (see §4).
- **SQLCipher** at-rest encryption — implemented (PLAN Phase 11) behind
  `--features encryption`; the default release build omits it (avoids the
  OpenSSL/perl build).
- **App icon** — planned (PLAN Phase 12): add an icon set + `Dioxus.toml`
  `[bundle] icon` before a public release.
- **NVIDIA Parakeet** — build with `--features parakeet` (PLAN Phase 10); the
  release workflow currently builds the default (Whisper-only) configuration.
