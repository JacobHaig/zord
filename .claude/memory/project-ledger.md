---
name: project-ledger
description: SUPERSEDED (Phase 39) — the Phase 26 extract→reconcile ledger was deleted in favor of the living overview document; tables remain inert
metadata:
  node_type: memory
  type: project
---

**Phase 39 (June 2026) deleted the Phase 26 project ledger.** The
extract→reconcile→apply pipeline (`zord-overview` extract.rs/reconcile.rs,
`synthesize`/`fold_pending`/`rebuild_from_history`/`ledger_context`), its
prompts (`overview_prompt`, `extract_prompt`, `reconcile_prompt`), the
ledger UI (`LedgerPanel`/`ProjectCard`/`ItemRow`/`LedgerAction`), and the
ledger engine commands are **gone** — it minted random projects and dumped
item piles instead of a usable status page. The `projects` /
`project_items` / `session_overview_state` / `project_history` **tables
remain in the DB, inert** (no destructive migration; old data just isn't
shown).

The Overview is now **one living markdown document**:
- stored in `app_meta` under `overview_doc`; `overview_doc_prev` is a
  one-step "Revert last AI update" snapshot written ONLY before AI folds,
  never on user saves;
- AI-edited per meeting via `zord_overview::update_document` (system prompt
  `zord_config::overview_doc_prompt()`): `##` section per project,
  `- [ ]`/`- [x]` items with owners, dated trailing `## Archive`, archive
  entries pruned after ~30 days, user edits preserved;
- folds tracked **per session** via `sessions.overview_folded_ms` (NULL =
  not folded yet; deliberately NOT a high-water mark, so a newer fold can
  never permanently hide an older unfolded session);
- guarded by a 20 % sanity floor (fold output < base/5 → keep old doc,
  don't stamp, continue with other sessions) and an optimistic re-read
  (user edited mid-fold → one retry against the fresh text);
- auto chain: `overview_auto` setting (default on) runs compress → fold
  after each session's transcript is final; "Update now" folds all
  unstamped sessions oldest-first; CLI: `zord overview` / `--update`.

Compression (the fold's input) is **line-by-line condensation only** — the
rewritten `compress_prompt()` forbids action items/summaries/structure.
"Re-compress all sessions" (Settings → AI) re-runs history with it.

Rendering: pulldown-cmark with **raw HTML events mapped to text** (XSS
guard — the doc is LLM/user-authored and injected via
`dangerous_inner_html`; never remove that mapping). Chat's cross-meeting
scope grounds on the document, falling back to [[cross-meeting-synthesis]]
compressed digests while the doc is empty. Full design: docs/PLAN.md
Phase 39 + docs/superpowers/specs/2026-06-11-living-overview-design.md.
