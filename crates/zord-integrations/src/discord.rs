//! The real Discord [`Integration`] (Phase 30c), behind the `discord` feature.
//!
//! Built on the Phase 27 spike: a serenity gateway client + songbird voice
//! receiver, following a configured user into voice and turning Discord's
//! per-SSRC streams (decrypted through DAVE, decoded from Opus) into one
//! [`IntegrationEvent::ParticipantJoined`] per speaker. The followed user's own
//! stream is flagged `is_me` so the engine routes it to the "Me" track.
//!
//! Threading: serenity/songbird need a tokio runtime, but [`Integration`] is a
//! sync interface, so `start()` spawns a thread that owns a runtime and bridges
//! events into a std `mpsc` channel. `stop()` shuts the shard manager down.
//!
//! Runtime-verified by the user (a live DAVE call); compile-verified in CI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context as _, Result};
use dashmap::DashMap;
use serenity::all::{GatewayIntents, GuildId, UserId, VoiceState};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::http::Http;
use songbird::driver::{DecodeConfig, DecodeMode};
use songbird::model::payload::Speaking;
use songbird::{
    Config, CoreEvent, Event as SongbirdEvent, EventContext,
    EventHandler as VoiceEventHandler, SerenityInit,
};

use crate::integration::{Integration, IntegrationEvent, Participant};

/// Follows a Discord user into voice and yields one stream per participant.
pub struct DiscordProvider {
    token: String,
    follow_user_id: u64,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl DiscordProvider {
    pub fn new(token: impl Into<String>, follow_user_id: u64) -> Self {
        Self {
            token: token.into(),
            follow_user_id,
            shutdown: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }
}

impl Integration for DiscordProvider {
    fn name(&self) -> &str {
        "Discord"
    }

    fn start(&mut self) -> Result<Receiver<IntegrationEvent>> {
        let (ev_tx, ev_rx) = mpsc::channel();
        let token = self.token.clone();
        let follow = self.follow_user_id;
        let shutdown = self.shutdown.clone();

        self.handle = Some(
            thread::Builder::new()
                .name("zord-discord".into())
                .spawn(move || {
                    let rt = match tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(rt) => rt,
                        Err(e) => {
                            let _ = ev_tx.send(IntegrationEvent::Ended {
                                reason: format!("tokio runtime: {e}"),
                            });
                            return;
                        }
                    };
                    rt.block_on(run_client(token, follow, ev_tx, shutdown));
                })
                .context("spawn discord thread")?,
        );
        Ok(ev_rx)
    }

    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for DiscordProvider {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Build + run the serenity client until the followed user leaves, `stop()` is
/// called, or the connection drops. Always emits a final `Ended`.
async fn run_client(
    token: String,
    follow: u64,
    ev_tx: Sender<IntegrationEvent>,
    shutdown: Arc<AtomicBool>,
) {
    // Decrypt (DAVE) + Opus-decode received packets so VoiceTick carries PCM.
    let config = Config::default().decode_mode(DecodeMode::Decode(DecodeConfig::default()));
    let intents = GatewayIntents::non_privileged(); // GUILDS + GUILD_VOICE_STATES

    let bot = Bot {
        follow,
        ev_tx: ev_tx.clone(),
        joined: Arc::new(AtomicBool::new(false)),
    };

    let mut client = match Client::builder(&token, intents)
        .event_handler(bot)
        .register_songbird_from_config(config)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            let _ = ev_tx.send(IntegrationEvent::Ended {
                reason: format!("connect failed: {e} (check the bot token)"),
            });
            return;
        }
    };

    // Watchdog: shut the gateway down when stop() is called.
    let manager = client.shard_manager.clone();
    tokio::spawn(async move {
        while !shutdown.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        manager.shutdown_all().await;
    });

    if let Err(e) = client.start().await {
        let _ = ev_tx.send(IntegrationEvent::Ended {
            reason: format!("gateway error: {e}"),
        });
        return;
    }
    // start() returns on shutdown/disconnect; if a leave already sent Ended this
    // is harmless (the engine treats the first Ended as terminal).
    let _ = ev_tx.send(IntegrationEvent::Ended {
        reason: "disconnected".into(),
    });
}

/// Serenity gateway handler: join the followed user's voice channel and wire up
/// the voice receiver; end the session when they leave.
struct Bot {
    follow: u64,
    ev_tx: Sender<IntegrationEvent>,
    joined: Arc<AtomicBool>,
}

impl Bot {
    async fn try_join(&self, ctx: &Context, guild: GuildId, channel: serenity::all::ChannelId) {
        if self.joined.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(manager) = songbird::get(ctx).await else {
            let _ = self.ev_tx.send(IntegrationEvent::Ended {
                reason: "songbird not initialised".into(),
            });
            return;
        };
        match manager.join(guild, channel).await {
            Ok(call) => {
                let recv = VoiceReceiver::new(self.follow, self.ev_tx.clone(), ctx.http.clone(), guild);
                let mut call = call.lock().await;
                call.add_global_event(CoreEvent::SpeakingStateUpdate.into(), recv.clone());
                call.add_global_event(CoreEvent::VoiceTick.into(), recv.clone());
                call.add_global_event(CoreEvent::ClientDisconnect.into(), recv);
                tracing::info!("discord: joined voice + receiving");
            }
            Err(e) => {
                let _ = self.ev_tx.send(IntegrationEvent::Ended {
                    reason: format!("join failed: {e} (bot needs Connect permission)"),
                });
            }
        }
    }
}

#[async_trait]
impl EventHandler for Bot {
    async fn cache_ready(&self, ctx: Context, _guilds: Vec<GuildId>) {
        // If the followed user is already in a voice channel, join now.
        let target = ctx.cache.guilds().into_iter().find_map(|gid| {
            ctx.cache.guild(gid).and_then(|g| {
                g.voice_states
                    .get(&UserId::new(self.follow))
                    .and_then(|vs| vs.channel_id)
                    .map(|c| (gid, c))
            })
        });
        if let Some((g, c)) = target {
            self.try_join(&ctx, g, c).await;
        } else {
            tracing::info!("discord: waiting for user {} to join a voice channel", self.follow);
        }
    }

    async fn voice_state_update(&self, ctx: Context, _old: Option<VoiceState>, new: VoiceState) {
        if new.user_id != UserId::new(self.follow) {
            return;
        }
        match (new.guild_id, new.channel_id) {
            // Followed user is in a guild voice channel → follow them in.
            (Some(g), Some(c)) => self.try_join(&ctx, g, c).await,
            // Followed user left voice → end the session.
            (_, None) => {
                let _ = self.ev_tx.send(IntegrationEvent::Ended {
                    reason: "you left the voice channel".into(),
                });
            }
            // A non-guild voice channel (DM/group call) — not followed.
            (None, Some(_)) => {}
        }
    }
}

/// songbird voice receiver: maps SSRC→user (via speaking-state), announces each
/// participant once, and routes decoded PCM to its per-participant stream.
#[derive(Clone)]
struct VoiceReceiver(Arc<Inner>);

struct Inner {
    follow: u64,
    ev_tx: Sender<IntegrationEvent>,
    http: Arc<Http>,
    guild: GuildId,
    /// SSRC → the audio sender for that participant (present once announced).
    streams: DashMap<u32, Sender<Vec<f32>>>,
    /// Resolved display names, by user id (avoids repeat REST lookups).
    names: DashMap<u64, String>,
}

impl VoiceReceiver {
    fn new(follow: u64, ev_tx: Sender<IntegrationEvent>, http: Arc<Http>, guild: GuildId) -> Self {
        Self(Arc::new(Inner {
            follow,
            ev_tx,
            http,
            guild,
            streams: DashMap::new(),
            names: DashMap::new(),
        }))
    }

    /// Resolve a display name for `uid` (cached): prefer the server nickname,
    /// then the global name, then the username; fall back gracefully.
    async fn resolve_name(&self, uid: u64) -> String {
        if let Some(n) = self.0.names.get(&uid) {
            return n.clone();
        }
        let id = UserId::new(uid);
        let name = if let Ok(m) = self.0.http.get_member(self.0.guild, id).await {
            m.nick
                .clone()
                .or_else(|| m.user.global_name.clone())
                .unwrap_or(m.user.name)
        } else if let Ok(u) = self.0.http.get_user(id).await {
            u.global_name.clone().unwrap_or(u.name)
        } else {
            format!("User {uid}")
        };
        self.0.names.insert(uid, name.clone());
        name
    }

    /// Announce a participant for `ssrc`/`user_id` once, creating its stream.
    async fn announce(&self, ssrc: u32, user_id: u64) {
        if self.0.streams.contains_key(&ssrc) {
            return;
        }
        let name = self.resolve_name(user_id).await;
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        self.0.streams.insert(ssrc, tx);
        let _ = self.0.ev_tx.send(IntegrationEvent::ParticipantJoined {
            participant: Participant {
                key: user_id.to_string(),
                name,
                is_me: user_id == self.0.follow,
            },
            sample_rate: 48_000,
            audio: rx,
        });
    }
}

#[async_trait]
impl VoiceEventHandler for VoiceReceiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<SongbirdEvent> {
        match ctx {
            // The reliable SSRC→user mapping carrier — announce on first sight.
            EventContext::SpeakingStateUpdate(Speaking {
                ssrc,
                user_id: Some(uid),
                ..
            }) => {
                self.announce(*ssrc, uid.0).await;
            }
            // Decoded PCM per speaker, every 20 ms. Route to each announced
            // stream; silence between utterances is padded downstream (the
            // engine's per-speaker proc pads to wall-clock).
            EventContext::VoiceTick(tick) => {
                for (ssrc, data) in &tick.speaking {
                    let Some(pcm) = data.decoded_voice.as_ref() else {
                        continue;
                    };
                    if pcm.is_empty() {
                        continue;
                    }
                    let Some(tx) = self.0.streams.get(ssrc) else {
                        continue; // not yet mapped to a user — dropped (rare)
                    };
                    // Discord decodes to interleaved stereo i16 → mono f32.
                    let mono: Vec<f32> = pcm
                        .chunks_exact(2)
                        .map(|lr| (lr[0] as f32 + lr[1] as f32) * 0.5 / 32_768.0)
                        .collect();
                    let _ = tx.send(mono);
                }
            }
            EventContext::ClientDisconnect(_) => {} // stream just goes silent
            _ => {}
        }
        None
    }
}
