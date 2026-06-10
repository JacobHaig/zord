# Discord Integration — How It Works

How Zord records a Discord voice call: what the **user** does, and what happens
**behind the hood**. For the phase-by-phase build status see
[`docs/PLAN.md`](PLAN.md) → "Platform integrations (Phases 27–31)"; for diagrams
see [`docs/diagrams/integrations.md`](diagrams/integrations.md).

> **Status (June 2026).** The receive/decrypt path is **proven** (Phase 27 spike),
> and the engine, storage seam, and the **real `DiscordProvider`** are **built**
> (Phases 28–30c) — compile-verified; runtime-verified by you on a live call. The
> **Settings → Integrations UI** and the announcement / merged-file extras
> (Phases 30d–e) are **in progress**; until the UI lands, configure via
> `config.json` (or the `DISCORD_TOKEN`/`DISCORD_USER_ID` env vars) + capture mode
> "discord". Steps not yet wired are marked _(planned: 30x)_.

---

## 1. Why a bot at all?

Discord encrypts voice end-to-end (DAVE) and only delivers audio to *participants*
of a call. So Zord can't "tap" a Discord call from outside — instead **a bot you
control joins the voice channel as a participant** and receives one audio stream
per person. That gives Zord something the desktop-loopback path can't: **separate,
already-identified audio per speaker** (real names, no diarization guesswork) —
and Discord's own noise suppression / echo-cancellation is already applied.

You bring your own bot (a free Discord application). Nothing is hosted by Zord;
everything runs on your machine.

---

## 2. The user flow

### One-time setup
1. **Build with the feature:** `cargo run -p zord-gui --features discord`.
   (The default build omits the heavy Discord libraries.)
2. **Create a bot** in the [Discord Developer Portal](https://discord.com/developers/applications)
   and copy its **bot token**.
3. **In Zord → Settings → Integrations → Discord** _(planned: 30d)_, paste:
   - the **bot token**, and
   - **your own Discord user ID** (Discord → Settings → Advanced → Developer Mode
     on, then right-click your name → *Copy User ID*).
4. **Invite the bot to your server** — click **"Invite bot to a server"**
   _(planned: 30d)_. Zord reads the bot's application id from the token and opens
   the Discord authorize page; pick your server and approve. (The bot only needs
   *View Channel*, *Connect*, and *Send Messages*.)

### Each recording
5. Set the capture mode to **"Discord"** _(planned: 30d)_ and **join a voice
   channel** in a server the bot is in.
6. Press **Record**. The bot finds the channel you're in, **joins it**, and
   (optionally) posts a "recording started" message in the channel _(planned: 30e)_.
7. **Talk.** As each person speaks they appear as a labeled speaker (their Discord
   name); you're shown as **Me**. The transcript streams in if live transcription
   is on, or is produced when you stop.
8. Press **Stop** (or just leave the voice channel — the bot follows *you*, so it
   leaves when you do). The session lands in the sidebar with per-speaker labels,
   searchable and exportable like any other.
9. *Optional:* **Download merged audio** _(planned: 30e)_ — one WAV mixing every
   track.

What you **don't** do: pick a guild or channel (the bot follows you), grant a mic
permission (audio comes from Discord), or label speakers by hand (names are real).

---

## 3. Behind the hood — the moving parts

```
┌──────────────────────────── zord-gui (desktop app) ────────────────────────────┐
│  Settings → Integrations        engine control thread                          │
│  (token, user id) ─────────────► run_integration_session                       │
│                                        │                                       │
│                                        │ owns a job channel + a transcribe     │
│                                        │ thread + the integration thread       │
└────────────────────────────────────────┼───────────────────────────────────────┘
                                         │
       ┌─────────────────────────────────┼───────────────────────────────────────┐
       │ zord-integrations               │                                       │
       │   drive_session(provider) ◄─────┘   (assigns each participant a         │
       │        │                              TrackRole: Me or Speaker(n))      │
       │        ▼                                                                │
       │   Integration (trait)                                                   │
       │     └─ DiscordProvider  (feature `discord`)  ── songbird + serenity ──┐ │
       └───────────────────────────────────────────────────────────────────────┘ │
                                                                                 │
                                          Discord voice gateway (DAVE E2EE) ◄────┘
```

- **`zord-integrations`** holds the backend-agnostic seam: the `Integration`
  trait, `drive_session` (assigns speaker indices / the Me role), and `FakeProvider`
  (a dependency-free stand-in). The seam is in the *default* build — only the
  concrete `DiscordProvider` pulls the heavy libraries, behind `--features discord`.
- **`DiscordProvider`** (built, behind `--features discord`) wraps **songbird**
  (the Rust Discord voice library) + **serenity** (the gateway client) on a
  dedicated tokio runtime thread. It connects with your token, follows you into
  voice, and turns Discord's per-user RTP streams into plain mono-PCM streams.
- **`zord-gui`'s engine** (`run_integration_session`) wires those streams into the
  same recording pipeline everything else uses: resample → voice-activity
  segmentation → Whisper → SQLite.

---

## 4. Behind the hood — the data flow

### Connecting & following you in
```
You: Record (capture mode = Discord)
  │
  ▼
DiscordProvider.start()
  ├─ connect to the Discord gateway with the bot token
  ├─ wait for the guild cache, then scan voice states across every server the
  │  bot shares with you for your user id
  ├─ found in a channel → join it (songbird)            ── DAVE/MLS handshake ──►
  └─ not in voice yet → wait for a VOICE_STATE_UPDATE that puts you in one, then join
```
The bot only ever joins a server it's already a member of, and only the *one*
channel you're in (a user can be in only one voice channel at a time, so it's
unambiguous). No guild/channel is configured — this `identity → find live
session → join` primitive is the same one a future hosted bot would use.

### While recording — per participant
```
Discord voice (Opus, DAVE-encrypted)
  │  songbird decrypts (DAVE/MLS) + decodes Opus  →  48 kHz PCM, per SSRC
  ▼
DiscordProvider emits IntegrationEvent::ParticipantJoined { participant, audio }
        participant.is_me = true   if the SSRC maps to YOUR user id   → "Me"
        participant.is_me = false  otherwise                          → a speaker
  │
  ▼
drive_session assigns a TrackRole:  Me  |  Speaker(0), Speaker(1), …
  │
  ▼
engine spawns one proc per stream  (the same spawn_proc used for mic/desktop):
        downmix → resample to 16 kHz → VAD-segment → Whisper → Segment
        each Segment is tagged  source = Me/Others  + speaker index
        and inserted into SQLite; the real name goes into speaker_names
  │
  ▼
transcript view shows Me + named speakers, color-coded, on one timeline
```

Key properties:
- **"Me" is your own Discord stream**, not a local mic — so Discord's noise
  suppression applies to everyone uniformly, and there's no second clock to align.
- **Sparse → silence.** A stream only carries packets while that person talks. The
  engine pads the gaps with silence to wall-clock, so every track shares one
  timeline and speech lands at the right timestamp (see §5).
- **Names can lag the audio.** Discord maps an audio stream (SSRC) to a user via a
  separate "speaking" event; if someone was already talking when the bot joined,
  their stream is captured immediately but labeled once the mapping resolves
  (`ParticipantRenamed`), falling back to "Speaker N" if it never does
  _(the mapping-robustness fix is part of 30c)_.

### Joining / leaving mid-call
- Someone **joins** late → a new `ParticipantJoined` → a new speaker + track, with
  leading silence back to the session start (so their timeline still lines up).
- Someone **leaves** → their stream ends; their track just goes silent for the rest.
- **You** leave → the session ends (the bot follows you out).

### Stopping
```
Stop (or you leave voice)
  │
  ├─ provider leaves the channel
  ├─ each per-speaker track is finalized (a WAV under the session folder)
  ├─ any remaining audio is transcribed (if deferred), then
  └─ the session is saved — NO diarization runs (speakers are already known)
```

---

## 5. Where the audio + transcript live

Each session gets a date-time-named folder (Phase 28); every track is **session-
aligned** (same length, silence-padded), so `sample N` is the same instant on all
of them:

```
~/Library/Application Support/Zord/            (macOS; see README for other OSes)
└── audio/2026-06-09_18-15-47/
      ├── me.wav        ← your Discord stream
      ├── spk-0.wav     ← "Alex"
      ├── spk-1.wav     ← "Sam"
      └── …
```
- Because the tracks are aligned, the **merged single file** _(planned: 30e)_ is
  just a sample-wise sum + soft-limiter — derived on demand, not stored.
- Transcript segments (text + timing + `source`/`speaker`), full-text search, and
  `speaker_names` (index → real name) live in `zord.db`, exactly like every other
  session — which is why search/export/replay work unchanged.

---

## 6. Privacy, consent & trust

- **Local-only.** Audio, transcription, and storage stay on your machine. The bot
  connects to Discord (to receive the call) but Zord sends nothing elsewhere.
- **Consent.** Discord's developer policy requires recording consent. Zord's
  signal is transparency: the bot joins as a **visible participant**, and
  _(planned: 30e)_ posts a "recording started" message in the channel so everyone
  sees it live.
- **The bot token is a credential.** It's stored in `config.json` in plaintext
  (same as the optional LLM API key); keep it private. (Keychain storage is a
  possible later addition.)

---

## 7. Component cheat-sheet

| Piece | Role |
|---|---|
| `zord-integrations` (`Integration`, `drive_session`, `FakeProvider`) | backend-agnostic seam; default build, no heavy deps |
| `DiscordProvider` (feature `discord`) | songbird/serenity → per-user PCM + identity _(30c)_ |
| `songbird` | Discord voice: DAVE decrypt + Opus decode, per-SSRC receive |
| `serenity` | Discord gateway: login, guild/voice-state events, REST |
| `zord-gui` `run_integration_session` | spawns per-speaker procs, owns the session lifecycle |
| `spawn_proc` | shared resample→VAD→transcribe per track (mic/desktop/Discord alike) |
| `zord-config` | `discord_bot_token`, `discord_user_id`, capture mode |
| `zord-store` | segments + `speaker_names` + full-text search |
| `zord-net` | the bot-invite REST call (`/oauth2/applications/@me`) _(30d)_ |
