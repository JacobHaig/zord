---
name: summary-model-catalog
description: Adding summary LLM downloads — must be single-file GGUFs; official Qwen repos shard Q4_K_M at 7B+ (404s); mirror = hf-mirror.com host-swap
metadata:
  node_type: memory
  type: feedback
---

The summary-model catalog (`SummaryModel` in `zord-summarize/src/summarizer.rs`) is a
size/quality ladder of Qwen Instruct Q4_K_M GGUFs, `ALL` ordered ascending by size (which
is also the picker's display order). The downloader (`zord_net::download_to_file`) fetches
ONE url → ONE file.

**Gotcha (cost us a silent 404 bug):** official `Qwen/Qwen2.5-<N>B-Instruct-GGUF` repos
**shard Q4_K_M into multiple parts** at 7B and up (`...-q4_k_m-00001-of-00002.gguf`). A
single-file fetch of the unsharded name 404s. So any catalog entry MUST point at a
**single-file** Q4_K_M build:
- small (≤3B): official `Qwen/...-GGUF` is single-file — fine.
- 7B+: use **bartowski** (`bartowski/<Model>-GGUF`, e.g. `Qwen2.5-7B-Instruct-Q4_K_M.gguf`,
  `Qwen_Qwen3-14B-Q4_K_M.gguf`) or `lmstudio-community` — they publish single files.
- Qwen3 official `Qwen/Qwen3-8B-GGUF` happens to be single-file too.

**When adding a model, ALWAYS verify before committing** (network works from the sandbox):
1. `curl -s https://huggingface.co/api/models/<repo> | jq` (or python) → confirm the exact
   `Q4_K_M` rfilename is a SINGLE file (no `-0000N-of-0000M`).
2. `curl -s -L -r 0-0 -o /dev/null -w '%{http_code}' <resolve-url>` → must be **206/200**.
3. Grab the real size from the API (`siblings[].lfs.size` / `?blobs=true`) for `size_label`.

**Mirror:** `mirror_url()` is a uniform host-swap `huggingface.co` → `hf-mirror.com` (same
path/filename, works for any HF repo incl. bartowski). Don't reintroduce per-model
ModelScope links — they had different filenames/sharding and broke.

**Backward compat:** `name()` is the stable id persisted in `settings.summary_model`; never
change an existing value (orphans saved selections). `filename()` can change freely (it's
just the on-disk name). Related: [[asr-diarization-model-watchlist]], [[llm-test-endpoint]],
[[feature-build-deps]].
