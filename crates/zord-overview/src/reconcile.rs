//! Phase 26c — the **routing + merge engine**: fold one meeting's
//! [`SessionExtract`](crate::SessionExtract) into the running ledger.
//!
//! Two halves, split so the risky bit and the durable bit can be reasoned about
//! (and tested) separately:
//!
//! 1. **Plan** ([`plan_fold`], behind a backend feature). The LLM sees a
//!    [`LedgerSnapshot`] (existing projects + their still-open items, each with a
//!    real id) and the new extract, and returns a [`ReconcilePlan`]: which
//!    projects the meeting maps to (match an id or create), which open items it
//!    closed (by id), and what is genuinely new.
//! 2. **Apply** ([`apply_plan`], backend-free). Validates every id the model
//!    returned against the real ledger — an invented id just drops that
//!    operation, so the model can never hallucinate a completion or a phantom
//!    project — then performs the mutations, writes audit history, and bumps
//!    project activity. Anything that can't be routed lands in **Unfiled**.
//!
//! Ledger ids are minted deterministically from the session id (`<sess>-pN` /
//! `<sess>-iN`): unique because a session folds exactly once, and stable for
//! tests. A rebuild wipes the ledger first, so there is never a collision.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use zord_core::{ItemKind, ItemStatus, Project, ProjectItem, ProjectStatus};
use zord_store::Store;

#[cfg(any(feature = "llama", feature = "remote"))]
use crate::SessionExtract;

/// Catch-all project for items whose project couldn't be routed. Fixed id so it
/// is reused across folds.
pub const UNFILED_ID: &str = "unfiled";
pub const UNFILED_NAME: &str = "Unfiled";

// ---------------------------------------------------------------------------
// Snapshot (ledger → model input)
// ---------------------------------------------------------------------------

/// The slice of the ledger shown to the reconcile model: active projects and
/// their still-open items (done items are history and aren't routing targets).
#[derive(Debug, Clone, Serialize)]
pub struct LedgerSnapshot {
    pub projects: Vec<SnapProject>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapProject {
    pub id: String,
    pub name: String,
    pub open_items: Vec<SnapItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapItem {
    pub id: String,
    pub kind: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// Build the snapshot from the store: active projects, each with its active items.
pub fn snapshot(store: &Store) -> Result<LedgerSnapshot> {
    let mut projects = Vec::new();
    for p in store.list_projects()? {
        if p.status != ProjectStatus::Active {
            continue;
        }
        let open_items = store
            .list_items(&p.id)?
            .into_iter()
            .filter(|it| it.status.is_active())
            .map(|it| SnapItem {
                id: it.id,
                kind: it.kind.as_str().to_string(),
                text: it.text,
                owner: it.owner,
            })
            .collect();
        projects.push(SnapProject {
            id: p.id,
            name: p.name,
            open_items,
        });
    }
    Ok(LedgerSnapshot { projects })
}

// ---------------------------------------------------------------------------
// Plan (model output)
// ---------------------------------------------------------------------------

/// What the reconcile model decided to do with one meeting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconcilePlan {
    #[serde(default)]
    pub projects: Vec<PlanProject>,
    #[serde(default)]
    pub complete: Vec<PlanComplete>,
    #[serde(default)]
    pub add: Vec<PlanAdd>,
}

/// Routing for one project the meeting touched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanProject {
    /// Existing project id to merge into, or `None`/unknown to create.
    #[serde(default)]
    pub match_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub summary: String,
}

/// An existing open item the meeting closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanComplete {
    pub id: String,
    #[serde(default)]
    pub why: String,
}

/// A genuinely new item to add.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanAdd {
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub done: bool,
}

/// Ask the model to fold `extract` into the ledger `snapshot`.
#[cfg(any(feature = "llama", feature = "remote"))]
pub fn plan_fold(
    llm: &zord_summarize::LlmBackend,
    snapshot: &LedgerSnapshot,
    extract: &SessionExtract,
    n_ctx: u32,
) -> Result<ReconcilePlan> {
    use zord_summarize::GenOpts;
    let payload = format!(
        "EXISTING LEDGER:\n{}\n\nNEW EXTRACT:\n{}",
        serde_json::to_string_pretty(snapshot)?,
        serde_json::to_string_pretty(extract)?,
    );
    let raw = llm.generate(
        &payload,
        zord_config::reconcile_prompt(),
        GenOpts::overview(n_ctx),
    )?;
    Ok(parse_plan(&raw))
}

/// Parse a reconcile reply, tolerating prose/fences. Empty plan on failure.
pub fn parse_plan(raw: &str) -> ReconcilePlan {
    let Some(json) = crate::extract::first_json_object(raw) else {
        tracing::warn!("reconcile: no JSON object in reply ({} bytes)", raw.len());
        return ReconcilePlan::default();
    };
    serde_json::from_str::<ReconcilePlan>(json).unwrap_or_else(|e| {
        tracing::warn!("reconcile: JSON parse failed: {e}");
        ReconcilePlan::default()
    })
}

// ---------------------------------------------------------------------------
// Apply (plan → store mutations) — backend-free, fully testable
// ---------------------------------------------------------------------------

/// What a fold changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FoldStats {
    pub projects_created: usize,
    pub items_added: usize,
    pub items_completed: usize,
}

impl FoldStats {
    /// Accumulate another fold's stats (for multi-session runs).
    pub fn merge(&mut self, o: FoldStats) {
        self.projects_created += o.projects_created;
        self.items_added += o.items_added;
        self.items_completed += o.items_completed;
    }
}

/// Apply a validated plan to the store under session `session_id`. `now` is the
/// epoch-ms timestamp stamped on every change (caller supplies it so this stays
/// pure/testable). Unknown ids are silently skipped — the model cannot conjure
/// completions or projects that don't exist.
pub fn apply_plan(
    store: &Store,
    session_id: &str,
    now: u64,
    plan: &ReconcilePlan,
) -> Result<FoldStats> {
    let mut stats = FoldStats::default();

    // Existing projects keyed by normalized name, so a null match_id whose name
    // already exists merges instead of duplicating.
    let existing = store.list_projects()?;
    let by_name: HashMap<String, String> = existing
        .iter()
        .map(|p| (norm(&p.name), p.id.clone()))
        .collect();

    // 1. Route projects. Build canonical-name -> id for the add step.
    let mut name_to_id: HashMap<String, String> = HashMap::new();
    for (j, pp) in plan.projects.iter().enumerate() {
        let name = pp.name.trim();
        if name.is_empty() {
            continue;
        }
        let summary = pp.summary.trim();
        // Prefer a valid match_id, else an existing same-name project, else create.
        let matched = pp
            .match_id
            .as_deref()
            .filter(|id| existing.iter().any(|p| p.id == *id))
            .map(str::to_string)
            .or_else(|| by_name.get(&norm(name)).cloned());

        let id = match matched {
            Some(id) => {
                if !summary.is_empty() {
                    store.set_project_description(&id, Some(summary), now)?;
                }
                store.touch_project(&id, now)?;
                id
            }
            None => {
                let id = format!("{session_id}-p{j}");
                store.create_project(&Project {
                    id: id.clone(),
                    name: name.to_string(),
                    status: ProjectStatus::Active,
                    description: (!summary.is_empty()).then(|| summary.to_string()),
                    created_at: now,
                    updated_at: now,
                    last_activity_at: now,
                })?;
                store.log_history(&id, None, "project-created", Some(session_id), now)?;
                stats.projects_created += 1;
                id
            }
        };
        name_to_id.insert(norm(name), id);
    }

    // 2. Complete existing open items by id (validated; never invented).
    for c in &plan.complete {
        let id = c.id.trim();
        if id.is_empty() {
            continue;
        }
        let Some(item) = store.get_item(id)? else {
            continue;
        };
        if !item.status.is_active() {
            continue; // already done
        }
        store.update_item_status(
            id,
            ItemStatus::Done,
            Some(session_id),
            Some(session_id),
            now,
        )?;
        store.log_history(
            &item.project_id,
            Some(id),
            "completed",
            Some(session_id),
            now,
        )?;
        store.touch_project(&item.project_id, now)?;
        stats.items_completed += 1;
    }

    // 3. Add new items, routing each to its project (or Unfiled).
    let mut unfiled: Option<String> = None;
    for (k, a) in plan.add.iter().enumerate() {
        let text = a.text.trim();
        if text.is_empty() {
            continue;
        }
        let project_id = match name_to_id.get(&norm(&a.project)) {
            Some(id) => id.clone(),
            None => match by_name.get(&norm(&a.project)) {
                Some(id) => id.clone(),
                None => ensure_unfiled(store, &mut unfiled, now)?,
            },
        };
        let owner = a
            .owner
            .as_deref()
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .map(str::to_string);
        let status = if a.done {
            ItemStatus::Done
        } else {
            ItemStatus::Open
        };
        let completed_session = a.done.then(|| session_id.to_string());
        let id = format!("{session_id}-i{k}");
        store.add_item(&ProjectItem {
            id: id.clone(),
            project_id: project_id.clone(),
            kind: ItemKind::parse(&a.kind),
            text: text.to_string(),
            owner,
            status,
            created_session: Some(session_id.to_string()),
            updated_session: Some(session_id.to_string()),
            completed_session,
            created_at: now,
            updated_at: now,
            manual: false,
        })?;
        store.log_history(
            &project_id,
            Some(&id),
            if a.done { "added-done" } else { "added" },
            Some(session_id),
            now,
        )?;
        store.touch_project(&project_id, now)?;
        stats.items_added += 1;
    }

    Ok(stats)
}

/// Lazily create (once) and return the Unfiled bucket's id.
fn ensure_unfiled(store: &Store, cache: &mut Option<String>, now: u64) -> Result<String> {
    if let Some(id) = cache {
        return Ok(id.clone());
    }
    if store.get_project(UNFILED_ID)?.is_none() {
        store.create_project(&Project {
            id: UNFILED_ID.to_string(),
            name: UNFILED_NAME.to_string(),
            status: ProjectStatus::Active,
            description: Some("Items that couldn't be routed to a project.".to_string()),
            created_at: now,
            updated_at: now,
            last_activity_at: now,
        })?;
    } else {
        store.touch_project(UNFILED_ID, now)?;
    }
    *cache = Some(UNFILED_ID.to_string());
    Ok(UNFILED_ID.to_string())
}

/// Case/space-insensitive project-name key for matching.
fn norm(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("zord-rec-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        (Store::open(&db).unwrap(), dir)
    }

    fn plan(json: &str) -> ReconcilePlan {
        parse_plan(json)
    }

    #[test]
    fn first_fold_creates_projects_and_items() {
        let (s, dir) = tmp_store("first");
        let p = plan(
            r#"{
              "projects": [{"match_id": null, "name": "Billing migration", "summary": "porting to v2"}],
              "complete": [],
              "add": [
                {"project": "Billing migration", "kind": "action", "text": "Write the adapter", "owner": "Sarah", "done": false},
                {"project": "Billing migration", "kind": "decision", "text": "Drop legacy endpoint", "owner": null, "done": true}
              ]
            }"#,
        );
        let stats = apply_plan(&s, "sess-1", 1000, &p).unwrap();
        assert_eq!(stats.projects_created, 1);
        assert_eq!(stats.items_added, 2);

        let projects = s.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "Billing migration");
        assert_eq!(projects[0].description.as_deref(), Some("porting to v2"));

        let items = s.list_items(&projects[0].id).unwrap();
        assert_eq!(items.len(), 2);
        let decision = items.iter().find(|i| i.kind == ItemKind::Decision).unwrap();
        assert_eq!(decision.status, ItemStatus::Done);
        assert_eq!(decision.completed_session.as_deref(), Some("sess-1"));
        let action = items.iter().find(|i| i.kind == ItemKind::Action).unwrap();
        assert_eq!(action.status, ItemStatus::Open);
        assert_eq!(action.owner.as_deref(), Some("Sarah"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn second_fold_matches_project_and_completes_item() {
        let (s, dir) = tmp_store("second");
        // Fold 1.
        apply_plan(
            &s,
            "sess-1",
            1000,
            &plan(
                r#"{"projects":[{"match_id":null,"name":"Billing migration","summary":"v1"}],
                    "add":[{"project":"Billing migration","kind":"action","text":"Write the adapter","owner":"Sarah","done":false}]}"#,
            ),
        )
        .unwrap();
        let pid = s.list_projects().unwrap()[0].id.clone();
        let item_id = s.list_items(&pid).unwrap()[0].id.clone();
        assert_eq!(item_id, "sess-1-i0");

        // Fold 2: snapshot drives the model; here we hand-write the plan it would
        // produce — match the project by id, complete the adapter, add new work.
        let snap = snapshot(&s).unwrap();
        assert_eq!(snap.projects[0].open_items.len(), 1);
        assert_eq!(snap.projects[0].open_items[0].id, "sess-1-i0");

        let p2 = plan(&format!(
            r#"{{"projects":[{{"match_id":"{pid}","name":"Billing migration","summary":"adapter done"}}],
                 "complete":[{{"id":"sess-1-i0","why":"merged"}}],
                 "add":[{{"project":"Billing migration","kind":"action","text":"Do webhooks","owner":"Dev","done":false}}]}}"#
        ));
        let stats = apply_plan(&s, "sess-2", 2000, &p2).unwrap();
        assert_eq!(stats.projects_created, 0); // matched, not created
        assert_eq!(stats.items_completed, 1);
        assert_eq!(stats.items_added, 1);

        // Still one project; adapter now done with provenance; webhook open.
        let projects = s.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].description.as_deref(), Some("adapter done"));
        let adapter = s.get_item("sess-1-i0").unwrap().unwrap();
        assert_eq!(adapter.status, ItemStatus::Done);
        assert_eq!(adapter.completed_session.as_deref(), Some("sess-2"));
        assert_eq!(adapter.updated_session.as_deref(), Some("sess-2"));
        let open: Vec<_> = s
            .list_items(&pid)
            .unwrap()
            .into_iter()
            .filter(|i| i.status.is_active())
            .collect();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].text, "Do webhooks");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hallucinated_ids_are_dropped() {
        let (s, dir) = tmp_store("hallucinate");
        let stats = apply_plan(
            &s,
            "sess-1",
            1000,
            &plan(
                r#"{"projects":[{"match_id":"ghost-project","name":"Real","summary":""}],
                    "complete":[{"id":"ghost-item","why":"nope"}],
                    "add":[]}"#,
            ),
        )
        .unwrap();
        // match_id was invalid -> created fresh; phantom completion ignored.
        assert_eq!(stats.projects_created, 1);
        assert_eq!(stats.items_completed, 0);
        let projects = s.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, "sess-1-p0");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn null_match_id_with_known_name_merges() {
        let (s, dir) = tmp_store("merge-name");
        apply_plan(
            &s,
            "sess-1",
            1000,
            &plan(r#"{"projects":[{"match_id":null,"name":"Onboarding","summary":"a"}]}"#),
        )
        .unwrap();
        // Different casing, still null match_id -> should merge, not duplicate.
        let stats = apply_plan(
            &s,
            "sess-2",
            2000,
            &plan(
                r#"{"projects":[{"match_id":null,"name":"onboarding","summary":"b"}],
                    "add":[{"project":"onboarding","kind":"question","text":"SSO?","done":false}]}"#,
            ),
        )
        .unwrap();
        assert_eq!(stats.projects_created, 0);
        let projects = s.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].description.as_deref(), Some("b"));
        assert_eq!(s.list_items(&projects[0].id).unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unroutable_add_lands_in_unfiled() {
        let (s, dir) = tmp_store("unfiled");
        let stats = apply_plan(
            &s,
            "sess-1",
            1000,
            &plan(
                r#"{"projects":[],"add":[{"project":"Nowhere","kind":"action","text":"orphan task","done":false}]}"#,
            ),
        )
        .unwrap();
        assert_eq!(stats.items_added, 1);
        let unfiled = s.get_project(UNFILED_ID).unwrap().unwrap();
        assert_eq!(unfiled.name, UNFILED_NAME);
        let items = s.list_items(UNFILED_ID).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "orphan task");

        let _ = std::fs::remove_dir_all(dir);
    }
}
