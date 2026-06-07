# Zord

A fast, fully-local desktop app that records your microphone **and** desktop/system
audio and transcribes it (Whisper / optional Parakeet), labeled Me vs Others on one
timeline. Optional, all-local: AI summaries / compression / cross-meeting overview /
chat (built-in llama.cpp **or** any OpenAI-compatible server), per-speaker
diarization, full-text search, export, per-channel audio levels, deferred &
re-transcription, per-line audio replay, and at-rest encryption. Rust workspace +
Dioxus 0.7 desktop GUI (icon-rail shell) + a `zord` CLI + a localhost web dashboard.
See `README.md` (usage), `docs/PLAN.md` (design + phase roadmap), and
`docs/SECURITY.md` (security posture).

Optional capabilities are Cargo features so the default build stays lean: `parakeet`,
`llm-local` / `llm-remote` (AI features), `diarization`, `encryption`. Releases ship
all of them. Never add `Co-Authored-By` trailers to commits.

## Repo Memory

Claude stores project knowledge in `.claude/memory/` (committed to git).
At the start of every session, read `.claude/memory/MEMORY.md` to load context.
Use `/repo-memory` to save or retrieve memories.

### Recalling Information

Before answering questions about project decisions, conventions, or context,
check `.claude/memory/` first — read `MEMORY.md` for the index, then open
relevant files. This is the team's shared knowledge base.

### When to Save

| What | Type |
|------|------|
| Architectural decisions and their rationale | `project` |
| Team conventions, what to avoid or repeat | `feedback` |
| Links to external systems, dashboards, docs | `reference` |
| Personal preferences (add user_*.md to .gitignore if private) | `user` |
| Chosen libraries/frameworks and why alternatives were rejected | `project` |
| Things that were tried and didn't work (anti-patterns for this codebase) | `feedback` |
| Preferred naming conventions, code style, and formatting rules | `feedback` |
| Things that Claude got wrong multiple times and required correction | `feedback` |
| External API docs, service dashboards, internal wikis | `reference` |
| Environment setup notes (non-obvious deps, quirks, build steps) | `reference` |
| Domain knowledge the user has that I shouldn't re-explain | `user` |

### What NOT to Save
- Code patterns readable from the codebase
- Git history (git log / git blame are authoritative)
- Ephemeral task state or in-progress work
- Anything already in this CLAUDE.md
