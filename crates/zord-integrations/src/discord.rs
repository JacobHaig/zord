//! The real Discord [`Integration`] (Phase 30c), behind the `discord` feature.
//!
//! Built on the Phase 27 spike: a serenity gateway client + songbird voice
//! receiver, following a configured user into voice and turning Discord's
//! per-SSRC streams (decrypted through DAVE, decoded from Opus) into one
//! [`IntegrationEvent::ParticipantJoined`] per speaker. The followed user's own
//! stream is flagged `is_me` so the engine can tag which uniform spk-N track is
//! the app user (styling/perspective only — every participant records alike).
//!
//! Threading: serenity/songbird need a tokio runtime, but [`Integration`] is a
//! sync interface, so `start()` spawns a thread that owns a runtime and bridges
//! events into a std `mpsc` channel. `stop()` shuts the shard manager down.
//!
//! Runtime-verified by the user (a live DAVE call); compile-verified in CI.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use dashmap::DashMap;
use serenity::all::{ChannelId, GatewayIntents, GuildId, UserId, VoiceState};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::http::Http;
use songbird::driver::{DecodeConfig, DecodeMode, Scheduler, SchedulerConfig};
use songbird::model::payload::Speaking;
use songbird::{
    Config, CoreEvent, Event as SongbirdEvent, EventContext, EventHandler as VoiceEventHandler,
    SerenityInit,
};

use crate::integration::{Integration, IntegrationEvent, Participant};

/// Follows a Discord user into voice and yields one stream per participant.
pub struct DiscordProvider {
    token: String,
    follow_user_id: u64,
    /// Posted in the voice channel's text chat on join (the consent /
    /// transparency signal, Phase 30e). `None` = announce off.
    announce: Option<String>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl DiscordProvider {
    pub fn new(token: impl Into<String>, follow_user_id: u64) -> Self {
        Self {
            token: token.into(),
            follow_user_id,
            announce: None,
            shutdown: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }

    /// Post `message` in the channel when the bot joins (`None` disables).
    pub fn with_announce(mut self, message: Option<String>) -> Self {
        self.announce = message;
        self
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
        let announce = self.announce.clone();
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
                                error: true,
                            });
                            return;
                        }
                    };
                    rt.block_on(run_client(token, follow, announce, ev_tx, shutdown));
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
    announce: Option<String>,
    ev_tx: Sender<IntegrationEvent>,
    shutdown: Arc<AtomicBool>,
) {
    // Decrypt (DAVE) + Opus-decode received packets so VoiceTick carries PCM.
    //
    // A per-session scheduler is REQUIRED here: songbird's default scheduler
    // is a process-global `OnceLock` whose core task spawns on the first tokio
    // runtime that touches it. This provider builds a fresh runtime per
    // recording, so the global's core dies with session #1's runtime and every
    // later voice join panics in `Scheduler::new_mixer` (disconnected channel,
    // songbird scheduler/mod.rs:85) — recordings after the first capture
    // nothing. A scheduler owned by this session's runtime lives and dies with
    // it instead.
    let config = Config::default()
        .decode_mode(DecodeMode::Decode(DecodeConfig::default()))
        .scheduler(Scheduler::new(SchedulerConfig::default()));
    let intents = GatewayIntents::non_privileged(); // GUILDS + GUILD_VOICE_STATES

    // Records the guild+channel the bot joined: the shutdown watchdog reads the
    // guild to *leave* before killing the gateway, and the Phase 50 late-joiner
    // logic reads the channel to tell "someone joined OUR channel" from voice
    // activity elsewhere in the guild.
    let joined_chan: Arc<std::sync::Mutex<Option<(GuildId, ChannelId)>>> =
        Arc::new(std::sync::Mutex::new(None));
    let bot = Bot {
        follow,
        announce,
        ev_tx: ev_tx.clone(),
        joined: Arc::new(AtomicBool::new(false)),
        joined_chan: joined_chan.clone(),
        // Start "long ago" so the very first late-joiner isn't debounced away.
        last_rejoin: Arc::new(std::sync::Mutex::new(
            Instant::now() - Duration::from_secs(3600),
        )),
        rejoin_gen: Arc::new(AtomicU64::new(0)),
        rejoin_active: Arc::new(AtomicBool::new(false)),
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
                error: true,
            });
            return;
        }
    };

    // Watchdog: on stop(), leave voice first (so Discord drops our voice
    // state immediately — killing just the gateway leaves it lingering and
    // the next session's join can time out against it), then shut the
    // gateway down.
    let manager = client.shard_manager.clone();
    let voice = client
        .data
        .read()
        .await
        .get::<songbird::SongbirdKey>()
        .cloned();
    tokio::spawn(async move {
        while !shutdown.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let guild = joined_chan
            .lock()
            .map(|g| g.map(|(g, _)| g))
            .unwrap_or(None);
        if let (Some(voice), Some(guild)) = (voice, guild) {
            let _ = tokio::time::timeout(Duration::from_secs(5), voice.remove(guild)).await;
        }
        manager.shutdown_all().await;
    });

    if let Err(e) = client.start().await {
        let _ = ev_tx.send(IntegrationEvent::Ended {
            reason: format!("gateway error: {e}"),
            error: true,
        });
        return;
    }
    // start() returns on shutdown/disconnect; if a leave already sent Ended this
    // is harmless (the engine treats the first Ended as terminal).
    let _ = ev_tx.send(IntegrationEvent::Ended {
        reason: "disconnected".into(),
        error: false,
    });
}

/// Serenity gateway handler: join the followed user's voice channel and wire up
/// the voice receiver; end the session when they leave.
struct Bot {
    follow: u64,
    announce: Option<String>,
    ev_tx: Sender<IntegrationEvent>,
    joined: Arc<AtomicBool>,
    /// Which guild+channel the bot joined. The guild is read by the shutdown
    /// watchdog so it can *leave* before killing the gateway (a voice state
    /// that's never left lingers server-side and can time out the NEXT
    /// session's join). The channel is read by the Phase 50 late-joiner logic
    /// to recognise a join *into our channel* vs. activity elsewhere.
    joined_chan: Arc<std::sync::Mutex<Option<(GuildId, ChannelId)>>>,
    /// When the last DAVE re-key (leave+rejoin) finished — the debounce floor
    /// (Phase 50): a join within `REJOIN_FLOOR` of the last rejoin is ignored
    /// because that recent rejoin already re-keyed to include everyone present.
    last_rejoin: Arc<std::sync::Mutex<Instant>>,
    /// Monotonic counter that collapses a *burst* of joiners into ONE rejoin:
    /// every qualifying join bumps it and schedules a delayed rejoin tagged
    /// with the bumped value; the rejoin only fires if the counter is still
    /// unchanged after the quiet window (i.e. nobody else joined meanwhile).
    rejoin_gen: Arc<AtomicU64>,
    /// `true` while a rejoin is *in flight* (between the start of `rejoin()` and
    /// its end). The `last_rejoin` floor alone can't prevent overlap because it
    /// isn't stamped until the rejoin finishes — a joiner arriving mid-rejoin
    /// would see the stale stamp, pass the floor, and schedule a SECOND
    /// leave+join that interleaves with the first (churn / a transient dead
    /// connection). A scheduled task bails if this is already set: the in-flight
    /// rejoin already re-keys everyone present, including this joiner.
    rejoin_active: Arc<AtomicBool>,
}

/// Phase 50 debounce tuning. `REJOIN_FLOOR` is the minimum spacing between two
/// re-keys: a fresh rejoin already includes everyone present, so joiners that
/// arrive right after one need no further action. `REJOIN_QUIET_WINDOW` is how
/// long we wait after the *last* joiner in a burst before rejoining once for
/// all of them (people trickling into a meeting must cost one rejoin, not ten).
const REJOIN_FLOOR: Duration = Duration::from_secs(8);
const REJOIN_QUIET_WINDOW: Duration = Duration::from_secs(3);

/// Does this `voice_state_update` represent a user *entering* our channel
/// (a fresh join or a move-in from elsewhere), as opposed to an in-channel
/// state change (mute/deafen/video toggle) that also fires `voice_state_update`?
///
/// `new_channel` is where the user is now; `old_channel` is where they were
/// (from serenity's cache, `None` if the cache had no prior state). `our`
/// is the channel the bot is recording.
///
/// Pure (no I/O) so it can be unit-tested — the live event just feeds it.
fn is_channel_join(
    old_channel: Option<ChannelId>,
    new_channel: Option<ChannelId>,
    our: ChannelId,
) -> bool {
    // Must be in OUR channel now…
    if new_channel != Some(our) {
        return false;
    }
    // …and NOT already have been (else it's a mute/deafen update in-channel).
    // If the cache gave us no prior state we can't distinguish, so we treat it
    // as a join — over-triggering here only costs at most one extra (debounced)
    // rejoin, whereas under-triggering would silently lose a late joiner.
    old_channel != Some(our)
}

/// Debounce floor decision (Phase 50): may we start a NEW rejoin now, given how
/// long ago the previous one finished? Pure for unit-testing.
fn may_rejoin(since_last_rejoin: Duration) -> bool {
    since_last_rejoin >= REJOIN_FLOOR
}

impl Bot {
    /// First connection to the followed user's channel. Guarded so it runs once;
    /// re-keys after this go through [`Bot::rejoin`].
    async fn try_join(&self, ctx: &Context, guild: GuildId, channel: ChannelId) {
        if self.joined.swap(true, Ordering::SeqCst) {
            return;
        }
        match self.join_call(ctx, guild, channel, false).await {
            Ok(()) => {
                if let Ok(mut g) = self.joined_chan.lock() {
                    *g = Some((guild, channel));
                }
                tracing::info!("discord: joined voice + receiving");
                // Consent/transparency: post in the voice channel's text chat
                // (30e). Best-effort — a missing Send-Messages permission must
                // not break the recording.
                if let Some(msg) = self.announce.clone() {
                    let http = ctx.http.clone();
                    tokio::spawn(async move {
                        if let Err(e) = channel.say(&http, msg).await {
                            tracing::warn!("discord: recording announcement failed: {e}");
                        }
                    });
                }
            }
            Err(JoinError::Timeout) => {
                let _ = self.ev_tx.send(IntegrationEvent::Ended {
                    reason: "joining the voice channel timed out — try recording again".into(),
                    error: true,
                });
            }
            Err(JoinError::Failed(e)) => {
                let _ = self.ev_tx.send(IntegrationEvent::Ended {
                    reason: format!("join failed: {e} (bot needs Connect permission)"),
                    error: true,
                });
            }
            Err(JoinError::NoSongbird) => {
                let _ = self.ev_tx.send(IntegrationEvent::Ended {
                    reason: "songbird not initialised".into(),
                    error: true,
                });
            }
        }
    }

    /// The shared join sequence used by both the first join and a Phase 50
    /// re-key. Registers the voice handlers BEFORE joining: Discord delivers the
    /// Speaking events that carry the SSRC→user mapping in the first moments of
    /// the connection. Registering after `join` returned (the old order) raced
    /// them — and a lost mapping was a session that captured nothing (live-test
    /// finding). Mirrors Songbird::join, with the handlers added between
    /// `get_or_insert` and `Call::join`.
    ///
    /// `rejoin` selects behaviour that must differ between the two callers:
    /// this method ALWAYS builds and registers a FRESH [`VoiceReceiver`] on the
    /// call so the handlers are live on the new connection after a re-key — its
    /// empty SSRC/name/pending maps are correct, because post-rejoin every
    /// participant re-announces under their stable user-id key and the engine
    /// maps those back to the existing tracks (Change 2). The same `ev_tx` is
    /// reused so ParticipantJoined events keep flowing to the same session.
    async fn join_call(
        &self,
        ctx: &Context,
        guild: GuildId,
        channel: ChannelId,
        rejoin: bool,
    ) -> Result<(), JoinError> {
        let Some(manager) = songbird::get(ctx).await else {
            return Err(JoinError::NoSongbird);
        };
        let call = manager.get_or_insert(guild);
        let recv = VoiceReceiver::new(self.follow, self.ev_tx.clone(), ctx.http.clone(), guild);
        {
            let mut c = call.lock().await;
            // On a rejoin the previous receiver's handlers are still attached to
            // the same Call; drop them first so we don't double-handle ticks
            // (each VoiceTick would otherwise be processed twice).
            if rejoin {
                c.remove_all_global_events();
            }
            c.add_global_event(CoreEvent::SpeakingStateUpdate.into(), recv.clone());
            c.add_global_event(CoreEvent::VoiceTick.into(), recv.clone());
            c.add_global_event(CoreEvent::ClientDisconnect.into(), recv);
        }
        // Bound the join: a wedged driver otherwise leaves the session "live"
        // while capturing nothing, and the user only finds out afterwards.
        let joined = tokio::time::timeout(Duration::from_secs(20), async {
            let stage_1 = {
                let mut c = call.lock().await;
                c.join(channel).await
            };
            match stage_1 {
                Ok(chan) => chan.await,
                Err(e) => Err(e),
            }
        })
        .await;
        match joined {
            Err(_) => Err(JoinError::Timeout),
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(JoinError::Failed(e.to_string())),
        }
    }

    /// Phase 50: a participant joined our channel after we connected. DAVE's
    /// MLS group never gained a decryptor for them (songbird 0.6.0 race →
    /// permanent `InvalidPacket` for that user). Forcing a fresh DAVE epoch
    /// fixes it: leaving and rejoining produces a new MLS Welcome that includes
    /// ALL currently-present members, so everyone — the late joiner included —
    /// becomes decryptable again.
    ///
    /// CRITICAL: the bot's own `leave()` here must NOT be read as "followed user
    /// left" — we never send a *benign* `Ended` for it, and serenity's
    /// `voice_state_update` for the *bot's* own user id is filtered out in the
    /// handler.
    ///
    /// Once `leave()` has run the pre-rejoin call is GONE — so a failed re-join
    /// is NOT recoverable: the bot has left, isn't connected, and would capture
    /// nothing while `drive_session` kept looping (the session would hang as
    /// "recording" until the user manually stopped). We therefore FAIL LOUD on a
    /// rejoin-join failure: send `Ended { error: true }` so the engine ends the
    /// session and the user is told to restart, instead of hanging silently.
    async fn rejoin(&self, ctx: &Context, guild: GuildId, channel: ChannelId) {
        // Mark in-flight + stamp the floor at the START (not the end): any
        // joiner arriving during this rejoin must see "active"/"recent" and bail
        // rather than schedule an overlapping leave+join. Cleared on the way out.
        self.rejoin_active.store(true, Ordering::SeqCst);
        if let Ok(mut t) = self.last_rejoin.lock() {
            *t = Instant::now();
        }
        // Surface the brief gap so the user understands the ~1–3 s of silence.
        let _ = self.ev_tx.send(IntegrationEvent::Notice(
            "Someone joined — re-syncing Discord audio…".into(),
        ));
        let Some(manager) = songbird::get(ctx).await else {
            self.rejoin_active.store(false, Ordering::SeqCst);
            return;
        };
        if let Some(call) = manager.get(guild) {
            let mut c = call.lock().await;
            let _ = c.leave().await;
        }
        // Brief settle before reconnecting so Discord drops the old voice state.
        tokio::time::sleep(Duration::from_millis(300)).await;
        match self.join_call(ctx, guild, channel, true).await {
            Ok(()) => {
                tracing::info!("discord: re-keyed DAVE via rejoin (late-joiner capture)");
            }
            Err(e) => {
                // leave() already ran → the call is gone and we failed to get it
                // back. End the session loudly rather than hang capturing nothing.
                tracing::warn!("discord: rejoin to re-key DAVE failed: {e:?}");
                let _ = self.ev_tx.send(IntegrationEvent::Ended {
                    reason: "lost the voice connection while re-syncing — stop and start recording again".into(),
                    error: true,
                });
            }
        }
        // Re-stamp at the end too so the floor measures from when we're settled,
        // then clear the in-flight flag (order: stamp before clear, so a joiner
        // that observes !active also sees a fresh stamp).
        if let Ok(mut t) = self.last_rejoin.lock() {
            *t = Instant::now();
        }
        self.rejoin_active.store(false, Ordering::SeqCst);
    }

    /// A user entered our channel after we connected: schedule a debounced
    /// re-key. Returns immediately; the actual rejoin runs on a spawned task
    /// after a quiet window (so a burst of joiners collapses to one rejoin).
    fn schedule_rejoin(&self, ctx: &Context, guild: GuildId, channel: ChannelId) {
        // A rejoin already in flight re-keys everyone currently present
        // (including this joiner) — scheduling another would interleave a second
        // leave+join with the first. Bail.
        if self.rejoin_active.load(Ordering::SeqCst) {
            tracing::debug!("discord: skip rejoin — one already in flight");
            return;
        }
        // Debounce floor: a recent rejoin already re-keyed to include everyone
        // present, so skip.
        let since = self
            .last_rejoin
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or(Duration::MAX);
        if !may_rejoin(since) {
            tracing::debug!("discord: skip rejoin — re-keyed {since:?} ago (< floor)");
            return;
        }
        // Quiet-window debounce: bump the generation; this task only fires if
        // no later joiner bumped it again during the window.
        let my_gen = self.rejoin_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let gen = self.rejoin_gen.clone();
        let ev_tx = self.ev_tx.clone();
        let last_rejoin = self.last_rejoin.clone();
        let rejoin_active = self.rejoin_active.clone();
        let follow = self.follow;
        let announce = self.announce.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(REJOIN_QUIET_WINDOW).await;
            if gen.load(Ordering::SeqCst) != my_gen {
                // A newer joiner reset the window; that later task will rejoin.
                return;
            }
            // A rejoin may have started during our wait (e.g. an earlier burst's
            // task fired): re-check the in-flight guard before committing.
            if rejoin_active.load(Ordering::SeqCst) {
                return;
            }
            // Re-build a transient Bot view to run the rejoin (cheap clones).
            // It SHARES `rejoin_active`/`last_rejoin`/`rejoin_gen` so the guards
            // above stay coherent with the real handler.
            let bot = Bot {
                follow,
                announce,
                ev_tx,
                joined: Arc::new(AtomicBool::new(true)),
                joined_chan: Arc::new(std::sync::Mutex::new(Some((guild, channel)))),
                last_rejoin,
                rejoin_gen: gen,
                rejoin_active,
            };
            bot.rejoin(&ctx, guild, channel).await;
        });
    }
}

/// Internal join outcome so [`Bot::try_join`] and [`Bot::rejoin`] can react
/// differently (try_join surfaces `Ended`; rejoin only logs).
#[derive(Debug)]
enum JoinError {
    NoSongbird,
    Timeout,
    Failed(String),
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
            tracing::info!(
                "discord: waiting for user {} to join a voice channel",
                self.follow
            );
        }
    }

    async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        // --- Followed-user logic (unchanged): drives session start/end. ---
        if new.user_id == UserId::new(self.follow) {
            match (new.guild_id, new.channel_id) {
                // Followed user is in a guild voice channel → follow them in.
                (Some(g), Some(c)) => self.try_join(&ctx, g, c).await,
                // Followed user left voice → end the session.
                (_, None) => {
                    let _ = self.ev_tx.send(IntegrationEvent::Ended {
                        reason: "you left the voice channel".into(),
                        error: false,
                    });
                }
                // A non-guild voice channel (DM/group call) — not followed.
                (None, Some(_)) => {}
            }
            return;
        }

        // --- Phase 50: late-joiner detection for ANY other user. ---
        // Never react to the bot's own voice state — its leave()/join() during a
        // re-key would otherwise trigger an endless rejoin loop.
        if new.user_id == ctx.cache.current_user().id {
            return;
        }
        // Only when we have actually joined a channel of our own to compare to.
        let Some((our_guild, our_channel)) = self.joined_chan.lock().ok().and_then(|g| *g) else {
            return;
        };
        // Same guild as our recording (voice activity in other guilds is moot).
        if new.guild_id != Some(our_guild) {
            return;
        }
        // A user *entering* our channel (join or move-in) — not an in-channel
        // mute/deafen update. `old` is serenity's cached prior state (may be
        // None; `is_channel_join` then treats it as a join, see its doc).
        if is_channel_join(old.and_then(|o| o.channel_id), new.channel_id, our_channel) {
            self.schedule_rejoin(&ctx, our_guild, our_channel);
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
    /// Audio ticks seen for SSRCs that have no user mapping yet — after a
    /// grace period they're announced unnamed (see `announce_unmapped`).
    pending: DashMap<u32, u32>,
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
            pending: DashMap::new(),
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
            // Already streaming. If it was announced *unmapped* (audio beat
            // this speaking event), upgrade its label now that we know who —
            // `names` remembers which users we've already resolved/renamed.
            if !self.0.names.contains_key(&user_id) {
                let name = self.resolve_name(user_id).await;
                let _ = self.0.ev_tx.send(IntegrationEvent::ParticipantRenamed {
                    key: format!("ssrc-{ssrc}"),
                    name,
                });
            }
            return;
        }
        self.0.pending.remove(&ssrc);
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

    /// Audio arrived for an SSRC whose speaking event (the user mapping) never
    /// came: announce an unnamed participant so the audio is captured anyway.
    /// The empty name falls back to "Speaker N" in the UI and is upgraded by
    /// [`Self::announce`] when the mapping does show up. (Trade-off: if this
    /// fires for the followed user, their audio lands on a speaker track, not
    /// "Me" — better than the silent loss it replaces.)
    //
    // Phase 50 known limitation (accepted): this keys on `ssrc-N`, not the
    // user id. After a rejoin a participant gets a fresh SSRC, so a joiner whose
    // SSRC→user mapping never arrives lands on a SECOND `ssrc-*` track instead
    // of merging back into their pre-rejoin one — degraded (a split track), not
    // corrupt, and inherent to the unmapped fallback. The mapped path (`announce`,
    // keyed by user id) merges correctly.
    fn announce_unmapped(&self, ssrc: u32) {
        if self.0.streams.contains_key(&ssrc) {
            return;
        }
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        self.0.streams.insert(ssrc, tx);
        let _ = self.0.ev_tx.send(IntegrationEvent::ParticipantJoined {
            participant: Participant {
                key: format!("ssrc-{ssrc}"),
                name: String::new(),
                is_me: false,
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
                    if !self.0.streams.contains_key(ssrc) {
                        // No user mapping yet. Give the speaking event ~1 s
                        // (50 ticks) to deliver it — it usually lands right at
                        // join — then announce unnamed rather than ever
                        // dropping a speaker's audio outright.
                        let ticks = {
                            let mut p = self.0.pending.entry(*ssrc).or_insert(0);
                            *p += 1;
                            *p
                        };
                        if ticks < 50 {
                            continue;
                        }
                        self.announce_unmapped(*ssrc);
                    }
                    let Some(tx) = self.0.streams.get(ssrc) else {
                        continue;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(n: u64) -> ChannelId {
        ChannelId::new(n)
    }

    // --- is_channel_join: distinguishing a join INTO our channel from an
    // in-channel state update or unrelated voice activity (Phase 50). ---

    #[test]
    fn join_into_our_channel_from_outside_triggers() {
        // Was not in our channel (in another, or not in voice) → now in it.
        assert!(is_channel_join(Some(ch(2)), Some(ch(1)), ch(1)));
        assert!(is_channel_join(None, Some(ch(1)), ch(1)));
    }

    #[test]
    fn in_channel_state_update_does_not_trigger() {
        // Already in our channel, still in it: a mute/deafen/video toggle.
        assert!(!is_channel_join(Some(ch(1)), Some(ch(1)), ch(1)));
    }

    #[test]
    fn activity_in_other_channel_does_not_trigger() {
        // Joined some other channel entirely.
        assert!(!is_channel_join(None, Some(ch(2)), ch(1)));
        assert!(!is_channel_join(Some(ch(3)), Some(ch(2)), ch(1)));
    }

    #[test]
    fn leaving_does_not_trigger() {
        // Left voice (new channel None) — never a join into ours.
        assert!(!is_channel_join(Some(ch(1)), None, ch(1)));
    }

    #[test]
    fn unknown_prior_state_treated_as_join() {
        // Cache had no `old` (None) and they're now in our channel: we can't
        // prove it's not a fresh join, so we trigger (over-trigger is one extra
        // debounced rejoin; under-trigger silently loses the late joiner).
        assert!(is_channel_join(None, Some(ch(1)), ch(1)));
    }

    // --- may_rejoin: the debounce FLOOR between consecutive re-keys. ---

    #[test]
    fn rejoin_blocked_within_floor() {
        assert!(!may_rejoin(Duration::from_secs(0)));
        assert!(!may_rejoin(REJOIN_FLOOR - Duration::from_millis(1)));
    }

    #[test]
    fn rejoin_allowed_after_floor() {
        assert!(may_rejoin(REJOIN_FLOOR));
        assert!(may_rejoin(REJOIN_FLOOR + Duration::from_secs(60)));
    }
}
