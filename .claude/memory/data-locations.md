---
name: data-locations
description: Where Zord stores config, the SQLite DB, models, audio, and exports per OS
metadata:
  node_type: memory
  type: reference
---

All local data lives under the OS app-data dir (via the `directories` crate,
`ProjectDirs::from("io","zord","zord")`):

- macOS: `~/Library/Application Support/io.zord.zord/`
- Windows: `%APPDATA%\zord\zord\data\`
- Linux: `~/.local/share/zord/`

Layout: `config.json` (settings), `zord.db` (SQLite: sessions + segments +
FTS5; `summary` column on sessions), `models/` (Whisper `.bin`, Parakeet dirs,
summary GGUFs), `audio/` (kept per-channel WAVs when keep-audio is on),
`exports/` (GUI export output). `storage_dir` in settings can relocate
db/audio/exports (NOT models or config). Models download on first use — never
embedded. Related: [[architecture]].
