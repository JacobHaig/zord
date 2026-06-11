# Living Overview + Faithful Compression (Phase 39) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compression becomes faithful line-by-line condensation; the Overview
becomes one living, user-editable, AI-maintained markdown document organized
by project — replacing the Phase 26 ledger pipeline.

**Architecture:** new prompts in zord-config; document storage in the existing
`app_meta` table (`overview_doc`, `overview_doc_prev`, `overview_doc_fold_ms`);
zord-overview reworked to a single `update_document` LLM call (extract/
reconcile/synthesize deleted, tables left inert); engine gains
`UpdateOverviewDoc`/`RecompressAll`/`SaveOverviewDoc`/`LoadOverviewDoc` + an
auto chain after transcription; GUI renders markdown (pulldown-cmark) with an
Edit toggle.

**Tech Stack:** existing llm-local/llm-remote backends; `pulldown-cmark`
(pure Rust) in zord-gui.

Spec: `docs/superpowers/specs/2026-06-11-living-overview-design.md`.
Commits to `develop` per task; full gate before each
(`cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings &&
cargo clippy -p zord-gui --features llm-remote -- -D warnings &&
cargo test --workspace`; note most of this phase is llm-gated — clippy the
`llm-remote` config every task since it's the cheap LLM build).

---

### Task 1 (39a): zord-config — prompts + `overview_auto` (TDD)

**Files:** `crates/zord-config/src/lib.rs`

- [ ] **Step 1 (red):** tests:
```rust
#[test]
fn phase39_prompts_and_defaults() {
    let c = compress_prompt().to_lowercase();
    assert!(c.contains("line"), "compress prompt must demand line-by-line output");
    assert!(!c.contains("action item"), "compress prompt must not synthesize");
    let o = overview_doc_prompt().to_lowercase();
    assert!(o.contains("archive") && o.contains("30 days"));
    assert!(o.contains("full updated document"));
    assert!(Settings::default().overview_auto);
}
```
- [ ] **Step 2:** rewrite `compress_prompt()` (~line 300) per the spec wording
  (line-by-line, keep `Name:` labels and order, shortest faithful rewording,
  drop pure filler lines, never add structure/action items/summaries/
  interpretation, output condensed transcript lines only). Add
  `overview_doc_prompt()` next to `overview_prompt()` per the spec wording
  (maintain `##` project sections, `- [ ]`/`- [x]` items with owners,
  `## Archive` with dates, delete archive entries older than 30 days,
  preserve user-written content, output the FULL updated document). Leave the
  old `overview_prompt()`/`extract_prompt()` for Task 2 to delete with their
  consumers. Add `overview_auto: bool` (serde default fn returning true, doc
  comment, Default impl entry — mirror `voiceprints_match`'s pattern).
- [ ] **Step 3:** green; commit `feat(config): line-by-line compression + living-overview prompts (Phase 39a)`.

---

### Task 2 (39b): zord-overview — `update_document`, delete the ledger pipeline

**Files:** `crates/zord-overview/src/lib.rs`, delete `extract.rs`,
`reconcile.rs`; `crates/zord-store/src/lib.rs` only if app_meta helpers need a
typed wrapper (it already has get/set at ~lines 207/218 — reuse, don't add).

- [ ] **Step 1:** read the crate: how `synthesize`/`fold_pending`/
  `rebuild_from_history` get an LLM handle and a progress callback — the new
  fn keeps the same shape so the engine swap (Task 3) is mechanical.
- [ ] **Step 2:** add:
```rust
/// One fold: rewrite the living overview document with `session_input`
/// (the session's compressed text, else its labeled transcript) merged in.
/// Returns the full replacement markdown. Pure LLM wrapper — bookkeeping
/// (which sessions to fold, snapshots, high-water mark) lives in the engine.
pub fn update_document(
    doc: &str,
    session_input: &str,
    session_label: &str, // "2026-06-11 — <title>" for the prompt's dating
    llm: &Llm,           // whatever handle type the crate already uses
    progress: &mut dyn FnMut(/* match existing signature */),
) -> Result<String>
```
  Prompt assembly: system = `zord_config::overview_doc_prompt()`; user =
  current doc (or "(empty document)") + "--- New meeting (<label>) ---" +
  session_input. Reuse the crate's existing token-budget/context handling.
- [ ] **Step 3:** delete `extract.rs`, `reconcile.rs`, `synthesize`,
  `fold_pending`, `rebuild_from_history`, `OverviewData`-producing code and
  the now-unused prompts in zord-config (`overview_prompt`, the Phase 26
  extract prompt). Keep the crate compiling under both llm features.
- [ ] **Step 4:** unit-test what's pure (prompt assembly / input truncation)
  with the crate's existing test affordances; gate green; commit
  `feat(overview): living-document update; retire extract/reconcile ledger (Phase 39b)`.

---

### Task 3 (39c): engine — fold bookkeeping, auto chain, re-compress sweep

**Files:** `crates/zord-gui/src/engine.rs`

Anchors: `SummCmd::{Overview, FoldOverview}` arms at ~765-775 with workers
`overview_one`/`fold_overview` at ~971/1002; `SummCmd::Compress(id)` at ~758;
`DbCmd::LoadOverview`/`LoadLedger` at ~1675/1679; `Event::Overview` at ~113;
`OverviewData` at ~198; post-stop transcription completion in
`post_transcribe_session`/`run_integration_session` (~3203) and the normal
session end path; `app_meta` store API at zord-store ~207/218.

- [ ] **Step 1: storage + selection helpers** (pure, unit-tested in the
  engine test module): `overview_doc`/`overview_doc_prev`/
  `overview_doc_fold_ms` app_meta keys behind small fns
  (`load_overview_doc(store) -> (String, u64 /*updated_at*/)`, save with
  prev-snapshot, fold-mark get/bump); `unfolded_sessions(store, mark) ->
  Vec<Session>` (ended, has transcript, `ended_at > mark`, oldest-first) —
  test with a temp store.
- [ ] **Step 2: commands.** Replace `SummCmd::Overview`/`FoldOverview` with
  `UpdateOverviewDoc { session: Option<String> }`; replace
  `DbCmd::LoadOverview`/`LoadLedger` with `LoadOverviewDoc` and add
  `DbCmd::SaveOverviewDoc(String)`; replace `Event::Overview(..)`/ledger
  events with `Event::OverviewDoc { markdown: String, updated_at: u64 }`.
  Worker for UpdateOverviewDoc: job "overview", for each target session
  (one for `Some(id)`, `unfolded_sessions` for `None`): input = compressed
  text else labeled transcript (reuse `render_labeled_transcript`), call
  `zord_overview::update_document`, sanity floor (folding into a non-empty
  doc must return ≥ 20% of the old length — else notice + keep old),
  snapshot prev, optimistic write (re-read updated_at; changed → retry once
  with fresh doc), bump fold mark to that session's `ended_at`, emit
  OverviewDoc. Cancellable between sessions.
- [ ] **Step 3: RecompressAll.** `SummCmd::RecompressAll`: all ended sessions
  with segments, oldest-first, reuse the existing `Compress(id)` body
  (factor it to a fn), job "recompress" cancellable between sessions, final
  notice with the count.
- [ ] **Step 4: auto chain.** Where a session's transcript work finishes
  (both the normal-session end and the integration post-stop path — put it
  beside the existing auto hooks at the end of those flows): if
  `settings.overview_auto` and an LLM backend is configured (reuse the
  existing `build_llm_backend` availability check pattern): send
  `SummCmd::Compress(id)` if not yet compressed, then
  `SummCmd::UpdateOverviewDoc { session: Some(id) }` (the summ worker is a
  single thread — queued commands serialize correctly).
- [ ] **Step 5:** gate green (incl. `--features llm-remote` clippy); commit
  `feat(engine): living-overview fold + auto chain + re-compress sweep (Phase 39c)`.

---

### Task 4 (39d): GUI — document view, markdown rendering, settings

**Files:** `crates/zord-gui/src/main.rs` (OverviewView + Settings → AI tab),
`crates/zord-gui/src/style.css`, `crates/zord-gui/Cargo.toml`
(`pulldown-cmark = "0.12"`, default deps — markdown rendering is useful even
in LLM-less builds since the doc is user-editable).

- [ ] **Step 1:** replace the ledger Overview UI (`OverviewView` at ~3603 and
  its project-card/ledger components + `LedgerAction` plumbing at ~190) with
  the document panel:
  - state: `overview_doc: Signal<String>`, `overview_doc_updated: Signal<u64>`,
    `overview_editing: Signal<bool>`, draft signal; `Event::OverviewDoc` arm;
    `DbCmd::LoadOverviewDoc` sent when opening the view (mirror
    `on_open_speakers`).
  - rendered mode: `pulldown_cmark::Parser` (enable TASKLISTS + TABLES +
    STRIKETHROUGH options) → `push_html` → `div { class: "md-doc",
    dangerous_inner_html: html }`.
  - edit mode: full-height textarea + Save (`DbCmd::SaveOverviewDoc`) /
    Cancel; toggle button Rendered ⟷ Edit.
  - toolbar: "Update now" → `UpdateOverviewDoc { session: None }` (busy via
    the existing jobs panel pattern), "Revert last AI update" (confirm) →
    new `DbCmd::RevertOverviewDoc` (swap doc/prev in the store — add the
    arm in Task 4, trivial), "Updated <fmt_date>" stamp, empty state copy.
- [ ] **Step 2: CSS** — `.md-doc` typography from tokens (headings, lists,
  `li.task-list-item` checkboxes, code, tables, blockquote, hr; readable
  measure ~72ch; Archive section de-emphasized via `h2 + content` is not
  selectable in CSS — skip, keep uniform).
- [ ] **Step 3: Settings → AI tab**: `overview_auto` toggle ("Update the
  Overview automatically after each recording is transcribed") + danger-ish
  "Re-compress all sessions" button with confirm dialog ("Re-runs the local
  LLM over every saved transcript — can take a long time") →
  `SummCmd::RecompressAll`.
- [ ] **Step 4:** gate green; commit
  `feat(gui): living Overview document — markdown view, editor, settings (Phase 39d)`.

---

### Task 5 (39e): docs + close-out

- [ ] PLAN.md: Phase 39 entry (✅ style, honest "live LLM pass pending");
  update the Phase 23c/26 entries with a superseded-by-39 note; README's
  overview bullet reworded ("a living, editable project document the AI
  keeps current"); KICKSTART unchanged unless it mentions the ledger.
- [ ] Repo memory: update the overview-related memory file (or add one):
  document model, app_meta keys, fold mark, the compression philosophy.
- [ ] Full gate incl. `llm-remote` + `discord,voiceprints,parakeet` clippy;
  push develop, ff-merge main, push.
- [ ] Manual pass with the user: record → watch compression come out
  line-by-line → overview self-updates → hand-edit survives the next fold →
  Revert works → Re-compress all on a couple of old sessions.

---

## Self-review

- **Spec coverage:** prompts/toggle (T1), update_document + pipeline removal
  (T2), bookkeeping/auto chain/re-compress/optimistic write/sanity floor
  (T3), rendered⟷edit UI + revert + settings (T4), docs (T5). Edge cases:
  LLM-less builds (doc still viewable/editable — pulldown-cmark in default
  deps, T4), mid-job user edit (T3 Step 2 retry), garbage output floor (T3),
  transcript-less sessions skipped (T3 Step 1 selection).
- **Placeholders:** update_document's exact llm-handle/progress types
  deliberately deferred to the crate's existing signatures (T2 Step 1 reads
  them first) — acceptable; behavior pinned.
- **Type consistency:** `Event::OverviewDoc { markdown, updated_at }` used in
  T3 and T4; `UpdateOverviewDoc { session: Option<String> }` in T3/T4;
  `RevertOverviewDoc` introduced in T4 with its arm.
