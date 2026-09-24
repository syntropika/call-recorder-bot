mod local;
mod s3;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub use local::LocalWavStorage;
pub use s3::S3ArchiveProcessor;

/// A completed recording can live on disk or at a remote provider.
#[derive(Clone, Debug)]
pub struct RecordingArtifact {
    pub origin: RecordingOrigin,
    pub recording_id: u128,
    pub location: String,
    pub speaker_tracks: Vec<SpeakerTrack>,
}

#[derive(Clone, Debug)]
pub struct RecordingOrigin {
    pub engine: String,
    pub room_id: String,
}

impl RecordingOrigin {
    pub fn new(engine: &str, room_id: impl ToString) -> Result<Self> {
        if engine.is_empty()
            || !engine
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            bail!("recording engine name must contain only lowercase letters, digits, or hyphens");
        }
        let room_id = room_id.to_string();
        if room_id.is_empty()
            || !room_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            bail!("recording room ID must contain only letters, digits, hyphens, or underscores");
        }
        Ok(Self {
            engine: engine.to_owned(),
            room_id,
        })
    }
}

/// A source track stays aligned with the mixed recording's timeline.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SpeakerTrack {
    #[serde(alias = "ssrc")]
    pub source_id: u32,
    /// Kept to read manifests written before participant identities were generalized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant_label: Option<String>,
    pub location: String,
    /// Position of this track's first sample on the mixed recording timeline.
    #[serde(default)]
    pub start_sample: u64,
}

/// Port implemented by each recording provider's active writer.
pub trait RecordingWriter: Send {
    fn write_tick(&mut self, mixed: &[i16], speakers: &[(u32, Vec<i16>)]) -> Result<()>;
    fn identify_speaker(&mut self, source_id: u32, participant_id: String, label: String);
    fn finish(self: Box<Self>) -> Result<RecordingArtifact>;
    fn abort(self: Box<Self>) -> Result<()>;
}

/// Port used by the bot to open a recording, independent of its destination.
pub trait RecordingStorage: Send + Sync {
    fn start(
        &self,
        origin: RecordingOrigin,
        recording_id: u128,
    ) -> Result<Box<dyn RecordingWriter>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_accepts_opaque_room_ids_without_path_traversal() -> Result<()> {
        assert_eq!(RecordingOrigin::new("meet", "abc-def")?.room_id, "abc-def");
        assert!(RecordingOrigin::new("meet", "../another-room").is_err());
        Ok(())
    }
}
