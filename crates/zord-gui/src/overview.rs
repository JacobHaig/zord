//! Living overview document view — Phase 39d / 43e.
//!
//! Replaces the Phase 26 project-ledger UI with a markdown document panel:
//! rendered view by default, raw textarea editor on toggle, and a toolbar with
//! Update / Revert / stamp controls (gated to LLM builds).
//!
//! Phase 43e: task-list checkboxes in the rendered view are made clickable via
//! an eval bridge (JS → Rust via `dioxus.send` / `eval.recv`).  The bridge
//! calls [`toggle_nth_task`] to flip the Nth checkbox in the markdown source
//! and saves via `DbCmd::SaveOverviewDoc`.
//!
//! Re-render / generation hazard: every time the rendered HTML changes (a new
//! doc version), a new `use_effect` run spawns a fresh bridge task.  The
//! previous task's `eval` handle becomes invalidated when the DOM is rebuilt,
//! so its `recv()` returns an `EvalError::Finished` / error — the loop breaks
//! naturally.  We also carry a `bridge_gen` counter: incrementing it on each
//! new doc causes the old spawn to detect it no longer owns the generation and
//! exit, providing an extra safety net.

use dioxus::prelude::*;
use zord_config::Settings;

use crate::{
    engine::{DbCmd, Engine, SummCmd},
    fmt_date, icon,
};

/// Toggle the Nth task-list checkbox (`- [ ]` / `- [x]`, case-insensitive x,
/// also `*`- or `+`-bulleted) in markdown source order.
///
/// Returns `None` when `n` is out of range (doc changed under us — caller
/// drops the click).  Code fences (triple-backtick) are skipped so that
/// example task lists inside fences are never counted.
///
/// Parse rule (mirrors pulldown-cmark TASKLISTS):
/// - Line whose *trimmed* start is `[-*+]` then a space, then `[` then
///   space-or-x/X then `]` then a space (or end-of-content).
/// - Source order == rendered order — that's the index contract.
pub fn toggle_nth_task(md: &str, n: usize) -> Option<String> {
    let mut result = String::with_capacity(md.len());
    let mut count = 0usize;
    let mut in_fence = false;
    let mut found = false;

    for line in md.split_inclusive('\n') {
        // Track fenced code blocks so we don't count task items inside them.
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            result.push_str(line);
            continue;
        }

        if !in_fence && !found {
            if let Some((pre, rest)) = parse_task_marker(line) {
                if count == n {
                    // Flip the checkbox state.
                    let toggled = if let Some(tail) = rest.strip_prefix("[ ]") {
                        format!("{pre}[x]{tail}")
                    } else {
                        // Checked: [x] or [X] — rest.strip_prefix("[x]") / "[X]"
                        let tail = rest.get(3..).unwrap_or("");
                        format!("{pre}[ ]{tail}")
                    };
                    result.push_str(&toggled);
                    found = true;
                    continue;
                }
                count += 1;
            }
        }
        result.push_str(line);
    }

    if found {
        Some(result)
    } else {
        None // n was out of range
    }
}

/// Parse a task-list marker from a line.
///
/// Returns `(prefix_including_bracket_open, rest_starting_at_bracket)`
/// where `rest` is `"[ ] …"` or `"[x] …"` (bracket pair + trailing content).
///
/// The prefix includes the leading whitespace, bullet, space, and `[`.
fn parse_task_marker(line: &str) -> Option<(&str, &str)> {
    // Find the byte offset where the trimmed content starts.
    let indent_len = line.len() - line.trim_start().len();
    let after_indent = &line[indent_len..];

    // Must start with a list bullet.
    let bullet = after_indent.chars().next()?;
    if !matches!(bullet, '-' | '*' | '+') {
        return None;
    }
    let bullet_len = bullet.len_utf8();

    // Must be followed by a single space.
    let after_bullet = after_indent.get(bullet_len..)?;
    if !after_bullet.starts_with(' ') {
        return None;
    }
    let after_space = &after_bullet[1..];

    // Must be followed by `[` then space/x/X then `]` then space (or eol).
    if !after_space.starts_with('[') {
        return None;
    }
    let inner = after_space.get(1..)?;
    let marker_char = inner.chars().next()?;
    if !matches!(marker_char, ' ' | 'x' | 'X') {
        return None;
    }
    let after_marker = inner.get(marker_char.len_utf8()..)?;
    if !after_marker.starts_with(']') {
        return None;
    }
    // The character after `]` must be a space, `\n`, `\r`, or end-of-string.
    let after_close = &after_marker[1..];
    if !after_close.is_empty()
        && !after_close.starts_with(' ')
        && !after_close.starts_with('\n')
        && !after_close.starts_with('\r')
    {
        return None;
    }

    // prefix = everything up to (but not including) the `[` char.
    let bracket_offset = indent_len + bullet_len + 1 /* space */;
    let prefix = &line[..bracket_offset];
    let rest = &line[bracket_offset..]; // starts with `[`
    Some((prefix, rest))
}

/// Render the markdown string `md` to HTML using pulldown-cmark.
///
/// Raw HTML is neutralized: pulldown-cmark would otherwise pass
/// `Event::Html`/`Event::InlineHtml` through verbatim, and the result lands in
/// `dangerous_inner_html` — the document is LLM/user-authored, so a literal
/// `<script>`/`<img onerror>` would execute in the webview. Mapping those
/// events to `Event::Text` makes push_html escape them instead.
fn md_to_html(md: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser};
    let opts = Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(md, opts).map(|e| match e {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        other => other,
    });
    let mut html_out = String::new();
    html::push_html(&mut html_out, parser);
    html_out
}

/// The Overview document panel.
///
/// Toolbar: Edit/View toggle; "Update now" (LLM builds only); "Revert last AI
/// update" (confirm, LLM builds only); "Updated <date>" stamp.
/// Empty state: hero explainer.
/// Edit mode: full-height textarea + Save / Cancel.
/// Rendered mode: `div.md-doc` with `dangerous_inner_html`.
#[component]
pub fn OverviewDocView(
    doc: Signal<String>,
    doc_updated: Signal<u64>,
    editing: Signal<bool>,
    draft: Signal<String>,
    notice: Signal<Option<String>>,
    engine: Engine,
    settings: Signal<Settings>,
) -> Element {
    let _ = settings; // reserved for future per-view settings

    let md = doc.read().clone();
    let updated_at = *doc_updated.read();
    let is_editing = *editing.read();

    // Confirm-revert dialog state (local).
    let mut confirm_revert = use_signal(|| false);

    let html = md_to_html(&md);
    let is_empty = md.trim().is_empty();

    // ── Phase 43e: eval bridge — clickable checkboxes ─────────────────────
    //
    // Strategy: a `use_effect` that subscribes to `html` re-attaches the JS
    // click listeners every time the rendered document changes.  The previous
    // bridge task's `eval` handle is dropped (DOM was rebuilt), so its
    // `eval.recv()` returns an error and the loop exits naturally.  A
    // `bridge_gen` counter is also incremented on each new HTML version so the
    // spawned task can bail if a newer generation has already started.
    //
    // We only attach when NOT in edit mode — the rendered `div.md-doc` is not
    // mounted while `editing` is true, so there is nothing to query.
    let mut bridge_gen = use_signal(|| 0u64);

    {
        // Clone what the effect closure needs.
        let html_for_effect = html.clone();
        let engine_cb = engine.clone();
        use_effect(move || {
            // Subscribe to both html content and editing state.
            let _ = &html_for_effect; // reactive dependency
            let currently_editing = *editing.read();

            if currently_editing {
                // Rendered view is not mounted — skip bridge setup.
                return;
            }

            // Bump generation so any prior spawn knows it's been superseded.
            let gen = *bridge_gen.peek() + 1;
            bridge_gen.set(gen);

            // JS: remove `disabled` attr, enable pointer-events, attach click
            // handler that sends the checkbox index back via `dioxus.send(i)`.
            // Each handler calls `e.preventDefault()` so the native checkbox
            // state doesn't flip — our Rust side owns the source of truth.
            let script = r#"
                (function() {
                    var cbs = document.querySelectorAll('.md-doc input[type="checkbox"]');
                    for (var i = 0; i < cbs.length; i++) {
                        (function(cb, idx) {
                            cb.disabled = false;
                            cb.style.pointerEvents = 'auto';
                            cb.style.cursor = 'pointer';
                            cb.addEventListener('click', function(e) {
                                e.preventDefault();
                                dioxus.send(idx);
                            });
                        })(cbs[i], i);
                    }
                })();
            "#;

            let mut eval = document::eval(script);
            // Clone the sender so the spawned async block owns it independently
            // (use_effect's FnMut closure runs on every re-render; db_tx must
            // survive across invocations).
            let db_tx = engine_cb.db_tx.clone();

            spawn(async move {
                // Drive the recv loop.  Each `dioxus.send(idx)` from JS arrives
                // here as a usize.  On EvalError (DOM rebuilt / handle dropped /
                // newer generation) we stop.
                loop {
                    // Check generation — bail if a newer bridge has taken over.
                    if *bridge_gen.peek() != gen {
                        break;
                    }

                    match eval.recv::<usize>().await {
                        Ok(idx) => {
                            // Generation check again after the await.
                            if *bridge_gen.peek() != gen {
                                break;
                            }
                            let current = doc.peek().clone();
                            if let Some(updated) = toggle_nth_task(&current, idx) {
                                doc.set(updated.clone());
                                // SaveOverviewDoc does NOT snapshot overview_doc_prev
                                // (it's a user edit — matches existing save semantics).
                                let _ = db_tx.send(DbCmd::SaveOverviewDoc(updated));
                            }
                            // If None: stale index (doc changed) — ignore.
                        }
                        Err(_) => {
                            // Eval handle finished (DOM rebuilt or bridge superseded).
                            break;
                        }
                    }
                }
            });
        });
    }

    rsx! {
        div { class: "overview-doc-view",
            // ── Toolbar ──────────────────────────────────────────────────────
            div { class: "overview-doc-toolbar",
                h2 { "Overview" }
                div { class: "overview-doc-actions",
                    // Updated stamp
                    if updated_at > 0 {
                        span { class: "overview-doc-stamp", "Updated {fmt_date(updated_at)}" }
                    }

                    // "Update now" — only when LLM features compiled in.
                    if cfg!(any(feature = "llm-local", feature = "llm-remote")) {
                        {
                            let engine = engine.clone();
                            rsx! {
                                button {
                                    class: "mbtn",
                                    title: "Fold any un-folded meetings into the overview (runs in the background)",
                                    onclick: move |_| {
                                        let _ = engine.summ_tx.send(SummCmd::UpdateOverviewDoc { session: None });
                                    },
                                    {icon("sparkles")} "Update now"
                                }
                            }
                        }
                    }

                    // "Revert last AI update" — only when LLM features compiled in.
                    if cfg!(any(feature = "llm-local", feature = "llm-remote")) {
                        if confirm_revert() {
                            span { class: "overview-doc-confirm",
                                "Restore the document as it was before the last AI update?"
                                {
                                    let engine = engine.clone();
                                    rsx! {
                                        button {
                                            class: "mbtn danger",
                                            onclick: move |_| {
                                                confirm_revert.set(false);
                                                let _ = engine.db_tx.send(DbCmd::RevertOverviewDoc);
                                            },
                                            "Revert"
                                        }
                                    }
                                }
                                button {
                                    class: "mbtn ghost",
                                    onclick: move |_| confirm_revert.set(false),
                                    "Cancel"
                                }
                            }
                        } else {
                            button {
                                class: "mbtn ghost",
                                title: "Restore the document to how it was before the last AI edit",
                                onclick: move |_| confirm_revert.set(true),
                                {icon("refresh")} "Revert"
                            }
                        }
                    }

                    // Edit / View toggle
                    if is_editing {
                        // Save button
                        {
                            let engine = engine.clone();
                            rsx! {
                                button {
                                    class: "mbtn",
                                    onclick: move |_| {
                                        let text = draft.peek().clone();
                                        let _ = engine.db_tx.send(DbCmd::SaveOverviewDoc(text));
                                        editing.set(false);
                                    },
                                    {icon("check")} "Save"
                                }
                            }
                        }
                        button {
                            class: "mbtn ghost",
                            onclick: move |_| editing.set(false),
                            "Cancel"
                        }
                    } else {
                        button {
                            class: "mbtn ghost",
                            onclick: move |_| {
                                // Seed the draft from the current doc on open.
                                draft.set(doc.peek().clone());
                                editing.set(true);
                            },
                            {icon("pen")} "Edit"
                        }
                    }
                }
            }

            // ── Body ─────────────────────────────────────────────────────────
            if is_editing {
                // Full-height raw editor.
                textarea {
                    class: "overview-doc-editor",
                    value: "{draft}",
                    spellcheck: false,
                    oninput: move |e| draft.set(e.value()),
                }
            } else if is_empty {
                // Empty state.
                div { class: "empty",
                    "Record a meeting — the overview writes itself. Or press Edit and start typing."
                }
            } else {
                // Rendered markdown.
                div { class: "md-doc", dangerous_inner_html: "{html}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{md_to_html, toggle_nth_task};

    #[test]
    fn md_to_html_escapes_raw_html() {
        let out =
            md_to_html("hello <script>alert(1)</script> world\n\n<img src=x onerror=alert(2)>");
        assert!(
            !out.contains("<script>"),
            "raw <script> must be escaped, got: {out}"
        );
        assert!(
            !out.contains("<img"),
            "raw <img> must be escaped, got: {out}"
        );
        // The text content survives, escaped.
        assert!(
            out.contains("&lt;script&gt;"),
            "escaped form expected, got: {out}"
        );
    }

    #[test]
    fn md_to_html_renders_tasklists_and_tables() {
        let out = md_to_html("- [x] done\n- [ ] open\n\n|a|b|\n|-|-|\n|1|2|");
        assert!(
            out.contains("checkbox"),
            "tasklist checkboxes expected, got: {out}"
        );
        assert!(
            out.contains("<table>"),
            "table rendering expected, got: {out}"
        );
    }

    // ── toggle_nth_task ────────────────────────────────────────────────────

    #[test]
    fn toggle_unchecked_to_checked() {
        let md = "- [ ] task one\n- [ ] task two\n";
        let out = toggle_nth_task(md, 0).expect("index 0 should exist");
        assert!(out.starts_with("- [x] task one\n"), "got: {out}");
        assert!(out.contains("- [ ] task two\n"), "got: {out}");
    }

    #[test]
    fn toggle_checked_to_unchecked() {
        let md = "- [x] done\n- [ ] open\n";
        let out = toggle_nth_task(md, 0).expect("index 0 should exist");
        assert!(out.starts_with("- [ ] done\n"), "got: {out}");
    }

    #[test]
    fn toggle_nth_index() {
        let md = "- [ ] a\n- [ ] b\n- [x] c\n- [ ] d\n";
        let out = toggle_nth_task(md, 2).expect("index 2 should exist");
        assert_eq!(out, "- [ ] a\n- [ ] b\n- [ ] c\n- [ ] d\n", "got: {out}");
    }

    #[test]
    fn toggle_out_of_range_returns_none() {
        let md = "- [ ] only one\n";
        assert!(toggle_nth_task(md, 1).is_none());
        assert!(toggle_nth_task(md, 99).is_none());
        assert!(toggle_nth_task("", 0).is_none());
    }

    #[test]
    fn toggle_star_and_plus_bullets() {
        let md = "* [ ] star\n+ [X] plus\n";
        let out0 = toggle_nth_task(md, 0).expect("star bullet");
        assert!(out0.contains("* [x] star\n"), "got: {out0}");
        let out1 = toggle_nth_task(md, 1).expect("plus bullet");
        assert!(out1.contains("+ [ ] plus\n"), "got: {out1}");
    }

    #[test]
    fn toggle_preserves_all_other_bytes() {
        let md = "# Header\n\nSome prose.\n\n- [ ] task\n\nMore text.\n";
        let out = toggle_nth_task(md, 0).expect("index 0");
        assert!(
            out.starts_with("# Header\n\nSome prose.\n\n- [x] task\n\nMore text.\n"),
            "got: {out}"
        );
        // Byte length only changes by 0 (space → x, same byte count).
        assert_eq!(out.len(), md.len());
    }

    #[test]
    fn toggle_skips_checkboxes_in_fenced_code_blocks() {
        let md = "- [ ] real task 0\n\n```\n- [ ] fake inside fence\n```\n\n- [ ] real task 1\n";
        // Only 2 real tasks (indices 0 and 1); the fenced one is invisible.
        let out0 = toggle_nth_task(md, 0).expect("real task 0");
        assert!(out0.contains("- [x] real task 0\n"), "got: {out0}");
        assert!(
            out0.contains("- [ ] fake inside fence"),
            "fence must be untouched, got: {out0}"
        );

        let out1 = toggle_nth_task(md, 1).expect("real task 1");
        assert!(out1.contains("- [x] real task 1\n"), "got: {out1}");

        // Index 2 is out of range.
        assert!(toggle_nth_task(md, 2).is_none());
    }

    #[test]
    fn toggle_case_insensitive_x() {
        let md = "- [X] upper\n- [x] lower\n";
        let out0 = toggle_nth_task(md, 0).expect("upper X");
        assert!(out0.contains("- [ ] upper\n"), "got: {out0}");
        let out1 = toggle_nth_task(md, 1).expect("lower x");
        assert!(out1.contains("- [ ] lower\n"), "got: {out1}");
    }

    #[test]
    fn toggle_mixed_content_correct_indices() {
        let md = concat!(
            "# Goals\n",
            "\n",
            "- [ ] item 0\n",
            "- not a task\n",
            "- [x] item 1\n",
            "\n",
            "Paragraph.\n",
            "\n",
            "1. not a task either\n",
            "- [ ] item 2\n",
        );
        assert!(toggle_nth_task(md, 0).unwrap().contains("- [x] item 0\n"));
        assert!(toggle_nth_task(md, 1).unwrap().contains("- [ ] item 1\n"));
        assert!(toggle_nth_task(md, 2).unwrap().contains("- [x] item 2\n"));
        assert!(toggle_nth_task(md, 3).is_none());
    }
}
