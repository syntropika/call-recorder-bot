use std::{path::Path, sync::Arc};

use anyhow::{Context, Result};
use serenity::{
    all::{ChannelId, CreateAllowedMentions, CreateAttachment, CreateMessage},
    async_trait,
    http::Http,
};

use crate::{recording::TranscriptDelivery, storage::RecordingArtifact};

// Stay below Discord's base upload limit without relying on server boosts.
const ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct DiscordTranscriptDelivery {
    http: Arc<Http>,
    notice_channel: ChannelId,
    voice_channel: ChannelId,
}

impl DiscordTranscriptDelivery {
    pub(super) fn new(
        http: Arc<Http>,
        notice_channel: ChannelId,
        voice_channel: ChannelId,
    ) -> Self {
        Self {
            http,
            notice_channel,
            voice_channel,
        }
    }
}

#[async_trait]
impl TranscriptDelivery for DiscordTranscriptDelivery {
    async fn deliver(&self, artifact: &RecordingArtifact, transcript: String) -> Result<()> {
        let name = Path::new(&artifact.location)
            .file_stem()
            .and_then(|name| name.to_str())
            .context("recording has no transcript filename")?;
        let transcript = if transcript.trim().is_empty() {
            "No speech was detected in this recording.".to_owned()
        } else {
            transcript
        };
        let parts = transcript_parts(&transcript, ATTACHMENT_BYTES);
        for (index, part) in parts.iter().enumerate() {
            let (filename, suffix) = if parts.len() == 1 {
                (format!("{name}.txt"), String::new())
            } else {
                (
                    format!("{name}.part-{}.txt", index + 1),
                    format!(" Part {}/{}.", index + 1, parts.len()),
                )
            };
            self.notice_channel
                .send_message(
                    &self.http,
                    CreateMessage::new()
                        .content(format!(
                            "📝 Transcript for <#{}> ({name}).{suffix}",
                            self.voice_channel
                        ))
                        .allowed_mentions(
                            CreateAllowedMentions::new()
                                .all_users(false)
                                .all_roles(false)
                                .everyone(false),
                        )
                        .add_file(CreateAttachment::bytes(part.as_bytes(), filename)),
                )
                .await
                .context("could not send the transcript attachment to Discord")?;
        }
        tracing::info!(
            recording = artifact.recording_id,
            channel = self.notice_channel.get(),
            "transcript delivered to Discord"
        );
        Ok(())
    }
}

fn transcript_parts(mut text: &str, limit: usize) -> Vec<&str> {
    let mut parts = Vec::new();
    while text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let (part, remaining) = text.split_at(end);
        parts.push(part);
        text = remaining;
    }
    parts.push(text);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, body::Bytes, extract::State, routing::post};
    use serenity::{all::Message, http::HttpBuilder};
    use tokio::sync::mpsc;

    #[test]
    fn long_transcripts_preserve_unicode_and_every_byte() {
        let text = "Hello 🌍!".repeat(10);
        let parts = transcript_parts(&text, 13);
        assert!(parts.iter().all(|part| part.len() <= 13));
        assert_eq!(parts.concat(), text);
        assert_eq!(transcript_parts("", 13), vec![""]);
    }

    #[tokio::test]
    async fn sends_the_transcript_to_the_captured_notice_channel() -> Result<()> {
        async fn capture(
            State(sender): State<mpsc::UnboundedSender<Vec<u8>>>,
            body: Bytes,
        ) -> Json<Message> {
            sender.send(body.to_vec()).unwrap();
            Json(Message::default())
        }
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let router = Router::new()
            .route("/api/v10/channels/11/messages", post(capture))
            .with_state(sender);
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let http = HttpBuilder::new("test-token")
            .proxy(format!("http://{address}"))
            .ratelimiter_disabled(true)
            .build();
        let delivery =
            DiscordTranscriptDelivery::new(Arc::new(http), ChannelId::new(11), ChannelId::new(10));
        let artifact = RecordingArtifact {
            origin: crate::storage::RecordingOrigin::new("discord", "42")?,
            recording_id: 1,
            location: "/removed/2026-09-24T21-35-08Z.wav".into(),
            speaker_tracks: Vec::new(),
        };
        delivery
            .deliver(&artifact, "[00:01] Speaker 1: Hello 🌍".into())
            .await?;
        let body = String::from_utf8(receiver.try_recv()?)?;
        assert!(body.contains("2026-09-24T21-35-08Z.txt"));
        assert!(body.contains("[00:01] Speaker 1: Hello 🌍"));
        assert!(body.contains("Transcript for <#10>"));
        assert!(body.contains("\"parse\":[]"));
        server.abort();
        Ok(())
    }
}
