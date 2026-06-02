---
name: teams-audio-options
description: How to get Teams meeting audio / speaker identity — what's possible without a bot (researched June 2026)
metadata:
  node_type: memory
  type: reference
---

Researched June 2026 for "separate Teams voices more coherently without adding a
participant." Findings:

- **No botless per-participant audio.** The Teams desktop client mixes all remote
  participants into one output stream (what Zord captures as system audio). There
  is no local API to split it per speaker.
- **Per-participant audio = a bot.** Microsoft Graph real-time media
  (`Calls.AccessMedia.All`, app-hosted media bot, `Microsoft.Graph.Communications.
  Calls.Media`, Windows-Server host) can deliver **unmixed per-participant audio +
  active/dominant speaker identity** — but the bot **joins the meeting as a
  participant** and needs tenant-admin consent. Rejected: that's "another user".
- **⭐ Real speaker NAMES without a bot — Graph `callTranscript` (GA).** Post-
  meeting, fetch Teams' own transcript (VTT) with real `speakerName` + text +
  timing via `GET /users/{id}/onlineMeetings/{mtgId}/transcripts/{id}/content`.
  Requires: Teams **live transcription was on** during the meeting + Azure AD app
  with Graph perms (`OnlineMeetingTranscript.Read.All` or RSC). An org tenant may
  restrict this. Highest-fidelity path to real names; planned as a deferred phase.
- **Browser-extension capture** (Tactiq-style) is botless but transcription-only.

**Local, no-auth alternative** to improve speaker separation on the mixed stream:
upgrade diarization — **NVIDIA Sortformer** (>2× lower DER than pyannote, better
overlap, streaming) or **pyannote community-1 (4.0)**; optionally speech-
separation-guided diarization for crosstalk. Tracked as PLAN Phase 21.

Docs: Graph callTranscript — learn.microsoft.com/graph/api/calltranscript-get ·
real-time media bots — learn.microsoft.com/microsoftteams/platform/bots/calls-and-meetings/real-time-media-concepts
Related: [[diarization-design]].
