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

/// The pulldown-cmark options shared by [`md_to_html`] and
/// [`toggle_nth_task`].  They MUST stay identical: the toggle's checkbox
/// indexing contract is "the Nth `TaskListMarker` event equals the Nth
/// rendered `<input>`", which only holds when both sides parse with the same
/// options.
fn md_options() -> pulldown_cmark::Options {
    use pulldown_cmark::Options;
    Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH
}

/// Toggle the Nth task-list checkbox in markdown source order.
///
/// Returns `None` when `n` is out of range (doc changed under us — caller
/// drops the click).
///
/// Uses pulldown-cmark itself (`into_offset_iter`) to locate the Nth
/// `Event::TaskListMarker`, so agreement with the renderer is by
/// construction: ordered-list tasks (`1. [ ]`), blockquoted tasks,
/// multi-space bullets, etc. all count exactly when they render as
/// checkboxes, and fenced/indented code blocks never do.  The marker's byte
/// `Range` spans the `[x]`-style marker in the SOURCE; the state char sits
/// right after the `[` within it, and ` `/`x`/`X` are all 1 byte, so the
/// flip preserves every other byte exactly.
pub fn toggle_nth_task(md: &str, n: usize) -> Option<String> {
    use pulldown_cmark::{Event, Parser};

    // Nth task-list marker in source order (same order pulldown renders).
    let range = Parser::new_ext(md, md_options())
        .into_offset_iter()
        .filter_map(|(ev, r)| matches!(ev, Event::TaskListMarker(_)).then_some(r))
        .nth(n)?;

    // The range covers the `[ ]`/`[x]`/`[X]` marker. Locate the `[` within it
    // (robust even if a pulldown version widens the span) and flip the state
    // char that follows.
    let bracket = md[range.clone()].find('[')? + range.start;
    let state_pos = bracket + 1;
    let flipped = match md.as_bytes().get(state_pos)? {
        b' ' => "x",
        b'x' | b'X' => " ",
        _ => return None,
    };

    let mut out = String::with_capacity(md.len());
    out.push_str(&md[..state_pos]);
    out.push_str(flipped);
    out.push_str(&md[state_pos + 1..]);
    Some(out)
}

/// Render the markdown string `md` to HTML using pulldown-cmark.
///
/// Raw HTML is neutralized: pulldown-cmark would otherwise pass
/// `Event::Html`/`Event::InlineHtml` through verbatim, and the result lands in
/// `dangerous_inner_html` — the document is LLM/user-authored, so a literal
/// `<script>`/`<img onerror>` would execute in the webview. Mapping those
/// events to `Event::Text` makes push_html escape them instead.
fn md_to_html(md: &str) -> String {
    use pulldown_cmark::{html, Event, Parser};
    let parser = Parser::new_ext(md, md_options()).map(|e| match e {
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
                // Rendered markdown. The OUTER div scrolls at full width (so
                // the scrollbar sits at the window edge, not mid-screen); the
                // INNER div carries the readable measure, centered.
                div { class: "md-doc",
                    div { class: "md-doc-inner", dangerous_inner_html: "{html}" }
                }
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

    #[test]
    fn toggle_ordered_list_tasks() {
        // Numbered task lists render as checkboxes too (AI output uses these
        // constantly) — they must count.
        let md = "1. [ ] first\n2. [x] second\n";
        let out0 = toggle_nth_task(md, 0).expect("ordered task 0");
        assert!(out0.contains("1. [x] first\n"), "got: {out0}");
        let out1 = toggle_nth_task(md, 1).expect("ordered task 1");
        assert!(out1.contains("2. [ ] second\n"), "got: {out1}");
        assert!(toggle_nth_task(md, 2).is_none());
    }

    #[test]
    fn toggle_blockquoted_and_multispace_tasks() {
        let md = "> - [ ] quoted task\n\n-   [x] multi-space bullet\n";
        let out0 = toggle_nth_task(md, 0).expect("blockquoted task");
        assert!(out0.contains("> - [x] quoted task\n"), "got: {out0}");
        let out1 = toggle_nth_task(md, 1).expect("multi-space bullet task");
        assert!(out1.contains("-   [ ] multi-space bullet\n"), "got: {out1}");
    }

    #[test]
    fn toggle_skips_indented_code_blocks() {
        // 4-space-indented code is NOT rendered as a task by pulldown — it
        // must not count either.
        let md = "- [ ] real\n\n        - [ ] inside indented code\n";
        let out = toggle_nth_task(md, 0).expect("real task");
        assert!(out.contains("- [x] real\n"), "got: {out}");
        assert!(
            out.contains("        - [ ] inside indented code"),
            "indented code must be untouched, got: {out}"
        );
        assert!(toggle_nth_task(md, 1).is_none());
    }

    /// Extract the checked state of every rendered `<input>` in `html`,
    /// in document order.
    fn checked_states(html: &str) -> Vec<bool> {
        html.match_indices("<input")
            .map(|(i, _)| {
                let end = i + html[i..].find('>').expect("unclosed <input tag");
                html[i..end].contains("checked")
            })
            .collect()
    }

    /// Renderer-equivalence: for a gnarly document, the toggle's index space
    /// must equal the rendered checkbox index space — same count, and
    /// toggling index k flips exactly the k-th rendered checkbox.
    #[test]
    fn toggle_matches_rendered_checkboxes() {
        use pulldown_cmark::{Event, Parser};

        let md = concat!(
            "# Gnarly\n\n",
            "- [ ] plain task\n",
            "- plain non-task\n\n",
            "1. [x] numbered task\n",
            "2. [ ] another numbered\n\n",
            "> - [ ] blockquoted task\n\n",
            "-   [X] multi-space bullet\n\n",
            "```\n- [ ] fenced — not a task\n```\n\n",
            "    - [ ] indented code — not a task\n\n",
            "- [ ]no space after bracket — not a task\n",
            "- [ ] tail task\n",
        );

        // Count TaskListMarker events with the SAME options as the renderer.
        let marker_count = Parser::new_ext(md, super::md_options())
            .into_offset_iter()
            .filter(|(ev, _)| matches!(ev, Event::TaskListMarker(_)))
            .count();
        assert!(marker_count >= 6, "expected the gnarly tasks to all parse");

        // Same count as rendered <input> elements.
        let base_html = md_to_html(md);
        let base = checked_states(&base_html);
        assert_eq!(
            marker_count,
            base.len(),
            "marker count must equal rendered <input> count; html: {base_html}"
        );

        // Toggling index k flips exactly the k-th rendered checkbox.
        for k in 0..marker_count {
            let toggled = toggle_nth_task(md, k).expect("index in range");
            let states = checked_states(&md_to_html(&toggled));
            assert_eq!(states.len(), base.len(), "toggle must not add/remove tasks");
            for (j, (before, after)) in base.iter().zip(states.iter()).enumerate() {
                if j == k {
                    assert_ne!(before, after, "checkbox {k} must flip");
                } else {
                    assert_eq!(
                        before, after,
                        "checkbox {j} must not change when toggling {k}"
                    );
                }
            }
        }

        // One past the end is out of range.
        assert!(toggle_nth_task(md, marker_count).is_none());
    }
}
