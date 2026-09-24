mod cli_progress;
mod engines;
mod mixer;
mod processing;
mod recording;
mod recording_name;
mod settings;
mod storage;
mod transcription;

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, anyhow};
use engines::{CaptureEngine, discord::DiscordEngine};
use processing::{NoopProcessor, RecordingProcessor, TranscribingProcessor};
use recording::Recorder;
use settings::{AppSettings, EngineProvider, SecretEnvSettings, StorageProvider};
use storage::{
    LocalWavStorage, RecordingArtifact, RecordingOrigin, RecordingStorage, S3ArchiveProcessor,
};
use tokio::sync::mpsc;
use transcription::OpenAiTranscriber;

fn recording_origin(path: &Path, room_id: &str) -> Result<RecordingOrigin> {
    let engine = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !matches!(*name, "recordings" | "staging" | "diarized"))
        .unwrap_or("discord");
    RecordingOrigin::new(engine, room_id)
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let settings = AppSettings::load(Path::new("config.toml"))?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&settings.logging.filter)
                .context("invalid logging.filter in config.toml")?,
        )
        .init();
    let args: Vec<String> = env::args().skip(1).collect();
    let transcription_key =
        SecretEnvSettings::optional(&settings.secrets.transcription_api_key_env);
    let transcription_enabled = transcription_key.is_some();
    let transcriber = if let Some(key) = transcription_key {
        Some(Arc::new(OpenAiTranscriber::new(
            settings.transcription.url.clone(),
            key,
            settings.transcription.model.clone(),
            settings.transcription.language.clone(),
        )?) as Arc<dyn transcription::Transcriber>)
    } else {
        None
    };
    match args.as_slice() {
        [] => {}
        [flag, location] if flag == "--transcribe" => {
            let transcriber = transcriber.with_context(|| {
                format!(
                    "{} is required for --transcribe",
                    settings.secrets.transcription_api_key_env
                )
            })?;
            let path = PathBuf::from(location);
            let recording_id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(|name| recording_name::parse(name).ok())
                .unwrap_or(0);
            let room_id = path
                .parent()
                .and_then(|folder| folder.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("unknown");
            let (sender, receiver) = mpsc::unbounded_channel();
            let display = tokio::spawn(cli_progress::show(receiver));
            let processor = TranscribingProcessor::new(transcriber).with_progress(sender);
            let result = processor
                .on_completed(RecordingArtifact {
                    origin: recording_origin(&path, room_id)?,
                    recording_id,
                    location: location.clone(),
                    speaker_tracks: LocalWavStorage::load_speaker_tracks(&path)?,
                })
                .await;
            drop(processor);
            display.await?;
            result?;
            return Ok(());
        }
        [flag, location] if flag == "--archive" => {
            let path = PathBuf::from(location);
            let recording_id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(|name| recording_name::parse(name).ok())
                .context("archive WAV name must be a UTC timestamp or legacy recording ID")?;
            let room_id = path
                .parent()
                .and_then(|folder| folder.file_name())
                .and_then(|name| name.to_str())
                .context("archive WAV must be in a room ID directory")?;
            S3ArchiveProcessor::new(
                settings.s3()?,
                &settings.secrets,
                Arc::new(NoopProcessor),
                false,
            )?
            .on_completed(RecordingArtifact {
                origin: recording_origin(&path, room_id)?,
                recording_id,
                location: location.clone(),
                speaker_tracks: LocalWavStorage::load_speaker_tracks(&path)?,
            })
            .await?;
            return Ok(());
        }
        _ => {
            return Err(anyhow!(
                "usage: call-recorder-bot [--transcribe <wav-path> | --archive <wav-path>]"
            ));
        }
    }
    let transcription_processor: Arc<dyn RecordingProcessor> = match transcriber {
        Some(transcriber) => Arc::new(TranscribingProcessor::new(transcriber)),
        None => Arc::new(NoopProcessor),
    };
    let data_dir = settings.app.data_dir.clone();
    let (storage_dir, processor): (PathBuf, Arc<dyn RecordingProcessor>) =
        match settings.storage.provider {
            StorageProvider::Local => (data_dir.join("recordings"), transcription_processor),
            StorageProvider::S3 => (
                data_dir.join("staging"),
                Arc::new(S3ArchiveProcessor::new(
                    settings.s3()?,
                    &settings.secrets,
                    transcription_processor,
                    true,
                )?),
            ),
        };
    let recorder = Arc::new(Recorder::new(
        Arc::new(LocalWavStorage::new(storage_dir)) as Arc<dyn RecordingStorage>,
        processor,
    ));
    let engine: Box<dyn CaptureEngine> = match settings.engine.provider {
        EngineProvider::Discord => Box::new(DiscordEngine::new(
            SecretEnvSettings::required(&settings.secrets.discord_token_env)?,
            data_dir,
            transcription_enabled,
        )),
    };
    engine.run(recorder).await
}
