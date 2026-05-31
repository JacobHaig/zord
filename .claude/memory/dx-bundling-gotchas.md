---
name: dx-bundling-gotchas
description: dx bundle quirks — info_plist_path REPLACES the plist, app is named ZordGui.app, use --package from root
metadata:
  node_type: memory
  type: feedback
---

Packaging the GUI uses `dx bundle` (dioxus-cli 0.7.9). Gotchas learned the hard way:

- `Dioxus.toml` `[bundle.macos] info_plist_path` **REPLACES** the generated
  Info.plist — it does not merge. So `crates/zord-gui/Info.plist` must contain
  ALL required keys (`CFBundleIdentifier` = `io.zord.zord`, `CFBundleExecutable`
  = `zord-gui`, version, `CFBundleIconFile` = `ZordGui`, mic usage string, etc.).
  Missing `CFBundleIdentifier` breaks TCC permission persistence.
- The produced bundle is **`ZordGui.app`** (dx derives the product name from the
  package, not `[application] name`). It *displays* as "Zord" via
  `CFBundleName`/`CFBundleDisplayName`. The bundled icns is `ZordGui.icns`.
- Build/run from the repo root with `-p/--package zord-gui` (e.g.
  `dx bundle --release --package zord-gui --platform desktop`); relative paths in
  Dioxus.toml still resolve against the crate dir.
- The release workflow greps for `*.app` (not `Zord.app`).

**How to apply:** if you change identity/icon, update both `Info.plist` and the
icon resource name. Icon is regenerated with `swift tools/make_icon.swift`.
Related: [[macos-build]].
