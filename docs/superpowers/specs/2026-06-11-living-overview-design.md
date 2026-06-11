# Faithful compression + the living Overview document (Phase 39)

**Date:** 2026-06-11 · **Status:** approved (user-confirmed decisions below)

## Problem

Two compounding issues:
1. **Compression synthesizes instead of compressing.** The current prompt
   (`zord-config compress_prompt()`) explicitly asks for "action items as
   owner → task → status … decisions … open questions" — so the stored
   "compressed" text is an opinionated digest, not the conversation.
2. **The Overview mints random projects and dumps item piles.** Phase 26's
   pipeline (per-meeting JSON extract → ledger reconcile into
   `projects`/`project_items`) lets every meeting's delta invent project
   names, and the result reads as collections of items, not a status page.

## Decisions (user-confirmed)

- **Compression = faithful line-by-line condensation, nothing else.** Same
  utterance order, speaker labels kept, each line minimally reworded
  ("What I've been working on this morning and will continue to work on is
  the CICD process, and I should be done by end of today, at which point
  Jerry will review" → "Me: continuing CI/CD work, done by EOD; Jerry
  reviews after"). Pure-filler lines (greetings, acknowledgements,
  backchannel) may be dropped entirely. NO headings, bullets, action items,
  summaries, reordering, or merging of distinct statements.
- **Overview = ONE living markdown document** organized by project
  (`## <Project>` sections), maintained by the AI from compressed
  transcripts: it updates sections, checks off done items (`- [x]`), moves
  resolved/stale content into a trailing `## Archive` section (dated), and
  deletes archive entries older than ~30 days. The user can edit the
  document directly at any time and the agent must preserve those edits.
- **Fresh start**: the new document begins empty; the old ledger UI is
  retired (its tables stay in the DB, inert — no destructive migration).
- **Update timing**: automatically after a session's transcript is
  compressed, gated by a new settings toggle (default ON), plus a manual
  "Update overview" button.
- **Re-compress**: new prompt applies going forward; a "Re-compress all
  sessions" button (Settings) redoes history on demand (job-registered,
  cancellable).
- **Rendering**: Overview shows rendered markdown with a toggle to a raw
  editor; nice-to-have framing from the user ("render as a markdown file…
  swap back and forth with a toggle").

## Architecture

### zord-config
- Rewrite `compress_prompt()`: "You condense a meeting transcript
  line-by-line… keep speaker labels and order; reword each utterance to its
  shortest faithful form; drop pure filler lines; never add structure,
  action items, summaries, or interpretation; output only condensed
  transcript lines in the same `Name: text` format."
- New `overview_doc_prompt()`: "You maintain a running markdown status
  document organized by project (`##` sections)… fold the new meeting's
  condensed transcript in: update affected sections in place; create a new
  section only for a clearly distinct ongoing project; track action items
  as `- [ ]`/`- [x]` with owners; move resolved/stale content to
  `## Archive` with the date; delete Archive entries older than 30 days;
  preserve any user-written content/wording you aren't updating; output the
  FULL updated document, nothing else."
- Setting `overview_auto: bool` (default **true**) — "Update the Overview
  automatically after each recording is transcribed & compressed."

### Storage (zord-store — reuse `app_meta`, no schema change)
- `overview_doc` — the document; `overview_doc_prev` — snapshot taken
  before each AI edit (one-step "Revert last AI update").
- `overview_doc_fold_ms` — high-water mark: sessions with `ended_at` newer
  than this are un-folded; the manual button folds them oldest-first, the
  auto path folds the just-finished session. Both bump the mark.

### zord-overview (rework)
- New `update_document(doc: &str, session_input: &str, today: &str, llm) ->
  Result<String>` — one LLM call, returns the full replacement document.
  Session input = the stored compressed text, else the labeled transcript.
- The Phase 26 extract/reconcile/synthesize pipeline and its prompts are
  deleted (tables remain). `OverviewData`/ledger events go with them.

### Engine (zord-gui)
- `SummCmd::UpdateOverviewDoc { session: Option<String> }` — `Some(id)`
  (auto path) folds that session; `None` (manual) folds every un-folded
  ended session oldest-first. Job-registered ("overview"), cancellable
  between sessions. Before writing: re-read `overview_doc`; if it changed
  since the job read it (user edited mid-run), retry once against the
  fresh text. Always snapshot to `overview_doc_prev` first.
- Auto chain: where post-stop/live transcription completes, when
  `overview_auto` and an LLM backend is configured → `Compress(id)` then
  `UpdateOverviewDoc { session: Some(id) }` (compression itself only runs
  if not already compressed).
- `SummCmd::RecompressAll` — sweep all ended sessions with transcripts,
  re-run compression with the new prompt (job, cancellable, notices count).
- `DbCmd::LoadOverview`/`LoadLedger` + `Event::Overview(OverviewData)` are
  replaced by `LoadOverviewDoc` + `Event::OverviewDoc { markdown: String,
  updated_at: u64 }`. Saving a user edit: `DbCmd::SaveOverviewDoc(String)`.

### UI
- **Overview view** becomes the document: rendered markdown by default
  (new dep `pulldown-cmark`, pure Rust — render to HTML into
  `dangerous_inner_html`, styled from the 36a token layer: headings, lists,
  checkboxes, code, tables) ⟷ **Edit** toggle (textarea + Save). Toolbar:
  Update now (busy → jobs panel), Revert last AI update, last-updated
  stamp. Empty state explains the flow ("record a meeting — the overview
  writes itself; or press Edit and start typing").
- **Settings → AI**: `overview_auto` toggle + "Re-compress all sessions"
  button (confirm dialog — it's hours of local LLM on a big library).
- The chat "overview" scope (if it feeds the old synthesized overview/
  ledger) now feeds the document text.

## Edge cases
- No LLM features built/configured: Overview view shows the document
  read-only with Edit still available (it's just markdown); Update-now
  explains the missing backend. Auto chain no-ops.
- User edits while an update job runs: optimistic re-read + single retry;
  the user's text is in the doc the retry reads.
- Empty/garbage LLM output (< some sanity floor, e.g. output shorter than
  20% of the previous doc when folding): keep the old doc, notice the
  failure, leave `overview_doc_prev` untouched.
- Sessions with no transcript: skipped by both fold paths.

## Testing
- Config: prompt-content assertions (must contain "line", must NOT mention
  action items for compress; doc prompt mentions Archive/30 days) + the
  `overview_auto` default.
- zord-overview: `update_document` is a thin LLM wrapper — unit-test the
  fold bookkeeping (high-water mark math) and the sanity floor with a fake
  LLM backend if the crate has one; else factor those as pure fns + test.
- Engine: fold-selection (which sessions are un-folded) factored pure +
  tested; snapshot/retry logic exercised via the store in a temp DB.
- Manual pass: record → transcribe → compression is line-by-line → overview
  updates itself; edit the doc by hand → next update preserves the edit;
  Revert undoes one AI pass; Re-compress all reworks old sessions.
