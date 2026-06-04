---
name: llm-prefill-nbatch
description: llama.cpp aborts (ggml_abort/SIGABRT) if a prompt is decoded in one batch larger than n_batch — prefill in chunks
metadata:
  node_type: memory
  type: feedback
---

**Symptom.** Summarize / Compress / Overview crashed the whole app (macOS: EXC_CRASH
SIGABRT with `abort` ← `ggml_abort` ← `llama_context::decode` ← `llama_decode` ←
`Summarizer::complete`). It looked like an OOM but wasn't.

**Cause.** `Summarizer::complete` (crates/zord-summarize/src/summarizer.rs) fed the
ENTIRE prompt into one `LlamaBatch` and called `ctx.decode` once. llama.cpp asserts a
single decode batch can't exceed `n_batch` (default **2048** tokens); a larger prompt
trips `ggml_abort` → the process dies. Overview hits it immediately (it concatenates
many per-meeting compressions); long single-meeting summaries hit it too. It is
**cross-platform** (not the Windows-only CRT bug — see [[ci-windows-crt]]).

**Why this is sneaky:** it's content-dependent (only prompts > 2048 tokens), and
`ggml_abort` is a hard native abort that bypasses Rust panic hooks — pre-fix it left
nothing in the logs.

**How to apply.** Prefill in chunks ≤ `n_batch`. The fix decodes the prompt in
**512-token chunks** (safely under n_batch on every backend), incrementing positions,
marking only the final token for logits; the generation loop is unchanged. NEVER feed
a whole prompt in a single `decode` again. Regression test:
`long_prompt_prefills_without_abort` in summarizer.rs (run with
`cargo test -p zord-summarize --features llama -- --ignored`) feeds a ~7000-token
prompt and asserts it generates instead of aborting.

**Crash diagnostics (added alongside).** On a silent close, check the app-data
`logs/` dir ([[data-locations]]): `llm-trace.log` has flushed phase breadcrumbs
(`load:start/done`, `infer:ctx-alloc`, `infer:prefill`, `prefill-done`, `infer:done`)
and `crash.log` captures Rust panics. crash.log empty + llm-trace stuck at
`infer:prefill` ⇒ a native abort inside decode (this n_batch bug, or the CRT one).
Related: [[cross-meeting-synthesis]], [[ci-windows-crt]], [[verification-limits]].
