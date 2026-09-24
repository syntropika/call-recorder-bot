pub mod discord;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::recording::Recorder;

/// Owns platform events, room membership, notices, and shutdown.
/// Implementations start and finish sessions through Recorder and send 48 kHz mono PCM frames
/// through AudioSink. They must preserve silence between speech so track timing stays aligned,
/// and assign each audio source a stable u32 ID for the duration of a session.
#[async_trait]
pub trait CaptureEngine: Send + Sync {
    async fn run(&self, recorder: Arc<Recorder>) -> Result<()>;
}
