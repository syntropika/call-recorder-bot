mod openai;

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

pub use openai::OpenAiTranscriber;

#[derive(Debug, Deserialize)]
pub struct TranscriptionResult {
    pub text: String,
    #[serde(default)]
    pub segments: Vec<TranscriptSegment>,
}

#[derive(Debug, Deserialize)]
pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A transcription provider receives one complete WAV chunk at a time.
/// This keeps HTTP details outside recording and processing logic.
#[async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe_wav(&self, wav: Vec<u8>) -> Result<TranscriptionResult>;
}
