---
name: project-ledger
description: Phase 26 rolling project ledger — the stateful Overview (extract→reconcile→apply fold); supersedes the Phase 23 markdown rollup
metadata:
  node_type: memory
  type: project
---

Phase 26 replaced the stateless from-scratch Overview (one markdown blob re-synthesized
each refresh) with a **durable, incrementally-folded per-project ledger**. The Overview
view is now a list of **projects**, each holding **items** (action / question / decision)
with status, owner, and provenance. Each meeting is folded in as a *delta* — bounded work
(one meeting + current ledger), so cost doesn't grow with corpus size.

**Flow (per meeting):** `transcript → compression → extract → snapshot+reconcile → apply`
- **Extract** (`zord-overview::extract`, 26b): LLM turns one meeting into a `SessionExtract`
  (projects touched, items, resolved-mentions). Stateless. `parse_extract` is backend-free
  and lenient (string-aware brace scan digs JSON out of prose/fences). Input = the meeting's
  dense **compression** (reused/lazily built — the Phase 23 compress infra lives on for this
  and as the chat fallback).
- **Reconcile** (`zord-overview::reconcile`, 26c): `snapshot(store)` (active projects + open
  items, each with a real id) + extract → `plan_fold` (LLM) → `ReconcilePlan` (match/create
  projects, complete items by id, add new).
- **Apply** (`apply_plan`, backend-free, fully unit-tested): performs the mutations.

**Load-bearing INVARIANTS — do not break these:**
1. **`apply_plan` validates every id** the model returned against the real ledger; an
   invented/stale id silently drops that op. This is the only thing preventing hallucinated
   completions / phantom projects. Never "trust" model ids.
2. **`manual` flag** on `project_items` protects hand edits from being overwritten by later
   auto-folds. All GUI/CLI edits set it; only `clear_ledger` (rebuild) wipes it.
3. **Fold timestamp = the meeting's own time** (`ended_at`/`started_at`), NOT wall-clock — so
   incremental fold and `rebuild_from_history` produce identical activity ordering.
4. **Ledger ids are deterministic**: `<session_id>-pN` / `-iN` (unique because a session folds
   once; rebuild wipes first). Manual items: `manual-<now_ms>`.
5. Unroutable items → fixed **`Unfiled`** project (id `unfiled`). null-match projects merge by
   normalized name before creating (no dup projects).

**Idempotency:** `session_overview_state` records applied sessions (+ cached extract JSON).
`fold_pending` folds only `unapplied_sessions()` (oldest-first). `rebuild_from_history` =
`clear_ledger` + fold all (DESTRUCTIVE — drops manual edits; UI/CLI confirm).

**Surfaces:** store tables `projects`/`project_items`/`session_overview_state`/
`project_history` ([[architecture]]). GUI: Overview = `LedgerPanel`/`ProjectCard`/`ItemRow`,
edits via `LedgerAction`→`DbCmd`→re-emit `Event::Ledger` (Engine isn't PartialEq, so a single
`EventHandler<LedgerAction>` keeps components decoupled). `SummCmd::FoldOverview{rebuild}`.
Chat + CLI ground on `ledger_context(store)` (LLM-free renderer); CLI `zord overview`
[`--refresh`|`--rebuild`]. Legacy `app_meta["overview"]` shown read-only until first fold
(graceful upgrade), then superseded — see [[cross-meeting-synthesis]] for that Phase 23 infra.

Deferred: project **merge/split** (move-item covers the common case). Routing quality scales
with model strength ([[llm-test-endpoint]] for validating prompts). Full plan: docs/PLAN.md
Phase 26. Related: [[feature-flags]], [[commit-and-workflow]].
