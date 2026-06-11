//! Phase 39 — the living **Overview** document.
//!
//! One markdown status document, organized by project, maintained by the AI
//! from compressed transcripts ([`update_document`]) and editable by the user
//! at any time. The expensive work (LLM) is behind the `llama`/`remote`
//! backend features; [`load`] (reading the legacy Phase 23 rollup) works
//! without either.
//!
//! [`cross_meeting_context`] (compression-based grounding) remains as the
//! cross-meeting chat fallback for when the document is still empty.

#[cfg(any(feature = "llama", feature = "remote"))]
use anyhow::Result;
#[cfg(any(feature = "llama", feature = "remote"))]
use zord_config::Settings;
#[cfg(any(feature = "llama", feature = "remote"))]
use zord_store::Store;

// ---------------------------------------------------------------------------
// Cross-meeting chat grounding (compression-based fallback)
// ---------------------------------------------------------------------------

/// Build the grounding context for **cross-meeting chat** (Phase 23d): gather the
/// recent meetings' compressions (lazily generating any missing, with the given
/// loaded model) and fit them to `n_ctx`. Returns `(context, meetings_covered)`.
#[cfg(any(feature = "llama", feature = "remote"))]
pub fn cross_meeting_context(
    store: &Store,
    llm: &zord_summarize::LlmBackend,
    settings: &Settings,
    n_ctx: u32,
    progress: &mut dyn FnMut(&str),
) -> Result<(String, usize)> {
    let digests = collect_digests(store, llm, settings, progress)?;
    if digests.is_empty() {
        anyhow::bail!("no meetings with transcripts yet");
    }
    let meetings = digests.len();
    let context = fit_to_budget(
        llm,
        &digests,
        n_ctx.clamp(8192, 131_072),
        768,
        settings,
        progress,
    )?;
    Ok((context, meetings))
}

// ---------------------------------------------------------------------------
// Phase 39 — living-document update
// ---------------------------------------------------------------------------

/// Build the user-turn text that the LLM receives for a document update: the
/// current document (or a placeholder when it is empty), a separator, and the
/// new meeting's input.
///
/// This is a **pure** function so it can be unit-tested without an LLM.
pub fn build_update_input(doc: &str, session_input: &str, session_label: &str) -> String {
    let doc_part = if doc.trim().is_empty() {
        "(empty document)".to_string()
    } else {
        doc.to_string()
    };
    format!("{doc_part}\n\n--- New meeting ({session_label}) ---\n\n{session_input}")
}

/// Strip a wrapping ```` ```markdown … ``` ```` or ```` ``` … ``` ```` code
/// fence that a model may add around its output despite instructions.
///
/// Only the outermost fence is stripped; interior fences (e.g. inside a code
/// block in the document) are left intact. This is a **pure** function.
pub fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();

    // Accept ``` optionally followed by "markdown".
    let after_open = if let Some(rest) = s.strip_prefix("```markdown") {
        rest
    } else if let Some(rest) = s.strip_prefix("```") {
        rest
    } else {
        return s;
    };

    // The opening fence must be followed by a newline before the content starts.
    let body = if let Some(body) = after_open.strip_prefix('\n') {
        body
    } else if let Some(body) = after_open.strip_prefix("\r\n") {
        body
    } else {
        // No newline after the fence — not a real fence block; return original.
        return s;
    };

    // Strip the closing ``` (trimming any trailing whitespace/newlines first).
    if let Some(stripped) = body.trim_end().strip_suffix("```") {
        stripped.trim_end()
    } else {
        s
    }
}

/// One fold of the living overview (Phase 39): rewrite the whole document with
/// `session_input` (the session's compressed transcript, else its labeled
/// transcript) merged in. Returns the full replacement markdown.
///
/// Pure LLM wrapper — fold bookkeeping (which sessions, snapshots, the
/// high-water mark) lives in the engine.
///
/// `session_label` is a human-readable label for the meeting, e.g.
/// `"2026-06-11 — Standup"`, used by the model to date Archive entries.
///
/// Context budget: the document is preserved whole; if `session_input` would
/// push the combined input over the window, `session_input` is truncated (not
/// the document — the document is the state being maintained).
#[cfg(any(feature = "llama", feature = "remote"))]
pub fn update_document(
    doc: &str,
    session_input: &str,
    session_label: &str,
    llm: &zord_summarize::LlmBackend,
    settings: &Settings,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    use zord_summarize::GenOpts;

    let n_ctx = settings.overview_ctx.clamp(8192, 131_072);
    let opts = GenOpts::overview(n_ctx);

    // Reserve space for: the system prompt + chat template overhead (600 t),
    // the document itself, and the separator / label text (~50 chars).
    // Whatever remains is the budget for session_input.
    //
    // `opts.max_transcript_chars` is unused here; we compute our own per-field
    // budget so the doc is never truncated (only session_input is).
    // `input_budget` subtracts `opts.max_new_tokens + 600` from n_ctx
    // independently of GenOpts' internal reserve.
    let total_budget = input_budget(n_ctx, opts.max_new_tokens);
    let sep_overhead = session_label.len() + 60; // "--- New meeting (…) ---\n\n" etc.
    let doc_tokens = llm.count_tokens(doc)?;
    let session_budget_tokens = total_budget.saturating_sub(doc_tokens + sep_overhead);

    // Coarse character cap derived from the token budget (≈3.5 chars/token).
    let session_budget_chars = session_budget_tokens.saturating_mul(7) / 2;
    let session_input =
        if session_input.chars().count() > session_budget_chars && session_budget_chars > 0 {
            progress(&format!(
            "Session input too long for context — truncating to ~{session_budget_chars} characters."
        ));
            let truncated: String = session_input.chars().take(session_budget_chars).collect();
            truncated
        } else {
            session_input.to_string()
        };

    progress(&format!(
        "Updating the living overview with \"{session_label}\"…"
    ));

    let user_content = build_update_input(doc, &session_input, session_label);
    let raw = llm.generate(&user_content, zord_config::overview_doc_prompt(), opts)?;

    // Trim and strip any markdown code fence the model added despite instructions.
    let result = strip_code_fence(raw.trim()).to_string();
    Ok(result)
}

/// Gather up to `overview_max_meetings` recent meetings as `(header, compressed)`
/// pairs (newest first), reusing each stored compression and lazily generating +
/// persisting any that are missing.
#[cfg(any(feature = "llama", feature = "remote"))]
fn collect_digests(
    store: &Store,
    llm: &zord_summarize::LlmBackend,
    settings: &Settings,
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<(String, String)>> {
    let max = settings.overview_max_meetings.max(1) as usize;
    let mut digests: Vec<(String, String)> = Vec::new();
    let mut generated = 0usize;
    for s in store.list_sessions()? {
        if digests.len() >= max {
            break;
        }
        let compressed = match store.get_compressed(&s.id)? {
            Some(c) if !c.trim().is_empty() => c,
            _ => {
                let segs = store.segments(&s.id)?;
                if segs.is_empty() {
                    continue; // nothing recorded — skip
                }
                generated += 1;
                progress(&format!(
                    "Compressing meeting {} ({})…",
                    generated,
                    meeting_title(&s)
                ));
                let names = store.speaker_names(&s.id).unwrap_or_default();
                let transcript = build_transcript(&segs, &names);
                let c = llm.compress(
                    &transcript,
                    zord_config::compress_prompt(),
                    settings.compress_ctx,
                )?;
                store.set_compressed(&s.id, &c)?;
                c
            }
        };
        digests.push((meeting_header(&s), compressed));
    }
    Ok(digests)
}

/// Assemble `digests` and, if over the `n_ctx` input budget (after reserving
/// `reserve_out` for generation), condense them hierarchically until they fit.
#[cfg(any(feature = "llama", feature = "remote"))]
fn fit_to_budget(
    llm: &zord_summarize::LlmBackend,
    digests: &[(String, String)],
    n_ctx: u32,
    reserve_out: usize,
    settings: &Settings,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    let budget = input_budget(n_ctx, reserve_out);
    let assembled = assemble(digests);
    let tokens = llm.count_tokens(&assembled)?;
    if tokens <= budget {
        return Ok(assembled);
    }
    progress(&format!(
        "{} meetings (~{} tokens) exceed the {}-token context — condensing in groups…",
        digests.len(),
        tokens,
        n_ctx
    ));
    hierarchical_reduce(llm, digests, budget, settings, progress)
}

/// Greedily pack the digests (newest first) into groups, condense each group
/// into a single dense digest, then assemble those. Falls back to a recency trim
/// if the condensed digests are *still* over budget.
#[cfg(any(feature = "llama", feature = "remote"))]
fn hierarchical_reduce(
    llm: &zord_summarize::LlmBackend,
    digests: &[(String, String)],
    budget: usize,
    settings: &Settings,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    let group_budget = input_budget(settings.compress_ctx.clamp(8192, 131_072), 1024);
    let groups = pack(llm, digests, group_budget)?;
    let mut reduced: Vec<(String, String)> = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        progress(&format!("Condensing group {}/{}…", i + 1, groups.len()));
        let assembled = assemble(group);
        let digest = llm.compress(
            &assembled,
            zord_config::compress_prompt(),
            settings.compress_ctx,
        )?;
        reduced.push((
            format!("Group {} — {} meetings", i + 1, group.len()),
            digest,
        ));
    }

    let assembled = assemble(&reduced);
    if llm.count_tokens(&assembled)? <= budget {
        return Ok(assembled);
    }

    // Still too large: include the most-recent groups that fit and say so.
    let (kept, text) = take_within_budget(llm, &reduced, budget)?;
    progress(&format!(
        "Still over budget — overview covers the {} most recent groups ({} dropped).",
        kept,
        reduced.len() - kept
    ));
    Ok(text)
}

/// Pack items (already newest-first) into groups whose assembled token count
/// stays under `budget`. An oversized single item becomes its own group.
#[cfg(any(feature = "llama", feature = "remote"))]
fn pack(
    llm: &zord_summarize::LlmBackend,
    items: &[(String, String)],
    budget: usize,
) -> Result<Vec<Vec<(String, String)>>> {
    let mut groups: Vec<Vec<(String, String)>> = Vec::new();
    let mut cur: Vec<(String, String)> = Vec::new();
    let mut cur_tokens = 0usize;
    for item in items {
        let t = llm.count_tokens(&assemble(std::slice::from_ref(item)))? + 4;
        if !cur.is_empty() && cur_tokens + t > budget {
            groups.push(std::mem::take(&mut cur));
            cur_tokens = 0;
        }
        cur.push(item.clone());
        cur_tokens += t;
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    Ok(groups)
}

/// Include items (newest-first) until the next would exceed `budget`. Returns
/// (count_kept, assembled_text).
#[cfg(any(feature = "llama", feature = "remote"))]
fn take_within_budget(
    llm: &zord_summarize::LlmBackend,
    items: &[(String, String)],
    budget: usize,
) -> Result<(usize, String)> {
    let mut kept: Vec<(String, String)> = Vec::new();
    let mut tokens = 0usize;
    for item in items {
        let t = llm.count_tokens(&assemble(std::slice::from_ref(item)))? + 4;
        if !kept.is_empty() && tokens + t > budget {
            break;
        }
        tokens += t;
        kept.push(item.clone());
    }
    Ok((kept.len(), assemble(&kept)))
}

/// Tokens available for input = context minus the output reservation minus a
/// fixed allowance for the system prompt + chat template.
#[cfg(any(feature = "llama", feature = "remote"))]
fn input_budget(n_ctx: u32, reserve_out: usize) -> usize {
    (n_ctx as usize).saturating_sub(reserve_out + 600)
}

/// Render segments as newline-joined `speaker: text` lines.
#[cfg(any(feature = "llama", feature = "remote"))]
fn build_transcript(
    segs: &[zord_core::Segment],
    names: &std::collections::HashMap<i32, String>,
) -> String {
    segs.iter()
        .map(|seg| format!("{}: {}", seg.speaker_label(names), seg.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Assemble `(header, body)` pairs into the model-facing input block.
#[cfg(any(feature = "llama", feature = "remote"))]
fn assemble(items: &[(String, String)]) -> String {
    items
        .iter()
        .map(|(h, body)| format!("[{h}]\n{body}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// "YYYY-MM-DD · <title>" header for a meeting.
#[cfg(any(feature = "llama", feature = "remote"))]
fn meeting_header(s: &zord_core::Session) -> String {
    format!("{} · {}", fmt_date(s.started_at), meeting_title(s))
}

#[cfg(any(feature = "llama", feature = "remote"))]
fn meeting_title(s: &zord_core::Session) -> String {
    s.title
        .as_ref()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .unwrap_or_else(|| "Untitled recording".to_string())
}

/// Format an epoch-ms timestamp as a UTC `YYYY-MM-DD` date (no chrono dep).
#[cfg(any(feature = "llama", feature = "remote"))]
fn fmt_date(ms: u64) -> String {
    let (y, m, d) = civil_from_days((ms / 86_400_000) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days-since-Unix-epoch → (year, month, day). Howard Hinnant's `civil_from_days`.
#[cfg(any(feature = "llama", feature = "remote"))]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(test)]
mod update_document_tests {
    use super::*;

    // ---- build_update_input ------------------------------------------------

    #[test]
    fn empty_doc_uses_placeholder() {
        let out = build_update_input("", "Me: hey", "2026-06-11 — Standup");
        assert!(
            out.starts_with("(empty document)"),
            "empty doc must produce placeholder, got: {out:?}"
        );
    }

    #[test]
    fn whitespace_only_doc_uses_placeholder() {
        let out = build_update_input("   \n\t  ", "Me: hey", "2026-06-11 — Standup");
        assert!(out.starts_with("(empty document)"));
    }

    #[test]
    fn non_empty_doc_is_preserved_verbatim() {
        let doc = "## CI/CD\n- [ ] finish pipeline";
        let out = build_update_input(doc, "Me: done", "2026-06-11");
        assert!(out.starts_with(doc), "doc must appear first; got: {out:?}");
    }

    #[test]
    fn separator_contains_session_label() {
        let label = "2026-06-11 — Engineering sync";
        let out = build_update_input("some doc", "input text", label);
        assert!(
            out.contains(label),
            "separator must contain the session label; got: {out:?}"
        );
        assert!(
            out.contains("--- New meeting ("),
            "separator must be present; got: {out:?}"
        );
    }

    #[test]
    fn doc_appears_before_session_input() {
        let doc = "## Project A\nsome content";
        let session = "Me: project A is done";
        let out = build_update_input(doc, session, "2026-06-11");
        let doc_pos = out.find(doc).expect("doc must be in output");
        let session_pos = out.find(session).expect("session input must be in output");
        assert!(
            doc_pos < session_pos,
            "doc must appear before session input; doc_pos={doc_pos} session_pos={session_pos}"
        );
    }

    #[test]
    fn separator_between_doc_and_input() {
        let doc = "## A\nstuff";
        let session = "Me: update";
        let label = "2026-06-11 — Standup";
        let out = build_update_input(doc, session, label);
        // Between them there must be the separator line.
        let sep = format!("--- New meeting ({label}) ---");
        let doc_end = out.find(doc).unwrap() + doc.len();
        let sep_pos = out.find(&sep).unwrap();
        let session_pos = out.find(session).unwrap();
        assert!(doc_end < sep_pos, "separator must come after doc");
        assert!(
            sep_pos < session_pos,
            "session input must come after separator"
        );
    }

    // ---- strip_code_fence --------------------------------------------------

    #[test]
    fn strips_markdown_fence() {
        let input = "```markdown\n## Status\ncontent\n```";
        assert_eq!(strip_code_fence(input), "## Status\ncontent");
    }

    #[test]
    fn strips_plain_fence() {
        let input = "```\n## Status\ncontent\n```";
        assert_eq!(strip_code_fence(input), "## Status\ncontent");
    }

    #[test]
    fn no_fence_unchanged() {
        let input = "## Status\ncontent";
        assert_eq!(strip_code_fence(input), input);
    }

    #[test]
    fn fence_without_closing_unchanged() {
        // Opening fence but no closing — don't mangle it.
        let input = "```markdown\n## Status\ncontent";
        assert_eq!(strip_code_fence(input), input);
    }

    #[test]
    fn fence_without_newline_after_open_unchanged() {
        // ``` followed immediately by content (no newline) — not a real fence block.
        let input = "```## Status\ncontent\n```";
        assert_eq!(strip_code_fence(input), input);
    }

    #[test]
    fn strips_with_trailing_whitespace_on_content() {
        let input = "```markdown\n## H\ntext\n```\n\n";
        assert_eq!(strip_code_fence(input), "## H\ntext");
    }

    #[test]
    fn interior_fences_preserved() {
        // A document that legitimately contains a code block inside — only the
        // outer fence (if any) should be stripped.
        let inner = "## Doc\n```rust\nlet x = 1;\n```\nend";
        // Without an outer fence: unchanged.
        assert_eq!(strip_code_fence(inner), inner);
        // With an outer markdown fence: outer stripped, inner preserved.
        let wrapped = format!("```markdown\n{inner}\n```");
        assert_eq!(strip_code_fence(&wrapped), inner);
    }
}
