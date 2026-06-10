//! Phase 26b — the structured per-meeting **extract**.
//!
//! One meeting in, one [`SessionExtract`] out: the projects it touched, the
//! trackable items it raised (actions / open questions / decisions), and the
//! prior threads it reported as resolved. This step is deliberately *stateless*
//! — it sees only this meeting. The merge engine (26c) reconciles the extract
//! against the running ledger (routing items to projects, matching `resolved`
//! mentions to open items, marking completions with provenance).
//!
//! The extract is persisted verbatim in `session_overview_state.extract`, so a
//! re-transcribed or edited meeting can be re-folded without re-running the LLM.
//!
//! The LLM is asked for strict JSON (see [`zord_config::extract_prompt`]). Models
//! don't always comply perfectly, so [`parse_extract`] is lenient: it digs the
//! first balanced JSON object out of any surrounding prose and sanitizes the
//! result. Parsing is backend-free, so it unit-tests without a model.

use serde::{Deserialize, Serialize};

/// A project/topic this meeting touched, with a one-line state summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedProject {
    pub name: String,
    #[serde(default)]
    pub summary: String,
}

/// One trackable item pulled from a meeting, before routing/merge. `kind` is
/// kept as the raw string ("action" / "question" / "decision") and converted to
/// [`zord_core::ItemKind`] by the merge engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedItem {
    /// Project name hint — should match one of the extract's `projects`.
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub owner: Option<String>,
    /// Reported as already completed *in this meeting* (a decision, or a task
    /// finished on the spot). Distinct from `resolved`, which closes prior work.
    #[serde(default)]
    pub done: bool,
}

/// A thread this meeting reported as finished/answered that was likely opened in
/// an earlier meeting — a short description the reconciler matches to an existing
/// open item (to mark it done with provenance) rather than a new item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedMention {
    #[serde(default)]
    pub project: String,
    pub text: String,
}

/// The structured delta extracted from a single meeting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionExtract {
    #[serde(default)]
    pub projects: Vec<ExtractedProject>,
    #[serde(default)]
    pub items: Vec<ExtractedItem>,
    #[serde(default)]
    pub resolved: Vec<ResolvedMention>,
}

impl SessionExtract {
    /// Nothing trackable was found (the meeting can still be marked applied so
    /// it isn't re-extracted).
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty() && self.items.is_empty() && self.resolved.is_empty()
    }
}

/// Run the structured extract over one meeting's transcript (or dense
/// compression) using the chosen backend.
#[cfg(any(feature = "llama", feature = "remote"))]
pub fn extract_session(
    llm: &zord_summarize::LlmBackend,
    transcript: &str,
    n_ctx: u32,
) -> anyhow::Result<SessionExtract> {
    use zord_summarize::GenOpts;
    let user = format!("Meeting:\n\n{transcript}");
    let raw = llm.generate(
        &user,
        zord_config::extract_prompt(),
        GenOpts::overview(n_ctx),
    )?;
    Ok(parse_extract(&raw))
}

/// Parse an LLM reply into a [`SessionExtract`], tolerating prose/fences around
/// the JSON. Returns an empty extract (never an error) if no object is found —
/// a malformed extract should not abort a whole fold run.
pub fn parse_extract(raw: &str) -> SessionExtract {
    let Some(json) = first_json_object(raw) else {
        tracing::warn!(
            "extract: no JSON object in model reply ({} bytes)",
            raw.len()
        );
        return SessionExtract::default();
    };
    match serde_json::from_str::<SessionExtract>(json) {
        Ok(ex) => sanitize(ex),
        Err(e) => {
            tracing::warn!("extract: JSON parse failed: {e}");
            SessionExtract::default()
        }
    }
}

/// Trim whitespace, drop empty/garbage entries, and normalize `kind` to one of
/// the three known values (anything unrecognized becomes "action").
fn sanitize(mut ex: SessionExtract) -> SessionExtract {
    ex.projects.retain_mut(|p| {
        p.name = p.name.trim().to_string();
        p.summary = p.summary.trim().to_string();
        !p.name.is_empty()
    });
    ex.items.retain_mut(|it| {
        it.project = it.project.trim().to_string();
        it.text = it.text.trim().to_string();
        it.kind = normalize_kind(&it.kind);
        it.owner = it.owner.take().map(|o| o.trim().to_string()).filter(|o| {
            !o.is_empty() && !o.eq_ignore_ascii_case("null") && !o.eq_ignore_ascii_case("unknown")
        });
        !it.text.is_empty()
    });
    ex.resolved.retain_mut(|r| {
        r.project = r.project.trim().to_string();
        r.text = r.text.trim().to_string();
        !r.text.is_empty()
    });
    ex
}

fn normalize_kind(kind: &str) -> String {
    match kind.trim().to_ascii_lowercase().as_str() {
        "question" => "question",
        "decision" => "decision",
        _ => "action",
    }
    .to_string()
}

/// Return the slice of `s` covering the first complete, brace-balanced JSON
/// object (string-aware, so braces inside string literals don't count). `None`
/// if there's no balanced object. Shared with the reconcile parser.
pub(crate) fn first_json_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let c = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let raw = r#"{
            "projects": [{"name": "Billing migration", "summary": "porting to v2"}],
            "items": [
                {"project": "Billing migration", "kind": "action", "text": "Write the adapter", "owner": "Alex", "done": false},
                {"project": "Billing migration", "kind": "decision", "text": "Use the new API", "owner": null, "done": true}
            ],
            "resolved": [{"project": "Billing migration", "text": "auth question answered"}]
        }"#;
        let ex = parse_extract(raw);
        assert_eq!(ex.projects.len(), 1);
        assert_eq!(ex.items.len(), 2);
        assert_eq!(ex.items[0].owner.as_deref(), Some("Alex"));
        assert!(ex.items[1].owner.is_none());
        assert!(ex.items[1].done);
        assert_eq!(ex.resolved.len(), 1);
        assert!(!ex.is_empty());
    }

    #[test]
    fn digs_json_out_of_prose_and_fences() {
        let raw = "Sure! Here is the extract:\n```json\n{\"projects\":[],\"items\":[{\"project\":\"P\",\"kind\":\"action\",\"text\":\"do it\"}]}\n```\nHope that helps.";
        let ex = parse_extract(raw);
        assert_eq!(ex.items.len(), 1);
        assert_eq!(ex.items[0].text, "do it");
        // Missing fields default sensibly.
        assert!(!ex.items[0].done);
        assert_eq!(ex.items[0].kind, "action");
    }

    #[test]
    fn handles_braces_inside_strings() {
        let raw = r#"{"projects":[],"items":[{"project":"P","kind":"x","text":"use {curly} braces","owner":"","done":false}],"resolved":[]}"#;
        let ex = parse_extract(raw);
        assert_eq!(ex.items.len(), 1);
        assert_eq!(ex.items[0].text, "use {curly} braces");
        // Unknown kind normalized to action; blank owner dropped.
        assert_eq!(ex.items[0].kind, "action");
        assert!(ex.items[0].owner.is_none());
    }

    #[test]
    fn drops_empty_text_and_nameless_projects() {
        let raw = r#"{"projects":[{"name":"  ","summary":"x"},{"name":"Real","summary":""}],
                      "items":[{"project":"Real","kind":"action","text":"   "},{"project":"Real","kind":"question","text":"why?"}],
                      "resolved":[]}"#;
        let ex = parse_extract(raw);
        assert_eq!(ex.projects.len(), 1);
        assert_eq!(ex.projects[0].name, "Real");
        assert_eq!(ex.items.len(), 1);
        assert_eq!(ex.items[0].kind, "question");
    }

    #[test]
    fn empty_on_garbage() {
        assert!(parse_extract("no json here at all").is_empty());
        assert!(parse_extract("{ this is { not valid").is_empty());
    }

    #[test]
    fn owner_literal_null_string_dropped() {
        let raw = r#"{"items":[{"project":"P","kind":"action","text":"t","owner":"null"}]}"#;
        let ex = parse_extract(raw);
        assert!(ex.items[0].owner.is_none());
    }
}
