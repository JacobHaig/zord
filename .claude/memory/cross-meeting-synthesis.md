---
name: cross-meeting-synthesis
description: Phase 23 "Overview" — synthesize 30-50 meetings into per-project rollups via map-reduce, not one giant context
metadata:
  node_type: memory
  type: project
---

Phase 23 is the headline feature: a holistic **cross-meeting Overview** — across
the user's last ~30–50 recordings, a per-project picture of state / pending /
accomplished / owners / open questions, oriented around the user ("Me"). Goal:
answer "where's project X?" from a current, faithful rollup.

**Key decision — compress per meeting, then synthesize; NOT raw big context.**
50 raw meetings ≈ 400–650K tokens, far beyond practical local/CPU context. So:
1. **Compress** (per meeting, once, cached): LLM condenses the meeting into
   token-minimal **free-form dense prose** preserving facts — projects + state,
   action items (owner → what → status), completed, decisions, open questions.
   ~300–800 tokens vs 8–13K raw. Stored on the session; "Compress" + "Copy
   compressed" buttons (lazy-generate if missing). It's working memory, not the
   record — full transcript stays for drill-down + citations.
2. **Synthesize** (Overview): feed the stored compressions in ONE pass — ~30–50 ≈
   ~20–35K tokens **fit a 32K-context model** → holistic project-grouped rollup.
   CPU prefill of ~30K is minutes ("churn in background"). Fallback at scale:
   hierarchical (group by project + compress-the-compressions).
3. **Overview** = per-project rollups + pinned "My open action items".

Do NOT feed raw transcripts into one prompt — compress first; that's why the
user's "CPU, churn in background" instinct works.

**Locked:** compression format = **free-form dense prose**; dedicated full
**Overview view** (3rd top-level mode, button above the session list); **LLM
auto-detects + names projects** (+ normalization). Items cite source meeting
(anti-hallucination). Owner attribution leans on diarization + names; "Me" always
known. Sub-steps 23a (compress + buttons) → 23b (overview synthesis worker) →
23c (Overview UI) → 23d (refresh/edit + cross-meeting chat). Reuses llama.cpp; no
new heavy deps.

**23a is DONE** (compress shipped). The old `N_CTX=8192` cap is gone: context is
now per-call. `zord_summarize::compress(transcript, prompt, n_ctx)` + `GenOpts`
(`summarize()` is a thin wrapper at 8K; `generate()` is the shared core).
`zord_config::compress_prompt()` (dense free-form prose, machine-oriented, no
formatting) + `compress_ctx` setting (default 16K, clamped 8K–128K, editable in
Settings → Summaries). Store: `compressed` column + `set_compressed`/
`get_compressed`. Engine: `SummCmd::{Summarize,Compress}` over the summarize
worker; `Event::Compressed` emitted on session load + after compress. GUI: 🗜
Compress button + collapsible "Compressed (dense)" panel (Show/Hide + Copy). CLI:
`zord compress <id>`.

**23b is DONE** (synthesis worker). New crate **`zord-overview`** (feature `llama`,
shared by CLI + GUI): `synthesize(db, settings, progress)` loads the summary model
once, gathers the newest `overview_max_meetings` sessions, reuses each stored
compression / lazily generates+persists missing ones, assembles them (headed by
`YYYY-MM-DD · title`), one-pass synthesis at `overview_ctx`. **Hierarchical
fallback** when over budget: pack into groups → compress-the-compressions →
recency-trim (logged). `zord_config::overview_prompt()` (project-grouped, "My open
action items" first, cites sources) + `overview_ctx` (32K) / `overview_max_meetings`
(50). Store: generic `app_meta(key,value,updated_at)` + `set_meta`/`get_meta`;
rollup under key `overview` (+ `overview_meetings`); `zord_overview::load(store)`
reads it without the LLM. `zord_summarize::count_tokens()` + `GenOpts::overview()`;
`generate()` now takes the user msg verbatim (Transcript: framing moved into
summarize/compress). CLI: `zord overview [--max N]`.
**Next: 23c** — GUI Overview view: engine `SummCmd::Overview` + `DbCmd::LoadOverview`
+ `Event::Overview`, a 📊 Overview top-level mode, generate/refresh + "last updated".
Full plan: docs/PLAN.md Phase 23. Related: [[diarization-design]], [[feature-flags]].
