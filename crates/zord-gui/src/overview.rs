//! Living overview document view — Phase 39d.
//!
//! Replaces the Phase 26 project-ledger UI with a markdown document panel:
//! rendered view by default, raw textarea editor on toggle, and a toolbar with
//! Update / Revert / stamp controls (gated to LLM builds).

use dioxus::prelude::*;
use zord_config::Settings;

use crate::{
    engine::{DbCmd, Engine, SummCmd},
    fmt_date, icon,
};

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
    use super::md_to_html;

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
}
