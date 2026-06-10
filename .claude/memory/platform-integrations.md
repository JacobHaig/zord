---
name: platform-integrations
description: Phases 27–30 integration framework — separated per-participant feeds (Discord) reuse the diarization speaker surface; DAVE is the gate
metadata:
  node_type: memory
  type: project
---

Roadmap initiative (PLAN Phases 27–30, designed June 2026) to ingest audio from
platforms that hand us **separated, already-identified per-participant feeds** —
so diarization is unnecessary (we *know* who's speaking, with their real name).

**Approaches researched:**
- **A — Discord bot voice receive** (`songbird` `receive` feature, serenity
  ecosystem): per-SSRC decoded PCM, SSRC→user via `SpeakingStateUpdate`, name via
  REST. **Headline.** ⚠ **DAVE gate** — since March 2026 Discord mandates E2EE
  (MLS) on all voice; bots must implement DAVE to decrypt received audio.
  **songbird v0.6.0 (Apr 2026) added DAVE incl. in-place decryption.** ✅
  **VERIFIED LIVE (June 2026)** via the `discord-spike` bin: against a real
  DAVE-encrypted channel, crypto negotiated `Aes256Gcm`, MLS handshake completed,
  and the bot received 527 decoded audio frames in 30 s → a clean per-user WAV.
  **DAVE receive works** — bot path unblocked.
  ⚠ Gap: SSRC→user identity mapping returned 0 (no `SpeakingStateUpdate` for a
  user already speaking when the bot joined) — Phase 30 must make mapping robust
  (seed from voice states / client-connect, backfill, fall back to "Speaker N").
- **B — Per-process OS audio tap** (macOS 14.4+ Core Audio taps; Windows
  process-loopback): captures one app's output, no bot — but still a per-app
  *mix*, so diarization still needed. Universal fallback (Phase 30).
- **C — Zoom/Teams media bots/SDKs:** per-participant + names but heavy
  (bot joins + credentials + infra) → backlog.
- **D — Teams Graph `callTranscript`:** declined, no tenant access (see
  [[teams-audio-options]]).

**Architecture decisions (locked):**
- **Reuse the diarization identity surface, do NOT generalize `Source`.** An
  integration is a capture source that **pre-assigns the `speaker` index from
  ground truth** (writes real names into `speaker_names`) instead of clustering.
  Each participant stream runs the same `spawn_proc` path tagged `Others` +
  stable speaker index → FTS/exports/UI need ~no change. **Diarization parity:**
  diarized-desktop and integration speakers are structurally identical
  (source=Others + speaker idx + speaker_names); integration sessions are NEVER
  diarized (Identify-speakers button hidden), kept as plain per-speaker
  transcription like the mic. See [[diarization-design]], [[capture-design]].
- **Sparse audio → explicit silence (critical):** integration streams deliver
  packets only while a user speaks; absence MUST be padded to wall-clock silence
  (same hazard/fix as WASAPI loopback in `spawn_proc`). ⚠ revisit the 5-min
  pad cap — drive padding from the bot session clock for sparse sources.
- **Audio storage rework → folder-per-session (Phase 28).** Today flat
  `audio/<id>.{me,others}.wav` (prefix in `sessions.audio_path`); can't hold N
  speakers. Move to a **date-time-named folder** `audio/<YYYY-MM-DD_HH-MM-SS>/`
  (ALL session types) with `me.wav`/`others.wav`/`spk-N.wav` + a track manifest
  (role+speaker idx+name→file); `sessions.audio_path` holds the folder. Update all
  resolvers (session_audio_files, replay, retranscribe, diarize, apply_retention)
  with back-compat for the old flat layout. ✅ **28a–d DONE** (June 2026):
  `zord-config` has `session_audio_dir`/`track_path`/`resolve_track` (+chrono, 3
  tests); engine writes to the folder + stores it; all GUI/CLI readers
  (session_audio_files, diarize, post_transcribe, run_retranscribe, cmd_diarize)
  resolve via `resolve_track` (folder + flat back-compat, migration-free);
  `apply_retention` removes folders + legacy files. **28e (spk-N manifest +
  multi-track read) folded into Phase 30** (no producer to test until then).
- **Sparse-speaker model → FULL SESSION-ALIGNED tracks (decided).** Every track
  (me/others/spk-N) is anchored at session start + spans the whole recording,
  wall-clock silence-padded (a mid-meeting joiner gets leading silence; early
  leaver gets trailing silence). NO per-track offset → sample N = same instant on
  every track, so replay/re-transcribe/diarization/merge need zero new logic
  (exact generalization of the existing Me/Others model). Rejected: presence-
  window+offset (storage win, but offset concept everywhere) and per-utterance
  clips (fragments intermittent speech → bad ASR). Storage cost accepted (bounded
  by 30-day retention); trailing-silence trim is a future optimization.
- **Phase order (renumbered):** 27 Discord DAVE receive spike (do FIRST, bot key
  in hand) → 28 audio storage rework → 29 integration framework seam (fake
  provider) → 30 full Discord → 31 per-app capture.
- **User brings their own bot** — token pasted into settings (plaintext like the
  remote-LLM key), works with any bot. Per-instance recording **consent gate**
  (Discord ToS).
- **Follow-the-user auto-join — NO guild/channel input.** User provides the
  **Discord user ID to follow** (user ID is the decided primary path — needs only
  non-privileged `GUILDS`+`GUILD_VOICE_STATES`; username→ID would need the
  privileged `GUILD_MEMBERS` intent, deferred); on Connect the bot
  (intents `GUILDS` + `GUILD_VOICE_STATES`) scans the voice states of every guild
  it shares with that user, finds the VC they're currently in (only one
  possible), and joins. Only requirement: bot invited to the server being called
  in. **Leave when the user leaves** (VOICE_STATE_UPDATE → bot leaves, session
  finalizes). Include inline help in the Discord tab on how to get the ID/name
  (Developer Mode → Copy User ID). This `identity → find live session → join`
  primitive is chosen so it forward-maps to the future hosted bot.
- **Settings → Integrations (new tab)** is the home for all integrations (Discord
  now; Teams/Zoom later, not built). Follows the existing string-keyed `stab`
  button pattern in `zord-gui/src/main.rs`.
- **"Me" stays local mic**; the followed user == self, so their Discord SSRC is
  suppressed (dedupe self).
- **Live transcription reuses the existing Phase 25 `live_transcription` toggle**
  — stays optional, **defaults OFF for integration sessions** (N speakers live is
  CPU-heavy), capable machines can flip it on. No new mechanism.
- **Integration replaces system-loopback**: a Discord session is Me (mic) +
  per-speaker tracks, no mixed `others.wav`. Speakers are created mid-session
  (participants join/leave live), unlike diarization's fixed end-of-call set.

**Future direction (back-burnered, keep accessible):** a centralized/hosted bot
(named after the app) — given a user's Discord identity, joins their live
session, records, and DMs the transcript back to the requester; only requirement
is the bot added to the server. Local Phase 29 uses the same follow-by-user-id
primitive so it rolls forward; design the seam for a local↔hosted backend swap.

New crate `zord-integrations`; `serenity`/`songbird`/`opus` behind a `discord`
Cargo feature (out of the default build). Related: [[architecture]],
[[feature-flags]].

**Phase 29 seam (29a ✅ DONE, June 2026):** `Integration` trait
(`name`/`start`/`stop` → `Receiver<IntegrationEvent>`), events
`ParticipantJoined{participant,sample_rate,audio: Receiver<Vec<f32>>}` /
`ParticipantRenamed{key,name}` / `Ended{reason}`, `Participant{key,name}`. The
seam is dependency-free (default build, NOT behind `discord`) — only impls are
heavy. `FakeProvider` (canned sparse tone bursts) validates the path; unit-tested.
**29b ✅ DONE (build-verified):** `drive_session` (in zord-integrations,
unit-tested) assigns a stable 0-based speaker index per participant; engine
`run_integration_session` (separate fn, doesn't touch run_session) spawns a
per-speaker proc per `ParticipantJoined` (`Others` + ground-truth idx →
`spk-N.wav`, wall-clock aligned); `Job` gained `speaker: Option<i32>`; Me = local
mic; ends on `Ended` or Stop; no diarization. Hidden trigger
`ZORD_FAKE_INTEGRATION=1` reuses the Record button. Runtime check = GUI launch
(engine work isn't headless-testable). **29c folded into Phase 30** (env trigger
reuses Record; real UI = Settings → Integrations tab).

**Phase 30 decisions (June 2026):** feature flag = **`discord`** (per-platform,
zord-gui/app → zord-integrations/discord). Trigger = `capture_mode == "discord"`
(mutually exclusive with desktop loopback — no double-capture). **"Me" = the
followed user's own Discord stream (NOT a local mic)** — everyone captured via
Discord so its noise-suppression applies uniformly; `Participant.is_me` →
`TrackRole::Me` → `Source::Me`; no mic, no self-dedupe. Consent = **optional
in-channel announcement** (bot posts "recording started"). **Optional merged
single-file** export (mix session-aligned tracks — cheap since aligned).
- **30a ✅** `discord` feature on zord-gui + `discord_bot_token`/`discord_user_id`
  config.
- **30b ✅** `is_me`/`TrackRole` seam + engine routes Me/Others, no local mic;
  FakeProvider marks p0 as me; tests green.
- **30c** real `DiscordProvider` (songbird, behind feature): follow-user join,
  per-SSRC → ParticipantJoined (followed id → is_me), SSRC→user mapping fix, leave
  on user-leave; revisit 5-min pad cap.
- **30d** Settings → Integrations tab (token/user-id/help, Invite-bot button,
  Test-connection, capture-mode "Discord", capability-aware).
- **30e** in-channel announcement + merged-file export.
