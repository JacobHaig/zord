//! Phase 23b — cross-meeting **Overview** synthesis.
//!
//! Turns the per-meeting dense compressions (Phase 23a) into one holistic,
//! project-grouped rollup oriented around the user ("Me"). The expensive work
//! (LLM) is behind the `llama`/`remote` backend features; [`load`] (reading a
//! stored rollup) works without either.
//!
//! Flow: gather the most recent meetings → reuse each meeting's stored
//! compression, lazily generating + persisting any that are missing → assemble
//! them → if they fit the configured context, synthesize in one pass; otherwise
//! condense in groups first (hierarchical fallback) → store + return the rollup.

pub mod extract;
pub use extract::{ExtractedItem, ExtractedProject, ResolvedMention, SessionExtract};

use anyhow::Result;
#[cfg(any(feature = "llama", feature = "remote"))]
use std::path::Path;
#[cfg(any(feature = "llama", feature = "remote"))]
use zord_config::Settings;
use zord_store::Store;

/// `app_meta` key holding the synthesized Overview Markdown.
pub const META_OVERVIEW: &str = "overview";
/// `app_meta` key holding how many meetings the stored Overview covered.
pub const META_OVERVIEW_MEETINGS: &str = "overview_meetings";

/// A synthesized (or loaded) cross-meeting Overview.
#[derive(Debug, Clone)]
pub struct Overview {
    /// Markdown rollup.
    pub text: String,
    /// How many meetings it covered.
    pub meetings: usize,
    /// When it was generated (epoch ms).
    pub generated_at_ms: u64,
}

/// Load the most recently stored Overview, if any (no LLM needed).
pub fn load(store: &Store) -> Result<Option<Overview>> {
    let Some((text, generated_at_ms)) = store.get_meta(META_OVERVIEW)? else {
        return Ok(None);
    };
    let meetings = store
        .get_meta(META_OVERVIEW_MEETINGS)?
        .and_then(|(v, _)| v.parse().ok())
        .unwrap_or(0);
    Ok(Some(Overview { text, meetings, generated_at_ms }))
}

// ---------------------------------------------------------------------------
// Synthesis (needs the local LLM)
// ---------------------------------------------------------------------------

/// Synthesize the Overview with an already-built LLM backend (Phase 24: the
/// caller picks local GGUF vs external server and keeps it for the whole run —
/// lazy compressions + the final synthesis pass).
#[cfg(any(feature = "llama", feature = "remote"))]
pub fn synthesize(
    db_path: &Path,
    settings: &Settings,
    llm: &zord_summarize::LlmBackend,
    progress: &mut dyn FnMut(&str),
) -> Result<Overview> {
    use zord_summarize::GenOpts;

    let store = Store::open(db_path)?;

    // Gather the most recent meetings (lazily compressing any missing), then fit
    // them to the synthesis context (condensing in groups if they overflow).
    let digests = collect_digests(&store, llm, settings, progress)?;
    if digests.is_empty() {
        anyhow::bail!("no meetings with transcripts to synthesize yet");
    }
    let n_ctx = settings.overview_ctx.clamp(8192, 131_072);
    let input = fit_to_budget(llm, &digests, n_ctx, 2048, settings, progress)?;

    progress("Synthesizing the cross-meeting overview…");
    let text = llm.generate(&input, zord_config::overview_prompt(), GenOpts::overview(n_ctx))?;

    store.set_meta(META_OVERVIEW, &text)?;
    store.set_meta(META_OVERVIEW_MEETINGS, &digests.len().to_string())?;
    let generated_at_ms = store
        .get_meta(META_OVERVIEW)?
        .map(|(_, t)| t)
        .unwrap_or(0);
    progress(&format!("Overview ready — {} meetings.", digests.len()));
    Ok(Overview { text, meetings: digests.len(), generated_at_ms })
}

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
    let context = fit_to_budget(llm, &digests, n_ctx.clamp(8192, 131_072), 768, settings, progress)?;
    Ok((context, meetings))
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
                progress(&format!("Compressing meeting {} ({})…", generated, meeting_title(&s)));
                let names = store.speaker_names(&s.id).unwrap_or_default();
                let transcript = build_transcript(&segs, &names);
                let c = llm.compress(&transcript, zord_config::compress_prompt(), settings.compress_ctx)?;
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
        reduced.push((format!("Group {} — {} meetings", i + 1, group.len()), digest));
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
    segs
        .iter()
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
