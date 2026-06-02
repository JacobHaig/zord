---
name: data-locations
description: Where Zord stores config, the SQLite DB, models, audio, and exports per OS
metadata:
  node_type: memory
  type: reference
---

All local data lives under the OS app-data dir (via the `directories` crate,
`ProjectDirs::from("","","Zord")` — empty qualifier/org so macOS collapses to
just the app name). The keychain service id is still `io.zord.zord` (an
identifier, not a path — left unchanged to avoid orphaning stored DB keys).

- macOS: `~/Library/Application Support/Zord/`
- Windows: `%APPDATA%\Zord\data\`
- Linux: `~/.local/share/Zord/`

(Changed from the old reverse-DNS `io.zord.zord` on 2026-05-31 for a cleaner
path; the three `ProjectDirs::from` call sites are zord-config `app_data_dir`,
zord-transcribe `model_cache_dir`, zord-diarize `models_dir`.)

Layout: `config.json` (settings), `zord.db` (SQLite: sessions + segments +
FTS5; `summary` column on sessions; `speaker` column on segments +
`speaker_names` table), `models/` (Whisper `.bin`, Parakeet dirs, summary
GGUFs, diarization ONNX), `logs/` (Phase 17: `zord.log` via tracing-appender,
alongside stderr; always in app-data), `audio/` (kept per-channel WAVs when
keep-audio is on; diarization also writes a temp `<id>.others.wav`, deleted after
the pass when audio isn't retained), `exports/` (GUI export output). `storage_dir`
in settings can relocate db/audio/exports (NOT models, config, or logs). Models
download on first use — never embedded; a failed in-app download shows the direct
URL + "Open models folder" so users can drop the file in manually (Phase 17).

**Model sources** (matters on HF-blocked / corporate networks): Whisper ggml +
Qwen summary GGUFs are on **HuggingFace**; Parakeet + diarization models are on
**GitHub** (k2-fsa/sherpa-onnx releases). So an HF-blocked user should use
Parakeet for ASR. For summaries, Phase 19 added **custom GGUF** support: any
`.gguf` dropped into `models/` is auto-listed in Settings → Summaries (filename
is the id), so a model from any source works without HuggingFace. Verified non-HF
GGUF sources (Phase 22): **ModelScope** (`modelscope.cn/models/Qwen/Qwen2.5-<sz>-Instruct-GGUF/resolve/master/<file>`)
— same filenames as the built-ins, so a browser download drops in and is
recognized; surfaced in the download-failure fallback via `SummaryModel::mirror_url`.
And **Ollama registry** (`registry.ollama.ai/v2/library/<m>/manifests/<tag>` →
`application/vnd.ollama.image.model` layer digest → `/blobs/<digest>` = raw GGUF;
in-app download pending).

All in-app downloads go through the shared **`zord-net`** crate
(`download_to_file`): native-tls (OS cert store → trusts corporate HTTPS-
inspection CAs) + `HTTP(S)_PROXY`/`ALL_PROXY` env proxy + retries (Phase 18). Not
covered: PAC/WPAD or Windows WinINET system proxy with no env var — the manual
browser drop-in still covers that.
Related: [[architecture]], [[diarization-design]], [[teams-audio-options]].
