---
name: commit-and-workflow
description: Team conventions — no co-author trailers, phased commits, verify before claiming done, keep docs in sync
metadata:
  node_type: memory
  type: feedback
---

Working conventions for this repo:

- **No `Co-Authored-By` trailers** (or any AI co-author line) in commit messages.
  Plain messages only. The user said "we don't do co-authors here." If one slips
  in, `git commit --amend` to remove it.
- Work proceeds in **numbered phases** (see `docs/PLAN.md`); each phase is one or
  more focused commits, often split into sub-steps (e.g. 11a/11b/11c). Branch is
  `develop`.
- **Verify before claiming done** — run the build (and launch the GUI / a quick
  CLI exercise) and report real output. Don't assert success without evidence.
  Be explicit about what could NOT be verified ([[verification-limits]]).
- **Keep docs in sync**: update `docs/PLAN.md` (phase status), `README.md`, and
  `docs/RELEASE.md` as part of the phase that changes behavior.
- **Versioning — bump the PATCH only by default** (0.2.13 → 0.2.14 → …), even for
  large/visible changes (the icon-rail UI overhaul shipped as a patch). **Never**
  bump minor or major on your own — **ask first** if something seems to warrant
  it. Release = edit `version` in `[workspace.package]` of `Cargo.toml`, `cargo
  check` to refresh the lock, commit `chore: release vX.Y.Z` (body becomes the
  GitHub release Highlights via the `notes` CI job) + tag `vX.Y.Z` + push both.

**Why:** the user values diligence, honest reporting, a clean history, and a
steady incremental version scheme (minor/major bumps are an owner decision).
Related: [[docs-canonical]].
