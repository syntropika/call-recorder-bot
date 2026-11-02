mod config;
mod transcript;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use config::{Configs, GuildConfig};
use serenity::{
    all::{
        ChannelId, ChannelType, Command, CommandInteraction, CommandOptionType, Context,
        CreateCommand, CreateCommandOption, CreateInteractionResponse,
        CreateInteractionResponseFollowup, CreateInteractionResponseMessage, EventHandler,
        GatewayIntents, Guild, GuildId, Interaction, InteractionContext, Permissions, Ready,
    },
    async_trait,
    cache::Cache,
    http::Http,
};
use songbird::{
    Config, CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit,
    Songbird,
    driver::{Channels, DecodeConfig, DecodeMode, SampleRate},
};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::{
    engines::CaptureEngine,
    mixer,
    recording::{ActiveRecording, AudioSink, Recorder},
    storage::RecordingOrigin,
};

struct RecordingSession {
    channel_id: u64,
    notice_channel_id: u64,
    recording: ActiveRecording,
}

struct State {
    configs: Configs,
    sessions: HashMap<u64, RecordingSession>,
}

struct App {
    state: Mutex<State>,
    manager: Arc<Songbird>,
    recorder: Arc<Recorder>,
    transcription_enabled: bool,
    shutting_down: AtomicBool,
    data_dir: PathBuf,
}

impl App {
    async fn reconcile(&self, cache: &Cache, http: &Arc<Http>, guild_id: GuildId) -> Result<()> {
        let mut state = self.state.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Ok(());
        }
        let configured = state.configs.get(&guild_id.get()).copied();
        let people = configured.map(|cfg| count_people(cache, guild_id, cfg.voice_channel_id));

        let should_stop = state.sessions.get(&guild_id.get()).is_some_and(|session| {
            configured.is_none_or(|cfg| {
                cfg.voice_channel_id != session.channel_id
                    || cfg.notice_channel_id != session.notice_channel_id
            }) || people == Some(Some(0))
        });
        if should_stop {
            self.stop_session(http, guild_id, &mut state).await;
        }

        // A failed disconnect or gateway reconnect can outlive the recording session.
        // Keep reconciling the voice connection until Discord confirms the bot has left.
        if !should_stop && (configured.is_none() || people == Some(Some(0))) {
            let connected = cache.guild(guild_id).is_some_and(|guild| {
                guild
                    .voice_states
                    .get(&cache.current_user().id)
                    .is_some_and(|voice| voice.channel_id.is_some())
            });
            if connected || self.manager.get(guild_id).is_some() {
                self.manager.get_or_insert(guild_id);
                self.manager
                    .remove(guild_id)
                    .await
                    .context("could not disconnect an idle voice connection")?;
            }
        }

        if let (Some(cfg), Some(count)) = (configured, people.flatten())
            && count > 0
            && !state.sessions.contains_key(&guild_id.get())
        {
            self.start_session(http, guild_id, cfg, &mut state).await?;
        }
        Ok(())
    }

    async fn start_session(
        &self,
        http: &Http,
        guild_id: GuildId,
        cfg: GuildConfig,
        state: &mut State,
    ) -> Result<()> {
        let recording = self
            .recorder
            .start(RecordingOrigin::new("discord", guild_id.get())?)?;
        let voice_channel = ChannelId::new(cfg.voice_channel_id);
        let notice_channel = ChannelId::new(cfg.notice_channel_id);
        if let Err(err) = notice_channel
            .say(
                http,
                format!(
                    "🔴 Recording started in <#{}>. Everyone in that voice channel is being recorded. Separate speaker tracks are stored.{}",
                    cfg.voice_channel_id,
                    if self.transcription_enabled {
                        " The audio will be sent to a transcription provider after recording ends, and the completed transcript will be posted in this channel."
                    } else {
                        ""
                    }
                ),
            )
            .await
        {
            self.recorder.abort(recording).await;
            return Err(err).context("could not post the recording notice");
        }

        let call = self.manager.get_or_insert(guild_id);
        {
            let mut call = call.lock().await;
            let audio_receiver = AudioReceiver {
                sink: recording.sink(),
            };
            call.add_global_event(
                CoreEvent::SpeakingStateUpdate.into(),
                audio_receiver.clone(),
            );
            call.add_global_event(CoreEvent::VoiceTick.into(), audio_receiver);
        }
        if let Err(join_error) = self.manager.join(guild_id, voice_channel).await {
            recording.sink().deactivate();
            let _ = self.manager.remove(guild_id).await;
            self.recorder.abort(recording).await;
            let _ = notice_channel
                .say(
                    http,
                    "Recording could not start because the bot could not join the voice channel.",
                )
                .await;
            return Err(anyhow!("could not join voice channel: {join_error}"));
        }
        info!(guild = guild_id.get(), "recording started");
        state.sessions.insert(
            guild_id.get(),
            RecordingSession {
                channel_id: cfg.voice_channel_id,
                notice_channel_id: cfg.notice_channel_id,
                recording,
            },
        );
        Ok(())
    }

    async fn stop_session(&self, http: &Arc<Http>, guild_id: GuildId, state: &mut State) {
        let Some(session) = state.sessions.remove(&guild_id.get()) else {
            return;
        };
        session.recording.sink().deactivate();
        if let Err(err) = self.manager.remove(guild_id).await {
            warn!(guild = guild_id.get(), %err, "could not leave voice channel");
        }
        self.recorder
            .finish(
                session.recording,
                Some(Arc::new(transcript::DiscordTranscriptDelivery::new(
                    http.clone(),
                    ChannelId::new(session.notice_channel_id),
                    ChannelId::new(session.channel_id),
                ))),
            )
            .await;
        if let Err(err) = ChannelId::new(session.notice_channel_id)
            .say(
                http,
                format!("⏹️ Recording ended in <#{}>.", session.channel_id),
            )
            .await
        {
            warn!(guild = guild_id.get(), %err, "could not post the recording end notice");
        }
    }

    async fn shutdown(&self, http: &Arc<Http>) {
        self.shutting_down.store(true, Ordering::Release);
        let mut state = self.state.lock().await;
        let guilds: Vec<_> = state.sessions.keys().copied().collect();
        for guild in guilds {
            self.stop_session(http, GuildId::new(guild), &mut state)
                .await;
        }
        drop(state);
        self.recorder.wait_for_processing().await;
    }
}

fn count_people(cache: &Cache, guild_id: GuildId, voice_channel_id: u64) -> Option<usize> {
    let guild = cache.guild(guild_id)?;
    let own_id = cache.current_user().id;
    Some(
        guild
            .voice_states
            .values()
            .filter(|voice| {
                voice
                    .channel_id
                    .is_some_and(|id| id.get() == voice_channel_id)
            })
            .filter(|voice| voice.user_id != own_id)
            .filter(|voice| {
                !voice
                    .member
                    .as_ref()
                    .map(|member| member.user.bot)
                    .or_else(|| {
                        guild
                            .members
                            .get(&voice.user_id)
                            .map(|member| member.user.bot)
                    })
                    .or_else(|| cache.user(voice.user_id).map(|user| user.bot))
                    .unwrap_or(false)
            })
            .count(),
    )
}

#[derive(Clone)]
struct AudioReceiver {
    sink: AudioSink,
}

#[async_trait]
impl VoiceEventHandler for AudioReceiver {
    async fn act(&self, event: &EventContext<'_>) -> Option<Event> {
        match event {
            EventContext::SpeakingStateUpdate(update) => {
                if let Some(user_id) = update.user_id {
                    self.sink
                        .identify(
                            update.ssrc,
                            user_id.0.to_string(),
                            format!("Discord user <@{}>", user_id.0),
                        )
                        .await;
                }
            }
            EventContext::VoiceTick(tick) => {
                let mixed = mixer::mix_tick(
                    tick.speaking
                        .values()
                        .filter_map(|voice| voice.decoded_voice.as_deref()),
                );
                let speakers = tick
                    .speaking
                    .iter()
                    .filter_map(|(ssrc, voice)| {
                        voice
                            .decoded_voice
                            .as_ref()
                            .map(|audio| (*ssrc, audio.clone()))
                    })
                    .collect();
                self.sink.capture(mixed, speakers);
            }
            _ => {}
        }
        None
    }
}

struct Handler {
    app: Arc<App>,
    periodic_started: AtomicBool,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(bot = %ready.user.name, "connected to Discord");
        if let Err(err) = Command::set_global_commands(&ctx.http, commands()).await {
            error!(%err, "could not register slash commands");
        }
        if !self.periodic_started.swap(true, Ordering::AcqRel) {
            let app = self.app.clone();
            tokio::spawn(async move {
                let mut timer = tokio::time::interval(Duration::from_secs(30));
                loop {
                    timer.tick().await;
                    let guilds: HashSet<u64> = {
                        let state = app.state.lock().await;
                        state
                            .configs
                            .keys()
                            .chain(state.sessions.keys())
                            .copied()
                            .chain(app.manager.iter().map(|(guild, _)| guild.0.get()))
                            .chain(ctx.cache.guilds().into_iter().map(|guild| guild.get()))
                            .collect()
                    };
                    for guild in guilds {
                        if let Err(err) = app
                            .reconcile(&ctx.cache, &ctx.http, GuildId::new(guild))
                            .await
                        {
                            warn!(guild, %err, "could not reconcile recording state");
                        }
                    }
                }
            });
        }
    }

    async fn guild_create(&self, ctx: Context, guild: Guild, _is_new: Option<bool>) {
        if let Err(err) = self.app.reconcile(&ctx.cache, &ctx.http, guild.id).await {
            warn!(guild = guild.id.get(), %err, "could not reconcile recording state");
        }
    }

    async fn voice_state_update(
        &self,
        ctx: Context,
        _old: Option<serenity::all::VoiceState>,
        new: serenity::all::VoiceState,
    ) {
        if let Some(guild_id) = new.guild_id
            && let Err(err) = self.app.reconcile(&ctx.cache, &ctx.http, guild_id).await
        {
            warn!(guild = guild_id.get(), %err, "could not reconcile recording state");
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };
        if let Err(err) = handle_command(&self.app, &ctx, &command).await {
            error!(%err, "slash command failed");
            let _ = respond(&ctx, &command, "The command failed. Check the bot logs.").await;
        }
    }
}

fn commands() -> Vec<CreateCommand> {
    let admin = Permissions::MANAGE_GUILD;
    vec![
        CreateCommand::new("record-setup")
            .description("Automatically record a voice channel while people are present")
            .contexts(vec![InteractionContext::Guild])
            .default_member_permissions(admin)
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Channel,
                    "voice",
                    "Voice channel to record",
                )
                .channel_types(vec![ChannelType::Voice])
                .required(true),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Channel,
                    "notices",
                    "Channel for notices and transcripts; defaults to the voice channel",
                )
                .channel_types(vec![ChannelType::Text, ChannelType::Voice]),
            ),
        CreateCommand::new("record-disable")
            .description("Disable automatic recording and end any current recording")
            .contexts(vec![InteractionContext::Guild])
            .default_member_permissions(admin),
        CreateCommand::new("record-status")
            .description("Show the configured channel and recording status")
            .contexts(vec![InteractionContext::Guild])
            .default_member_permissions(admin),
    ]
}

async fn handle_command(app: &App, ctx: &Context, command: &CommandInteraction) -> Result<()> {
    let Some(guild_id) = command.guild_id else {
        respond(ctx, command, "This command must be used in a server.").await?;
        return Ok(());
    };
    let authorized = command
        .member
        .as_ref()
        .and_then(|member| member.permissions)
        .is_some_and(|permissions| {
            permissions.contains(Permissions::MANAGE_GUILD)
                || permissions.contains(Permissions::ADMINISTRATOR)
        });
    if !authorized {
        respond(ctx, command, "You need the Manage Server permission.").await?;
        return Ok(());
    }
    match command.data.name.as_str() {
        "record-setup" => {
            let option = |name: &str| -> Result<ChannelId> {
                command
                    .data
                    .options
                    .iter()
                    .find(|option| option.name == name)
                    .and_then(|option| option.value.as_channel_id())
                    .ok_or_else(|| anyhow!("missing {name} channel"))
            };
            let voice = option("voice")?;
            let notices = command
                .data
                .options
                .iter()
                .find(|option| option.name == "notices")
                .and_then(|option| option.value.as_channel_id())
                .unwrap_or(voice);
            let cfg = GuildConfig {
                voice_channel_id: voice.get(),
                notice_channel_id: notices.get(),
            };
            {
                let mut state = app.state.lock().await;
                let mut updated = state.configs.clone();
                updated.insert(guild_id.get(), cfg);
                config::save(&app.data_dir.join("config.json"), &updated).await?;
                state.configs = updated;
            }
            respond(ctx, command, &format!("Automatic recording configured for <#{}>. Recording notices and completed transcripts will appear in <#{}>.", voice.get(), notices.get())).await?;
            if let Err(err) = app.reconcile(&ctx.cache, &ctx.http, guild_id).await {
                warn!(guild = guild_id.get(), %err, "configuration saved, but recording could not start");
                command.create_followup(
                    &ctx.http,
                    CreateInteractionResponseFollowup::new()
                        .content("Configuration was saved, but recording could not start. Check the bot's channel permissions and logs.")
                        .ephemeral(true),
                ).await?;
            }
        }
        "record-disable" => {
            {
                let mut state = app.state.lock().await;
                let mut updated = state.configs.clone();
                updated.remove(&guild_id.get());
                config::save(&app.data_dir.join("config.json"), &updated).await?;
                state.configs = updated;
            }
            respond(ctx, command, "Automatic recording disabled.").await?;
            app.reconcile(&ctx.cache, &ctx.http, guild_id).await?;
        }
        "record-status" => {
            let state = app.state.lock().await;
            let message = match state.configs.get(&guild_id.get()) {
                Some(cfg) => format!(
                    "Voice channel: <#{}>. Notices: <#{}>. Recording: {}. Transcription: {}.",
                    cfg.voice_channel_id,
                    cfg.notice_channel_id,
                    if state.sessions.contains_key(&guild_id.get()) {
                        "active"
                    } else {
                        "idle"
                    },
                    if app.transcription_enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                ),
                None => "Automatic recording is disabled.".to_owned(),
            };
            respond(ctx, command, &message).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn respond(ctx: &Context, command: &CommandInteraction, message: &str) -> Result<()> {
    command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(message)
                    .ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}

pub struct DiscordEngine {
    token: String,
    data_dir: PathBuf,
    transcription_enabled: bool,
}

impl DiscordEngine {
    pub fn new(token: String, data_dir: PathBuf, transcription_enabled: bool) -> Self {
        Self {
            token,
            data_dir,
            transcription_enabled,
        }
    }
}

#[async_trait]
impl CaptureEngine for DiscordEngine {
    async fn run(&self, recorder: Arc<Recorder>) -> Result<()> {
        let configs = config::load(&self.data_dir.join("config.json")).await?;
        let voice_config = Config::default().decode_mode(DecodeMode::Decode(DecodeConfig::new(
            Channels::Mono,
            SampleRate::Hz48000,
        )));
        let manager = Songbird::serenity_from_config(voice_config);
        let app = Arc::new(App {
            state: Mutex::new(State {
                configs,
                sessions: HashMap::new(),
            }),
            manager: manager.clone(),
            recorder,
            transcription_enabled: self.transcription_enabled,
            shutting_down: AtomicBool::new(false),
            data_dir: self.data_dir.clone(),
        });
        let handler = Handler {
            app: app.clone(),
            periodic_started: AtomicBool::new(false),
        };
        let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;
        let mut client = serenity::Client::builder(&self.token, intents)
            .event_handler(handler)
            .register_songbird_with(manager)
            .await?;
        let shard_manager = client.shard_manager.clone();
        let run_result = tokio::select! {
            result = client.start() => result,
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown requested");
                Ok(())
            },
            _ = termination_signal() => {
                info!("termination requested");
                Ok(())
            },
        };
        app.shutdown(&client.http).await;
        shard_manager.shutdown_all().await;
        run_result?;
        Ok(())
    }
}

#[cfg(unix)]
async fn termination_signal() {
    let mut stream = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("could not listen for SIGTERM");
    stream.recv().await;
}

#[cfg(not(unix))]
async fn termination_signal() {
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests;
