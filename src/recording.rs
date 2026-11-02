use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use async_trait::async_trait;
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tracing::{error, info, warn};

use crate::{
    processing::RecordingProcessor,
    storage::{RecordingArtifact, RecordingOrigin, RecordingStorage},
};

enum AudioMessage {
    Samples {
        mixed: Vec<i16>,
        sources: Vec<(u32, Vec<i16>)>,
    },
    SpeakerIdentity {
        source_id: u32,
        participant_id: String,
        label: String,
    },
    Finish,
    Abort,
}

#[derive(Clone)]
pub struct AudioSink {
    sender: mpsc::Sender<AudioMessage>,
    active: Arc<AtomicBool>,
    dropped_ticks: Arc<AtomicU64>,
}

impl AudioSink {
    /// Accepts one 48 kHz mono PCM frame and its aligned per-source samples.
    /// Capture engines should include silent frames to keep recording time continuous.
    pub fn capture(&self, mixed: Vec<i16>, sources: Vec<(u32, Vec<i16>)>) {
        if self.active.load(Ordering::Acquire)
            && self
                .sender
                .try_send(AudioMessage::Samples { mixed, sources })
                .is_err()
        {
            self.dropped_ticks.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Associates a stable source ID with an opaque participant ID and a readable label.
    pub async fn identify(&self, source_id: u32, participant_id: String, label: String) {
        if self.active.load(Ordering::Acquire) {
            let _ = self
                .sender
                .send(AudioMessage::SpeakerIdentity {
                    source_id,
                    participant_id,
                    label,
                })
                .await;
        }
    }

    pub fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
    }

    fn dropped_ticks(&self) -> u64 {
        self.dropped_ticks.load(Ordering::Relaxed)
    }
}

pub struct ActiveRecording {
    sink: AudioSink,
    writer: JoinHandle<Result<Option<RecordingArtifact>>>,
}

impl ActiveRecording {
    pub fn sink(&self) -> AudioSink {
        self.sink.clone()
    }
}

/// Delivers a completed transcript after processing and archival have succeeded.
#[async_trait]
pub trait TranscriptDelivery: Send + Sync {
    async fn deliver(&self, artifact: &RecordingArtifact, transcript: String) -> Result<()>;
}

/// Owns recording files and post-recording work independently of a capture engine.
pub struct Recorder {
    storage: Arc<dyn RecordingStorage>,
    processor: Arc<dyn RecordingProcessor>,
    processing_tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Recorder {
    pub fn new(storage: Arc<dyn RecordingStorage>, processor: Arc<dyn RecordingProcessor>) -> Self {
        Self {
            storage,
            processor,
            processing_tasks: Mutex::new(Vec::new()),
        }
    }

    pub fn start(&self, origin: RecordingOrigin) -> Result<ActiveRecording> {
        let recording_id = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let writer = self.storage.start(origin, recording_id)?;
        let (sender, mut receiver) = mpsc::channel(500);
        let writer = tokio::task::spawn_blocking(move || -> Result<Option<RecordingArtifact>> {
            let mut writer = writer;
            while let Some(message) = receiver.blocking_recv() {
                match message {
                    AudioMessage::Samples { mixed, sources } => {
                        writer.write_tick(&mixed, &sources)?
                    }
                    AudioMessage::SpeakerIdentity {
                        source_id,
                        participant_id,
                        label,
                    } => writer.identify_speaker(source_id, participant_id, label),
                    AudioMessage::Finish => return writer.finish().map(Some),
                    AudioMessage::Abort => return writer.abort().map(|()| None),
                }
            }
            writer.finish().map(Some)
        });
        Ok(ActiveRecording {
            sink: AudioSink {
                sender,
                active: Arc::new(AtomicBool::new(true)),
                dropped_ticks: Arc::new(AtomicU64::new(0)),
            },
            writer,
        })
    }

    pub async fn abort(&self, recording: ActiveRecording) {
        recording.sink.deactivate();
        let _ = recording.sink.sender.send(AudioMessage::Abort).await;
        if let Err(err) = recording.writer.await {
            error!(%err, "recording writer task failed while aborting");
        }
    }

    pub async fn finish(
        &self,
        recording: ActiveRecording,
        delivery: Option<Arc<dyn TranscriptDelivery>>,
    ) {
        recording.sink.deactivate();
        let _ = recording.sink.sender.send(AudioMessage::Finish).await;
        match recording.writer.await {
            Ok(Ok(Some(artifact))) => {
                info!(engine = %artifact.origin.engine, room = %artifact.origin.room_id, recording = artifact.recording_id, location = %artifact.location, "recording finalized");
                let processor = self.processor.clone();
                let task = tokio::spawn(async move {
                    match processor.on_completed(artifact.clone()).await {
                        Ok(Some(transcript)) => {
                            if let Some(delivery) = delivery
                                && let Err(err) = delivery.deliver(&artifact, transcript).await
                            {
                                error!(%err, engine = %artifact.origin.engine, room = %artifact.origin.room_id, recording = artifact.recording_id, "transcript delivery failed; completed files remain in storage");
                            }
                        }
                        Ok(None) => {}
                        Err(err) => error!(%err, "post-recording processing failed"),
                    }
                });
                let mut tasks = self.processing_tasks.lock().await;
                tasks.retain(|task| !task.is_finished());
                tasks.push(task);
            }
            Ok(Ok(None)) => warn!("recording was aborted"),
            Ok(Err(err)) => error!(%err, "recording writer failed"),
            Err(err) => error!(%err, "recording writer task failed"),
        }
        let dropped = recording.sink.dropped_ticks();
        if dropped > 0 {
            warn!(
                dropped,
                "audio ticks were lost because the writer could not keep up"
            );
        }
    }

    pub async fn wait_for_processing(&self) {
        let tasks = std::mem::take(&mut *self.processing_tasks.lock().await);
        for task in tasks {
            if let Err(err) = task.await {
                error!(%err, "post-recording task failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{LocalWavStorage, RecordingStorage};
    use anyhow::Context as _;
    use async_trait::async_trait;

    struct CaptureProcessor(mpsc::UnboundedSender<RecordingArtifact>);

    #[async_trait]
    impl RecordingProcessor for CaptureProcessor {
        async fn on_completed(&self, artifact: RecordingArtifact) -> Result<Option<String>> {
            self.0.send(artifact)?;
            Ok(None)
        }
    }

    #[tokio::test]
    async fn records_and_processes_audio_without_a_platform_adapter() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "call-recorder-bot-recorder-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let recorder = Recorder::new(
            Arc::new(LocalWavStorage::new(root.clone())) as Arc<dyn RecordingStorage>,
            Arc::new(CaptureProcessor(sender)),
        );
        let session = recorder.start(RecordingOrigin::new("meet", "room-a")?)?;
        let sink = session.sink();
        sink.capture(vec![123; 960], vec![(7, vec![123; 960])]);
        sink.identify(7, "person-99".into(), "Alex".into()).await;
        recorder.finish(session, None).await;
        recorder.wait_for_processing().await;
        let artifact = receiver
            .recv()
            .await
            .context("processor did not receive recording")?;
        assert_eq!(artifact.origin.engine, "meet");
        assert_eq!(artifact.origin.room_id, "room-a");
        assert_eq!(
            artifact.speaker_tracks[0].participant_id.as_deref(),
            Some("person-99")
        );
        assert!(artifact.location.contains("/meet/room-a/"));
        assert_eq!(hound::WavReader::open(&artifact.location)?.duration(), 960);
        tokio::fs::remove_dir_all(root).await?;
        Ok(())
    }

    struct CompletedProcessor {
        fail: bool,
    }

    #[async_trait]
    impl RecordingProcessor for CompletedProcessor {
        async fn on_completed(&self, artifact: RecordingArtifact) -> Result<Option<String>> {
            if self.fail {
                anyhow::bail!("transcription failed");
            }
            tokio::fs::remove_file(&artifact.location).await?;
            Ok(Some("[00:01] Speaker 1: Hello".into()))
        }
    }

    struct CaptureDelivery(mpsc::UnboundedSender<String>);

    #[async_trait]
    impl TranscriptDelivery for CaptureDelivery {
        async fn deliver(&self, artifact: &RecordingArtifact, transcript: String) -> Result<()> {
            assert!(!tokio::fs::try_exists(&artifact.location).await?);
            self.0.send(transcript)?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn delivers_completed_text_after_cleanup_and_skips_failed_processing() -> Result<()> {
        for fail in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "call-recorder-delivery-{}-{}-{fail}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
            let (sender, mut receiver) = mpsc::unbounded_channel();
            let recorder = Recorder::new(
                Arc::new(LocalWavStorage::new(root.clone())),
                Arc::new(CompletedProcessor { fail }),
            );
            let session = recorder.start(RecordingOrigin::new("test", "room-a")?)?;
            session.sink().capture(vec![123; 960], Vec::new());
            recorder
                .finish(session, Some(Arc::new(CaptureDelivery(sender))))
                .await;
            recorder.wait_for_processing().await;
            if fail {
                assert!(receiver.try_recv().is_err());
                assert!(std::fs::read_dir(root.join("test/room-a"))?.any(|entry| {
                    entry
                        .unwrap()
                        .path()
                        .extension()
                        .is_some_and(|ext| ext == "wav")
                }));
            } else {
                assert_eq!(receiver.try_recv()?, "[00:01] Speaker 1: Hello");
            }
            tokio::fs::remove_dir_all(root).await?;
        }
        Ok(())
    }
}
