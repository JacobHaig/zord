# Platform Integrations — Diagrams

ASCII reference diagrams for the platform-integrations initiative
(Discord-first; PLAN.md → "Platform integrations (Phases 27–31)"). Kept here for
quick reference; update alongside the design.

---

## 1. The core insight — integrations reuse the diarization identity surface

Diarization *infers* a speaker by clustering one mixed track; an integration
*knows* the speaker by construction. Both end up stored identically
(`source=Others` + `speaker` index + a `speaker_names` entry), so storage / FTS /
exports / transcript UI need almost no change. Integration sessions are never
diarized.

```
                 local capture                  with an integration
   mic ──► Me                          Discord ─┬─► Me  (your own stream;
   system-loopback ──► Others ─┐       (no mic) │        no local capture)
                               │                ├─► Others + speaker=0 ("Alex")
                  diarization ─┘                ├─► Others + speaker=1 ("Sam")
                  (cluster → Speaker N)         └─► Others + speaker=2 ("Jo")
                                                name map written directly,
                                                NO diarization pass
```

> Phase 30b decision: in an integration session **"Me" is your own Discord
> stream too** — Discord's noise suppression applies to every track and there
> is no second clock to align.

---

## 2. Audio storage — flat files → folder-per-session (Phase 28)

A fixed two-file scheme can't hold N per-speaker tracks, so kept audio moved to a
date-time-named folder per session. Resolvers accept **both** layouts
(migration-free).

```
OLD:  audio/sess-1781029318224.me.wav
      audio/sess-1781029318224.others.wav        (flat, prefix-keyed)

NEW:  audio/2026-06-09_18-15-47/me.wav
      audio/2026-06-09_18-15-47/others.wav        (date-time folder)
      audio/2026-06-09_18-15-47/spk-0.wav         (per-speaker, integrations)
      audio/2026-06-09_18-15-47/spk-1.wav
```

---

## 3. Sparse-speaker model — full session-aligned tracks (decided)

Every track is anchored at session start and spans the whole recording,
wall-clock silence-padded. `sample N` = the same real instant on every track, so
no per-track offset is needed and replay / re-transcribe / merge are unchanged.
A mid-meeting joiner gets leading silence; an early leaver, trailing silence.
(`█` = audio, `░` = silence)

```
session start ─────────────────────────────── stop
Me       ████░░██░░░████░░░░██░░░░░░░██░░░░░██░░░  (full)
spk-Alex ████░░░██░░░░░██░░░░░░░░██░░░░░░██░░░░░░  (joined at 0)
spk-Sam  ░░░░░░░░░░░░░░░░░░░████░░██░░░░░░██░░░░░  (silence till join @5m)
spk-Jo   ░░░░░░░░░░░░██░░░██░░░░░░░░░░░░░          (left early → silence to end)

all tracks same length · sample N = same instant · no offsets
```

Alternatives considered and **rejected**:

```
Presence-window + offset   each track = join→leave + a stored start_offset_ms.
                           Less storage, but every reader must add the offset.

Per-utterance clips        only spoken bursts stored, no inter-utterance silence.
                           Smallest, but fragments intermittent speech → bad ASR.
```

---

## 4. Phase 27 DAVE receive spike — the full verified chain

Proved a bot can receive + decrypt per-user voice under Discord's mandatory DAVE
end-to-end encryption, all the way through to a transcript.

```
Discord voice (DAVE E2EE, Aes256Gcm + MLS)
   └─ songbird receive + decrypt ──► per-user PCM (48 kHz stereo)
        └─ downmix → mono WAV (wall-clock aligned, silence-padded via tick.silent)
             └─ Zord: resample→16 kHz ──► VAD ──► Whisper (Metal)
                  └─ timestamped transcript, stored in SQLite ✅
```

---

## 5. The integration seam (Phase 29a)

A dependency-free trait in the default build; only concrete impls (Discord, behind
`discord`) are heavy. `FakeProvider` validates the engine path with no network.

```
Integration (trait)
  start() ─► Receiver<IntegrationEvent>
                 ├─ ParticipantJoined { participant, sample_rate, audio: Receiver<Vec<f32>> }
                 ├─ ParticipantRenamed { key, name }     (late identity, e.g. SSRC→name)
                 └─ Ended { reason }                       (followed user left, etc.)

impls:  FakeProvider (default, tested)   ·   DiscordProvider (Phase 30, `discord`)
```

---

## 6. Phase 29b — engine integration-session flow

`run_integration_session` is separate from `run_session` so it can't destabilize
the proven recording path. `drive_session` assigns a stable speaker index per
participant; each becomes a per-speaker track.

```
Record Discord button            (dev: ZORD_FAKE_INTEGRATION=1 + Record)
        │
        ▼
run_integration_session          (separate from run_session; NO local mic)
        │
        └─ drive_session(provider)
              on ParticipantJoined(role, name, audio):
                 is_me ──► spawn_proc(Me)          ──► me.wav       ─┐
                 else  ──► set_speaker_name(idx)   ──► speaker_names │─► transcribe
                           spawn_proc(Others, idx) ──► spk-<idx>.wav ┘  (Job.speaker
              on Ended / user Stop ──► finalize, no diarization,         → segment)
                 then the post-stop transcription pass (when live is off)
```

---

## 7. Follow-the-user (local now, hosted later)

The local bot follows a configured Discord user id into whatever voice channel
they're in across any shared server — the same `identity → find live session →
join` primitive a future hosted bot would use, so the local impl rolls forward.

```
LOCAL (now):    user's bot token + user id ─► scan shared guilds' voice states
                                            ─► join the channel the user is in
                                            ─► record per-participant ─► local transcript

HOSTED (later): user identity ─► app-operated bot finds their live session
                              ─► joins (bot already in the server)
                              ─► records ─► DMs transcript back to the requester
```
