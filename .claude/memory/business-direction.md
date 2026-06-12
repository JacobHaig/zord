---
name: business-direction
description: Zord is going commercial — proprietary license, private repo planned, eventual free/premium business split; design features with that seam in mind
metadata:
  node_type: memory
  type: project
---

**Recorded June 2026 (user-stated).** Zord is moving from "open project" to a
commercial product:
- **License is proprietary** (all-rights-reserved `LICENSE`, Phase 43a;
  `Cargo.toml` uses `license-file`). Do NOT reintroduce open-source license
  claims in docs/metadata.
- **The repo is moving private.** ⚠ This breaks the self-updater's
  unauthenticated GitHub Releases check — decision pending between a public
  releases-only mirror repo (recommended) vs an authenticated updater. See
  `docs/SIGNING.md` §"Going private".
- **Planned business/enterprise split**: an eventual premium tier for
  business users. Premium candidates the user named or implied: platform
  integrations beyond Discord (Teams/Zoom identity), semantic search,
  knowledge-base export, team-scale voiceprints, admin/policy controls,
  store-channel distribution.
- **Implication for new work**: design features with a clean free/premium
  seam (Cargo features today; license-key gating later). Don't implement
  gating yet — just avoid entangling premium-candidate features with core
  paths in ways that would make a later split painful.

Related: [[productionization]], [[feature-flags]], docs/PLAN.md "Business
split" note + Phases 44 (KB export) / 45 (semantic search).
