---
name: productionization
description: Phases 32–35 official-release initiative — stability hardening, CI gates, channel-aware updates (stores own updates), unsigned for now
metadata:
  node_type: memory
  type: project
---

Initiative (June 2026, PLAN Phases 32–35): stabilize Zord and ship an
**official public release**. Triggered by a stability audit; full phase detail
in `docs/PLAN.md` → "Productionization & official release".

**Decisions locked (June 2026):**
- **Versioning stays 0.2.x** — no 1.0 declaration; the release is "latest".
- **Multi-channel distribution**: GitHub Releases first; Steam, Microsoft
  Store, maybe Mac App Store, possibly an own store later. **Stores own
  updates on their channels** (they forbid self-updating binaries).
- **Update mechanism = channel build seam**: build-time channel id
  (github|steam|msstore|macappstore) + a **`self-update` Cargo feature** only
  in GitHub/own-store builds. Portable-EXE update on Windows = rename-swap
  (running EXE can be renamed, not overwritten; `self-replace` crate). macOS =
  notify+link only until signing exists (Gatekeeper re-quarantines unsigned
  downloads).
- **Ship unsigned** (no certs yet); document Gatekeeper right-click-open +
  SmartScreen bypass; CI signing steps stay gated for when certs arrive.
- **Discord 30d/30e land BEFORE the release** (headline feature).
- **Order: 32 → 33 → 30d/30e → 34 → release; 35 (stores) can trail.**

**Audit top findings (verified, June 2026):**
1. `engine.rs` WAV `let _ = w.finalize()` — errors swallowed; panic mid-proc
   drops writer unfinalized → unplayable WAV (data loss).
2. zord-store: no SQLite `busy_timeout` → instant SQLITE_BUSY under
   concurrent db_loop + transcription writes.
3. Engine threads (control/db/model/play loops, spawn_proc) not panic-safe —
   only diarization has `catch_unwind`; a panic hangs the UI silently.
4. `config.json` written in place (not temp+rename) → corrupt on crash →
   silent full settings reset on next load.
5. WASAPI loopback drain `pop_front().unwrap()` race (zord-capture/system.rs).
6. No PR CI at all (only tag-triggered release.yml); 4 crates have zero tests
   (zord-gui, zord-capture, zord-core, zord-transcribe).
7. No update path for users (no check, no notice).

Related: [[platform-integrations]], [[commit-and-workflow]],
[[verification-limits]], [[ci-macos-xcode26]], [[ci-windows-crt]].
