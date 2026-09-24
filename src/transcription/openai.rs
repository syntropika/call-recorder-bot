use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use reqwest::{Client, multipart};
use std::time::Duration;

use super::{Transcriber, TranscriptionResult};

/// Adapter for OpenAI-compatible multipart transcription endpoints.
pub struct OpenAiTranscriber {
    client: Client,
    endpoint: String,
    api_key: String,
    model: String,
    language: String,
}

impl OpenAiTranscriber {
    pub fn new(endpoint: String, api_key: String, model: String, language: String) -> Result<Self> {
        if api_key.trim().is_empty() || model.trim().is_empty() || language.trim().is_empty() {
            bail!("transcription API key, model, and language must not be empty");
        }
        let url = reqwest::Url::parse(&endpoint).context("invalid transcription URL")?;
        if url.scheme() != "https" {
            bail!("transcription URL must use HTTPS");
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("could not create transcription HTTP client")?;
        Ok(Self {
            client,
            endpoint,
            api_key,
            model,
            language,
        })
    }
}

#[async_trait]
impl Transcriber for OpenAiTranscriber {
    async fn transcribe_wav(&self, wav: Vec<u8>) -> Result<TranscriptionResult> {
        let file = multipart::Part::bytes(wav)
            .file_name("recording.wav")
            .mime_str("audio/wav")?;
        let form = multipart::Form::new()
            .text("model", self.model.clone())
            .text("language", self.language.clone())
            .part("file", file);
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .context("transcription HTTP request failed")?;
        if !response.status().is_success() {
            bail!("transcription API returned HTTP {}", response.status());
        }
        let result: TranscriptionResult = response
            .json()
            .await
            .context("transcription API returned an invalid response")?;
        Ok(result)
    }
}
