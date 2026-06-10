---
name: docs-canonical
description: doc roles — PLAN.md is the phase tracker; README sells; KICKSTART.md is the build/run guide; RELEASE/SECURITY/discord-integration roles
metadata:
  node_type: memory
  type: reference
---

Canonical project docs and their single responsibilities (DRY — June 2026
restructure; keep each fact in exactly one place):

- **`docs/PLAN.md`** — source of truth for design decisions, gap analysis, and
  the **numbered phase roadmap with status**. Sections renumbered June 2026
  (8 = integrations, 9 = productionization, 10 = open questions, 11 = sources).
  Check here first for "what's done / what's next."
- **`README.md`** — the **sell**: what Zord is, why it's different (privacy,
  both-sides capture, Discord per-speaker, per-app capture, local AI),
  installing a release (unsigned-build bypasses, update behavior), a 2-line
  build quickstart, project layout, and the documentation map. NO detailed
  build/CLI content — that moved to KICKSTART.
- **`KICKSTART.md`** (root) — the **getting-started guide**: per-platform
  prerequisites, build commands, the optional-feature table (with extra deps
  per feature), running the GUI/CLI, first-run permissions, data locations,
  troubleshooting, packaging pointer.
- **`docs/RELEASE.md`** — distributable builds: signing/notarization, the CI
  release workflow, artifact naming (an API for the updater!), distribution
  channels (github/steam/msstore).
- **`docs/SECURITY.md`** — threat model + June 2026 review; "surfaces added
  after the review" section tracks Discord/update-check/self-update for the
  next pass.
- **`docs/discord-integration.md`** — Discord user flow + under-the-hood.
- **`docs/model-licensing.md`** — commercial-licensing audit of in-app models.
- **`docs/diagrams/integrations.md`** — ASCII diagrams (updated for
  Me-from-Discord + the Record Discord button).
- `NAMING.md` was deleted (June 2026, user request). NOTE: the
  rename-before-commercial question it tracked is still **open** — only the
  candidate list is gone.

Dioxus is pinned to **0.7.9** (and `dioxus-cli` 0.7.9). Build/run is plain
`cargo`; `dx` is only needed for bundling. Related: [[commit-and-workflow]],
[[productionization]].
