---
name: docs-canonical
description: docs/PLAN.md is the canonical design + phase tracker; README and docs/RELEASE.md roles
metadata:
  node_type: memory
  type: reference
---

Canonical project docs (keep current — see [[commit-and-workflow]]):

- **`docs/PLAN.md`** — the source of truth for design decisions, the gap
  analysis, and the **numbered phase roadmap with status** (✅/🟡/pending).
  Phases 1–15 are done; **Phase 16 (per-speaker diarization)** is the only
  remaining planned phase. Check here first for "what's done / what's next."
- **`README.md`** — user-facing: features, prerequisites, build (incl. optional
  `--features parakeet|encryption|summaries`), CLI usage, data locations.
- **`docs/RELEASE.md`** — packaging, code-signing/notarization (user's Apple
  account), and the GitHub Actions release workflow.

Dioxus is pinned to **0.7.9** (and `dioxus-cli` 0.7.9). Build/run is plain
`cargo`; `dx` is only needed for bundling.
