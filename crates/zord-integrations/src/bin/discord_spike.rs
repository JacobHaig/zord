//! Phase 27 — Discord DAVE receive spike.
//!
//! The single de-risking experiment that gates the whole Discord integration:
//! **can a bot still receive and decrypt per-user voice now that DAVE
//! (Discord's mandatory end-to-end encryption) is enforced?** songbird 0.6
//! claims DAVE support including in-place decryption — this proves it on a live
//! channel.
//!
//! What it does: joins one voice channel, subscribes to `VoiceTick` (decoded
//! per-user PCM, every 20 ms, with an explicit *silent* set), writes **one mono
//! 48 kHz WAV per speaker** — silence-padded to wall-clock so the tracks stay
//! aligned (a preview of the Phase 28 sparse→silence model) — maps SSRC→user,
//! then leaves after N seconds and prints the mapping.
//!
//! **Success = the per-user WAVs contain intelligible speech.** If they're empty
//! or garbled, DAVE receive is broken for bots and the plan pivots to Approach B
//! (per-app OS capture) as the primary path.
//!
//! Run it (build with the feature; the bot must already be in the server and you
//! must be in the target voice channel):
//!
//! ```bash
//! export DISCORD_TOKEN=...           # your bot token
//! export DISCORD_GUILD_ID=...        # server (guild) id
//! export DISCORD_CHANNEL_ID=...      # voice channel id to join
//! export ZORD_SPIKE_SECS=30          # optional, default 30
//! export ZORD_SPIKE_OUT=./spike-out  # optional, default ./discord-spike-out
//! cargo run -p zord-integrations --features discord --bin discord-spike
//! ```
//!
//! This is a throwaway bench — it joins a *fixed* channel by id. The production
//! follow-the-user auto-join (resolve the user's live VC from voice states) is
//! Phase 30, on the Phase 29 integration seam.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use dashmap::DashMap;
use serenity::all::{GatewayIntents, Ready};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::model::id::{ChannelId, GuildId, UserId};
use serenity::model::voice::VoiceState;
use songbird::driver::{DecodeConfig, DecodeMode};
use songbird::{
    Config, CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit,
};
use tokio::sync::Mutex;
use zord_audio::WavWriter;

/// One 20 ms frame of mono audio at Discord's 48 kHz decode rate.
const FRAME_SAMPLES: usize = 48_000 / 50; // 960

/// Shared receive state: the SSRC→user map and one open WAV writer per speaker.
struct Inner {
    /// SSRC (RTP stream id) → Discord user id, learned from speaking-state and
    /// voice-tick events. Lets us label each per-speaker track with its owner.
    ssrc_user: DashMap<u32, u64>,
    /// One open WAV per SSRC. Behind an async mutex because `VoiceTick` arrives
    /// on the driver task and the shutdown timer finalizes from another task.
    writers: Mutex<HashMap<u32, WavWriter>>,
    out_dir: PathBuf,
    // Diagnostic counters — tell apart "nobody was in the channel" from
    // "packets arrived but DAVE didn't decrypt".
    ticks: AtomicU64,           // VoiceTick events (one per 20ms while connected)
    rtp_packets: AtomicU64,     // raw RTP audio packets seen (encrypted or not)
    speaking_frames: AtomicU64, // per-tick speaker entries
    decoded_frames: AtomicU64,  // entries that carried decoded PCM
    /// Set once we've joined a channel — guards against double-join and lets the
    /// watchdog know whether the user ever joined voice.
    joined: AtomicBool,
}

impl Inner {
    /// Append `samples` to this SSRC's track, opening the file on first sight.
    /// Silently drops the frame if the file can't be created (spike-grade).
    async fn write(&self, ssrc: u32, samples: &[f32]) {
        let mut writers = self.writers.lock().await;
        let writer = writers.entry(ssrc).or_insert_with(|| {
            let path = self.out_dir.join(format!("spk-{ssrc}.wav"));
            WavWriter::create(&path, 48_000)
                .unwrap_or_else(|e| panic!("create {}: {e}", path.display()))
        });
        let _ = writer.write(samples);
    }
}

#[derive(Clone)]
struct Receiver(Arc<Inner>);

#[async_trait]
impl VoiceEventHandler for Receiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            // Maps an SSRC to its Discord user. Fires when a user (re)starts
            // speaking; a participant who never speaks stays unmapped.
            EventContext::SpeakingStateUpdate(s) => {
                if let Some(uid) = s.user_id {
                    self.0.ssrc_user.insert(s.ssrc, uid.0);
                    tracing::info!(ssrc = s.ssrc, user_id = uid.0, "ssrc → user mapped");
                }
            }

            // The heart of the spike: decoded PCM for every speaker, every 20 ms,
            // plus the set of SSRCs that are silent this tick. We advance EVERY
            // known track each tick — real audio when present, silence otherwise —
            // so the per-speaker WAVs stay wall-clock aligned (the sparse→silence
            // model the integration depends on).
            EventContext::VoiceTick(tick) => {
                self.0.ticks.fetch_add(1, Ordering::Relaxed);
                let silence = [0.0f32; FRAME_SAMPLES];

                // Speakers with audio this tick.
                for (ssrc, data) in &tick.speaking {
                    self.0.speaking_frames.fetch_add(1, Ordering::Relaxed);
                    if data.decoded_voice.as_ref().is_some_and(|v| !v.is_empty()) {
                        self.0.decoded_frames.fetch_add(1, Ordering::Relaxed);
                    }
                    match data.decoded_voice.as_ref() {
                        // Discord decodes to interleaved 48 kHz stereo i16;
                        // downmix to mono f32 in [-1, 1].
                        Some(pcm) if !pcm.is_empty() => {
                            let mono: Vec<f32> = pcm
                                .chunks_exact(2)
                                .map(|lr| (lr[0] as f32 + lr[1] as f32) * 0.5 / 32_768.0)
                                .collect();
                            self.0.write(*ssrc, &mono).await;
                        }
                        // Packet lost / no decode this tick → keep the clock by
                        // writing a frame of silence.
                        _ => self.0.write(*ssrc, &silence).await,
                    }
                }

                // Known-but-silent speakers: pad so their track tracks wall-clock.
                for ssrc in &tick.silent {
                    self.0.write(*ssrc, &silence).await;
                }
            }

            // Raw audio packets (counted before decode). If these climb but
            // decoded_frames stays 0, decryption/DAVE is the problem; if these
            // stay 0, nobody was transmitting.
            EventContext::RtpPacket(_) => {
                self.0.rtp_packets.fetch_add(1, Ordering::Relaxed);
            }

            EventContext::ClientDisconnect(d) => {
                tracing::info!(user_id = d.user_id.0, "client disconnected");
            }

            _ => {}
        }
        None
    }
}

/// Serenity gateway handler. Follows a user into voice across ANY server the bot
/// shares with them — no guild to configure (the Phase 30 mechanic).
struct Bot {
    /// Preferred: follow this user into whatever voice channel they're in.
    follow_user: Option<u64>,
    /// Fixed fallback (used only when no follow-user): explicit guild + channel.
    guild: Option<GuildId>,
    channel: Option<ChannelId>,
    secs: u64,
    recv: Receiver,
}

#[async_trait]
impl EventHandler for Bot {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        // `ready` fires before guild data is cached; we join from `cache_ready`.
        let guilds: Vec<u64> = ready.guilds.iter().map(|g| g.id.get()).collect();
        tracing::info!(bot = %ready.user.name, guild_count = guilds.len(), guilds = ?guilds, "gateway ready");
        if guilds.is_empty() {
            tracing::error!("the bot is in NO servers — invite it to the server you'll be calling in, then retry.");
            std::process::exit(1);
        }
    }

    async fn cache_ready(&self, ctx: Context, _guilds: Vec<GuildId>) {
        // Fixed-channel mode (no follow-user): join immediately.
        let Some(uid) = self.follow_user else {
            match (self.guild, self.channel) {
                (Some(g), Some(c)) => self.join_and_record(&ctx, g, c).await,
                _ => {
                    tracing::error!("set DISCORD_USER_ID (to follow), or both DISCORD_GUILD_ID + DISCORD_CHANNEL_ID");
                    std::process::exit(1);
                }
            }
            return;
        };

        // Scan EVERY server the bot is in: list who's in voice (with ids) so a
        // wrong user-id / wrong-server is obvious, and find the followed user.
        let mut found: Option<(GuildId, ChannelId)> = None;
        for gid in ctx.cache.guilds() {
            if let Some(g) = ctx.cache.guild(gid) {
                if !g.voice_states.is_empty() {
                    tracing::info!(
                        "server '{}' — {} user(s) in voice",
                        g.name,
                        g.voice_states.len()
                    );
                }
                for (u, vs) in g.voice_states.iter() {
                    let mark = if u.get() == uid {
                        "  ← matches DISCORD_USER_ID"
                    } else {
                        ""
                    };
                    tracing::info!(
                        "  in-voice: user {} → '{}' channel {:?}{mark}",
                        u.get(),
                        g.name,
                        vs.channel_id.map(|c| c.get())
                    );
                    if u.get() == uid {
                        if let Some(c) = vs.channel_id {
                            found = Some((gid, c));
                        }
                    }
                }
            }
        }

        if let Some((g, c)) = found {
            tracing::info!(user = uid, "user already in voice — following");
            self.join_and_record(&ctx, g, c).await;
            return;
        }

        // Otherwise wait for them to join (handled in voice_state_update).
        tracing::info!("waiting for user {uid} to join a voice channel in a server the bot is in — hop in now and talk…");
        let inner = self.recv.0.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(120)).await;
            if !inner.joined.load(Ordering::SeqCst) {
                tracing::warn!("gave up after 120s — never saw user {uid} join voice in a shared server. Are you in a server the bot is in, and is the user id right?");
                std::process::exit(1);
            }
        });
    }

    /// Live follow trigger: when the followed user joins/moves into a voice
    /// channel in ANY shared server, follow them in. (Phase 30's core mechanic.)
    async fn voice_state_update(&self, ctx: Context, _old: Option<VoiceState>, new: VoiceState) {
        // Log EVERY voice-state change so a user-id/server mismatch is visible.
        tracing::info!(
            user = new.user_id.get(),
            channel = ?new.channel_id.map(|c| c.get()),
            guild = ?new.guild_id.map(|g| g.get()),
            "voice_state_update"
        );
        let Some(uid) = self.follow_user else { return };
        if new.user_id != UserId::new(uid) {
            return;
        }
        if let (Some(g), Some(c)) = (new.guild_id, new.channel_id) {
            tracing::info!(
                user = uid,
                guild = g.get(),
                channel = c.get(),
                "followed user joined voice — following them in"
            );
            self.join_and_record(&ctx, g, c).await;
        }
    }
}

impl Bot {
    /// Join `channel` in `guild`, wire up the receive events, and arm the stop
    /// timer. Idempotent — only the first call does anything (guarded by `joined`).
    async fn join_and_record(&self, ctx: &Context, guild: GuildId, channel: ChannelId) {
        if self.recv.0.joined.swap(true, Ordering::SeqCst) {
            return; // already joined once
        }

        // Verify the channel is a voice channel so a failure is legible.
        match channel.to_channel(ctx).await {
            Ok(ch) => match ch.guild() {
                Some(gc) => {
                    tracing::info!(channel = %gc.name, kind = ?gc.kind, "resolved target channel")
                }
                None => {
                    tracing::warn!("target is not a guild channel (DM?) — join will likely fail")
                }
            },
            Err(e) => tracing::warn!(
                "could not resolve channel {} ({e}) — wrong id, or bot lacks View Channel?",
                channel.get()
            ),
        }

        let manager = songbird::get(ctx)
            .await
            .expect("songbird registered at init");
        let _call_lock = match manager.join(guild, channel).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    "join failed: {e} — bot lacks Connect permission, or wrong channel?"
                );
                std::process::exit(1);
            }
        };

        {
            let mut call = _call_lock.lock().await;
            // Call derefs to the Driver, which owns the global event store.
            call.add_global_event(CoreEvent::SpeakingStateUpdate.into(), self.recv.clone());
            call.add_global_event(CoreEvent::VoiceTick.into(), self.recv.clone());
            call.add_global_event(CoreEvent::RtpPacket.into(), self.recv.clone());
            call.add_global_event(CoreEvent::ClientDisconnect.into(), self.recv.clone());
        }

        tracing::info!(
            secs = self.secs,
            "✅ joined + receiving — recording per-user audio (talk now!)"
        );

        // Stop timer: leave, finalize every WAV, print the SSRC→user map + verdict.
        let manager = manager.clone();
        let inner = self.recv.0.clone();
        let secs = self.secs;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            let _ = manager.remove(guild).await;

            let mut writers = inner.writers.lock().await;
            let count = writers.len();
            for (ssrc, writer) in writers.drain() {
                let _ = writer.finalize();
                let who = inner
                    .ssrc_user
                    .get(&ssrc)
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| "unknown (never spoke / joined before bot)".into());
                tracing::info!("spk-{ssrc}.wav  ← user {who}");
            }
            // Diagnostic summary — reads as a decision tree.
            let ticks = inner.ticks.load(Ordering::Relaxed);
            let rtp = inner.rtp_packets.load(Ordering::Relaxed);
            let speaking = inner.speaking_frames.load(Ordering::Relaxed);
            let decoded = inner.decoded_frames.load(Ordering::Relaxed);
            let users = inner.ssrc_user.len();
            tracing::info!(
                ticks,
                rtp_packets = rtp,
                speaking_frames = speaking,
                decoded_frames = decoded,
                mapped_users = users,
                tracks = count,
                "receive diagnostics"
            );
            if rtp == 0 && speaking == 0 {
                tracing::warn!("NO audio packets → no one transmitted. Were you unmuted and talking in the channel the bot joined?");
            } else if decoded == 0 {
                tracing::error!("packets arrived but NONE decoded → DAVE/decrypt failure (the thing under test). decoded_voice was always empty.");
            } else {
                tracing::info!(
                    "✅ {decoded} decoded audio frames across {users} user(s) → DAVE receive WORKS. Listen to the WAVs in {} to confirm clean speech.",
                    inner.out_dir.display()
                );
            }
            std::process::exit(0);
        });
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,songbird=info,serenity=warn".into()),
        )
        .init();

    let token = std::env::var("DISCORD_TOKEN").context("set DISCORD_TOKEN (your bot token)")?;
    // Follow a user (preferred — the bot finds their current VC across any shared
    // server, no guild needed) OR a fixed guild+channel fallback.
    let follow_user: Option<u64> = std::env::var("DISCORD_USER_ID")
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let guild: Option<u64> = std::env::var("DISCORD_GUILD_ID")
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let channel: Option<u64> = std::env::var("DISCORD_CHANNEL_ID")
        .ok()
        .and_then(|s| s.trim().parse().ok());
    if follow_user.is_none() && (guild.is_none() || channel.is_none()) {
        anyhow::bail!("set DISCORD_USER_ID to follow your live voice channel (recommended), or both DISCORD_GUILD_ID + DISCORD_CHANNEL_ID for a fixed channel");
    }
    let secs: u64 = std::env::var("ZORD_SPIKE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let out_dir = PathBuf::from(
        std::env::var("ZORD_SPIKE_OUT").unwrap_or_else(|_| "discord-spike-out".into()),
    );
    std::fs::create_dir_all(&out_dir).context("create output dir")?;

    let recv = Receiver(Arc::new(Inner {
        ssrc_user: DashMap::new(),
        writers: Mutex::new(HashMap::new()),
        out_dir,
        ticks: AtomicU64::new(0),
        rtp_packets: AtomicU64::new(0),
        speaking_frames: AtomicU64::new(0),
        decoded_frames: AtomicU64::new(0),
        joined: AtomicBool::new(false),
    }));

    // GUILDS + GUILD_VOICE_STATES (both non-privileged) are all we need to join
    // and receive; non_privileged() includes them.
    let intents = GatewayIntents::non_privileged();

    // DecodeMode::Decode → songbird decrypts (DAVE) AND Opus-decodes to PCM, so
    // VoiceTick carries `decoded_voice`. This is the mode under test.
    let config = Config::default().decode_mode(DecodeMode::Decode(DecodeConfig::default()));

    let mut client = Client::builder(&token, intents)
        .event_handler(Bot {
            follow_user,
            guild: guild.map(GuildId::new),
            channel: channel.map(ChannelId::new),
            secs,
            recv,
        })
        .register_songbird_from_config(config)
        .await
        .context("build serenity client")?;

    tracing::info!("connecting to Discord gateway…");
    client.start().await.context("gateway client error")?;
    Ok(())
}
