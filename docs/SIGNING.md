# Code Signing Runbook

This document covers what to buy, how to configure the secrets, how the CI
steps activate, how to verify signed builds, and the private-repo updater
decision.

---

## 1. What to buy

### macOS — Apple Developer Program

You need an **Apple Developer Program** membership ($99/yr) at
<https://developer.apple.com/programs/enroll/>.

From it you obtain:

| Item | Where |
|---|---|
| **Developer ID Application** certificate | Xcode → Settings → Accounts → Manage Certificates, or developer.apple.com → Certificates. Export as `.p12` with a strong password. |
| **Signing identity string** | `security find-identity -v -p codesigning` → e.g. `Developer ID Application: Jacob Haig (TEAMID)` |
| **Team ID** | developer.apple.com → Membership Details |
| **App-specific password** | appleid.apple.com → Sign-In & Security → App-Specific Passwords (for `notarytool`) |

### Windows — Authenticode certificate

You need an **OV (Organization Validated)** or **EV (Extended Validation)**
code-signing certificate. EV is recommended — it establishes immediate
SmartScreen reputation. Typical vendors:

- **DigiCert** — <https://www.digicert.com/signing/code-signing-certificates>
- **Sectigo** — <https://sectigo.com/ssl-certificates-tls/code-signing>
- **GlobalSign** — <https://www.globalsign.com/en/code-signing-certificate>

EV certificates now require a hardware security key (FIDO2/USB token) for
private-key storage; the CI workflow uses a PFX export of an OV cert (which
still allows software-based export). If you use an EV cert, consult the
vendor's CI/CD guidance for token-based signing.

Deliver the certificate as a password-protected `.pfx` file.

---

## 2. GitHub secret names

### macOS (already in CI — secrets for existing `HAS_SIGNING` gate)

| Secret | Value |
|---|---|
| `MACOS_CERTIFICATE_P12` | `base64 -i cert.p12 \| pbcopy` — base64 of the `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | the `.p12` export password |
| `MACOS_SIGNING_IDENTITY` | `Developer ID Application: Jacob Haig (TEAMID)` |
| `APPLE_ID` | your Apple ID email |
| `APPLE_TEAM_ID` | your Team ID |
| `APPLE_APP_PASSWORD` | the app-specific password for `notarytool` |

### Windows (new — for the `HAS_WIN_SIGNING` gate added in Phase 43b)

| Secret | Value |
|---|---|
| `WINDOWS_CERT_PFX` | `base64 -i cert.pfx \| pbcopy` — base64 of the `.pfx` |
| `WINDOWS_CERT_PASSWORD` | the `.pfx` export password |

---

## 3. How activation works

Both platforms follow the same pattern — the signing steps are **dormant until
their secrets exist**:

- **macOS** — `HAS_SIGNING: ${{ secrets.MACOS_CERTIFICATE_P12 != '' && secrets.MACOS_SIGNING_IDENTITY != '' }}`.
  The import, sign, notarize, and staple steps all carry `if: env.HAS_SIGNING == 'true'`.
  Builds proceed unsigned until you add the secrets.

- **Windows** — `HAS_WIN_SIGNING: ${{ secrets.WINDOWS_CERT_PFX != '' }}`.
  The Authenticode step carries `if: env.HAS_WIN_SIGNING == 'true'`. No other
  change needed; just add the two secrets and the next release run signs everything.

Set secrets at: **Settings → Secrets and variables → Actions** in the GitHub
repository.

---

## 4. How to verify

### macOS

```bash
# After signing (before notarization):
codesign -dv --verbose=4 ZordGui.app

# After notarization and stapling:
spctl -a -vvv ZordGui.app
# Expected: "accepted" and "source=Notarized Developer ID"

xcrun stapler validate ZordGui.app
# Expected: "The validate action worked!"
```

### Windows

```powershell
# Locate signtool (adjust SDK version as needed):
$st = "C:\Program Files (x86)\Windows Kits\10\bin\10.0.22621.0\x64\signtool.exe"

# Verify a signed EXE:
& $st verify /pa /v Zord-<ver>-windows-x64-gui.exe
# Expected: "Successfully verified: ..."
```

---

## 5. Going private: the updater

**Context.** The `self-update` feature (GitHub/dev channel builds) checks the
public GitHub Releases API at launch and, on Windows, can download and install
the update in place. A **private repository breaks unauthenticated checks** —
the API returns 404 for releases on a private repo to callers without a token.

**No code change has been made.** This is a decision pending the repo going
private.

Two options:

### Option A — Public releases-only mirror repo (recommended)

Keep a separate **public** repository (e.g. `JacobHaig/zord-releases`) that
holds only the GitHub Releases (no source). Configure `release.yml` to publish
to that repo instead of the private one (the `softprops/action-gh-release`
action accepts a `repository` input). The updater's API call targets the
public mirror and never sees a 404.

Advantages: no token shipping, no changes to the updater code, the public
download URL is stable, SmartScreen and AV scanners can build reputation.

### Option B — Authenticated updater

Ship a read-only GitHub PAT (scoped to `public_repo` or a fine-grained
releases-read token) inside the app binary, and add `Authorization: Bearer
<token>` to the `self-update` HTTP call.

Disadvantages: a token in a shipped binary is a credential in the wild — even
a read-only releases token is a fingerprint; it can be extracted and abused
for rate-limit bypass or repo enumeration. Not recommended for public
distribution.

---

*This document was added in Phase 43b. Update it when certificates are
acquired and secrets are set.*
