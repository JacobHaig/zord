---
name: cross-meeting-synthesis
description: Phase 23 "Overview" — synthesize 30-50 meetings into per-project rollups via map-reduce, not one giant context
metadata:
  node_type: memory
  type: project
---

Phase 23 is the headline feature: a holistic **cross-meeting Overview** — across
the user's last ~30–50 recordings, a per-project picture of state / pending /
accomplished / owners / open questions, oriented around the user ("Me"). Goal:
answer "where's project X?" from a current, faithful rollup.

**Key decision — map-reduce over structured extracts, NOT a big context window.**
50 meetings ≈ 400–650K tokens, far beyond any practical local/CPU context (even
128K-context models are impractically slow + memory-huge on CPU). So:
1. **Map** (per meeting, once, cached): LLM emits a compact **structured JSON**
   record — project tag(s), action items `{what, owner, status, due}`, decisions,
   status updates, open questions, source refs (session id + timestamps).
2. **Reduce** (per project): group records by canonical project, synthesize a
   per-project rollup. Operates on the small extracts (reduced per project), so it
   fits — current Qwen2.5 (3B extract / 7B synth) on CPU is enough. Background
   worker; first backlog run is the only expensive pass, then incremental.
3. **Overview** = per-project rollups + pinned "My open action items".

Do NOT try to feed raw transcripts into one prompt — that's the trap; map-reduce
is why the user's "CPU, churn in background" instinct actually works.

**Locked UX/scope:** dedicated full **Overview view** (3rd top-level mode, button
above the session list); **LLM auto-detects + names projects** (+ normalization
pass). Items link back to source meeting/timestamp (anti-hallucination). Owner
attribution leans on diarization + speaker names; "Me" is always known.
Sub-steps 23a (extraction+schema) → 23b (group+rollup worker) → 23c (Overview UI)
→ 23d (refresh/edit + cross-meeting chat). Reuses llama.cpp; no new heavy deps.
Full plan: docs/PLAN.md Phase 23. Related: [[diarization-design]], [[feature-flags]].
