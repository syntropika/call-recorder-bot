use super::*;
use crate::{processing::NoopProcessor, storage::LocalWavStorage};
use serenity::all::{GuildCreateEvent, UserId, VoiceState};

#[tokio::test]
async fn empty_channel_removes_a_voice_call_without_an_active_recording() -> Result<()> {
    use serenity::{
        futures::{StreamExt, channel::mpsc},
        gateway::VoiceGatewayManager,
    };

    let cache = Cache::default();
    let guild_id = GuildId::new(42);
    let bot_id = cache.current_user().id;
    let mut guild = Guild::default();
    guild.id = guild_id;
    let voice: VoiceState = serde_json::from_value(serde_json::json!({
        "guild_id": "42", "channel_id": "10", "user_id": bot_id.to_string(),
        "session_id": "test", "deaf": false, "mute": false,
        "self_deaf": false, "self_mute": false, "self_video": false, "suppress": false
    }))?;
    guild.voice_states.insert(bot_id, voice);
    let mut event: GuildCreateEvent = serde_json::from_value(serde_json::to_value(guild)?)?;
    cache.update(&mut event);
    let manager = Songbird::serenity();
    manager.initialise_client_data(1, UserId::new(bot_id.get()));
    let (sender, mut receiver) = mpsc::unbounded();
    manager.register_shard(0, sender).await;
    manager.get_or_insert(guild_id);
    let app = App {
        state: Mutex::new(State {
            configs: HashMap::from([(
                42,
                GuildConfig {
                    voice_channel_id: 10,
                    notice_channel_id: 11,
                },
            )]),
            sessions: HashMap::new(),
        }),
        manager: manager.clone(),
        recorder: Arc::new(Recorder::new(
            Arc::new(LocalWavStorage::new(PathBuf::from("unused"))),
            Arc::new(NoopProcessor),
        )),
        transcription_enabled: false,
        shutting_down: AtomicBool::new(false),
        data_dir: PathBuf::from("unused"),
    };
    assert_eq!(count_people(&cache, guild_id, 10), Some(0));
    // The second pass has only Discord's cached connection, without a Songbird call.
    for _ in 0..2 {
        app.reconcile(&cache, &Arc::new(Http::new("test-token")), guild_id)
            .await?;
        assert!(
            manager.get(guild_id).is_none(),
            "the bot must leave even when a previous stop already removed its recording session"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), receiver.next())
                .await?
                .is_some()
        );
    }
    Ok(())
}

#[tokio::test]
async fn failed_disconnect_is_retried_after_the_last_person_leaves() -> Result<()> {
    use serenity::{
        all::{Message, VoiceStateUpdateEvent},
        futures::{StreamExt, channel::mpsc},
        gateway::{ShardRunnerMessage, VoiceGatewayManager},
        http::HttpBuilder,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    let root = std::env::temp_dir().join(format!(
        "call-recorder-leave-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = axum::Router::new().route(
        "/api/v10/channels/11/messages",
        axum::routing::post(|| async { axum::Json(Message::default()) }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let http = Arc::new(
        HttpBuilder::new("test-token")
            .proxy(format!("http://{address}"))
            .ratelimiter_disabled(true)
            .build(),
    );
    let cache = Cache::default();
    let guild_id = GuildId::new(42);
    let bot_id = cache.current_user().id;
    let mut guild = Guild::default();
    guild.id = guild_id;
    let mut person: VoiceState = serde_json::from_value(serde_json::json!({
        "guild_id": "42", "channel_id": "10", "user_id": "2",
        "session_id": "test", "deaf": false, "mute": false,
        "self_deaf": false, "self_mute": false, "self_video": false, "suppress": false
    }))?;
    guild.voice_states.insert(person.user_id, person.clone());
    let mut event: GuildCreateEvent = serde_json::from_value(serde_json::to_value(guild)?)?;
    cache.update(&mut event);
    assert_eq!(count_people(&cache, guild_id, 10), Some(1));

    let manager = Songbird::serenity();
    manager.initialise_client_data(1, bot_id);
    let (sender, receiver) = mpsc::unbounded();
    manager.register_shard(0, sender).await;
    manager.get_or_insert(guild_id);
    drop(receiver);
    let recorder = Arc::new(Recorder::new(
        Arc::new(LocalWavStorage::new(root.clone())),
        Arc::new(NoopProcessor),
    ));
    let recording = recorder.start(RecordingOrigin::new("discord", 42)?)?;
    recording.sink().capture(vec![123; 960], Vec::new());
    let app = App {
        state: Mutex::new(State {
            configs: HashMap::from([(
                42,
                GuildConfig {
                    voice_channel_id: 10,
                    notice_channel_id: 11,
                },
            )]),
            sessions: HashMap::from([(
                42,
                RecordingSession {
                    channel_id: 10,
                    notice_channel_id: 11,
                    recording,
                },
            )]),
        }),
        manager: manager.clone(),
        recorder: recorder.clone(),
        transcription_enabled: false,
        shutting_down: AtomicBool::new(false),
        data_dir: root.clone(),
    };
    person.channel_id = None;
    let mut departure: VoiceStateUpdateEvent =
        serde_json::from_value(serde_json::to_value(person)?)?;
    cache.update(&mut departure);
    app.reconcile(&cache, &http, guild_id).await?;
    assert!(app.state.lock().await.sessions.is_empty());
    assert!(
        manager.get(guild_id).is_some(),
        "the failed leave must remain eligible for retry"
    );

    let (sender, mut receiver) = mpsc::unbounded();
    manager.register_shard(0, sender).await;
    app.reconcile(&cache, &http, guild_id).await?;
    assert!(manager.get(guild_id).is_none());
    let message = tokio::time::timeout(Duration::from_secs(1), receiver.next())
        .await?
        .context("missing leave command")?;
    let ShardRunnerMessage::Message(message) = message else {
        anyhow::bail!("expected a voice state update");
    };
    let payload: serde_json::Value = serde_json::from_str(&message.to_string())?;
    assert_eq!(payload["op"], 4);
    assert_eq!(payload["d"]["channel_id"], serde_json::Value::Null);
    recorder.wait_for_processing().await;
    server.abort();
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}
