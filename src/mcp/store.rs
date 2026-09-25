use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use aws_sdk_s3::{
    Client,
    config::{BehaviorVersion, Credentials, Region},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{recording_name, settings::S3Settings, storage::RecordingOrigin};

const MAX_TRANSCRIPT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct TranscriptRef {
    pub engine: String,
    pub room_id: String,
    pub recording: String,
}

#[derive(Debug, Serialize)]
pub struct TranscriptPage {
    pub items: Vec<TranscriptRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[async_trait]
pub trait TranscriptStore: Send + Sync {
    async fn list(&self, cursor: Option<String>, limit: i32) -> Result<TranscriptPage>;
    async fn get(&self, reference: &TranscriptRef) -> Result<Option<String>>;
}

pub fn transcript_key(prefix: &str, reference: &TranscriptRef) -> Result<String> {
    RecordingOrigin::new(&reference.engine, &reference.room_id)?;
    recording_name::parse(&reference.recording)?;
    let name = &reference.recording;
    let path = format!(
        "{name}/{}/{}/{name}.txt",
        reference.engine, reference.room_id
    );
    Ok(if prefix.is_empty() {
        path
    } else {
        format!("{prefix}/{path}")
    })
}

pub fn parse_transcript_key(prefix: &str, key: &str) -> Option<TranscriptRef> {
    let relative = if prefix.is_empty() {
        key
    } else {
        key.strip_prefix(&format!("{prefix}/"))?
    };
    let mut parts = relative.split('/');
    let reference = TranscriptRef {
        recording: parts.next()?.to_owned(),
        engine: parts.next()?.to_owned(),
        room_id: parts.next()?.to_owned(),
    };
    if parts.next()? != format!("{}.txt", reference.recording) || parts.next().is_some() {
        return None;
    }
    transcript_key(prefix, &reference)
        .ok()
        .filter(|expected| expected == key)?;
    Some(reference)
}

pub struct S3TranscriptStore {
    client: Client,
    bucket: String,
    prefix: String,
}

impl S3TranscriptStore {
    pub fn new(settings: &S3Settings) -> Result<Arc<Self>> {
        let mut config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(settings.region.clone()))
            .credentials_provider(Credentials::new(
                settings.access_key_id.required()?,
                settings.secret_access_key.required()?,
                settings
                    .session_token
                    .as_ref()
                    .and_then(|value| value.optional()),
                None,
                "configured S3 credentials",
            ))
            .force_path_style(settings.force_path_style);
        if let Some(endpoint) = settings.resolved_endpoint()? {
            config = config.endpoint_url(endpoint);
        }
        Ok(Arc::new(Self {
            client: Client::from_conf(config.build()),
            bucket: settings.resolved_bucket()?,
            prefix: settings.prefix.trim_matches('/').to_owned(),
        }))
    }
}

#[async_trait]
impl TranscriptStore for S3TranscriptStore {
    async fn list(&self, cursor: Option<String>, limit: i32) -> Result<TranscriptPage> {
        let response = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .set_prefix((!self.prefix.is_empty()).then(|| format!("{}/", self.prefix)))
            .set_continuation_token(cursor)
            .max_keys(limit)
            .send()
            .await
            .context("could not list transcript objects")?;
        let items = response
            .contents()
            .iter()
            .filter_map(|object| {
                object
                    .key()
                    .and_then(|key| parse_transcript_key(&self.prefix, key))
            })
            .collect();
        let next_cursor = if response.is_truncated().unwrap_or(false) {
            Some(
                response
                    .next_continuation_token()
                    .context("S3 listing omitted continuation token")?
                    .to_owned(),
            )
        } else {
            None
        };
        Ok(TranscriptPage { items, next_cursor })
    }

    async fn get(&self, reference: &TranscriptRef) -> Result<Option<String>> {
        let key = transcript_key(&self.prefix, reference)?;
        let response = match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error)
                if error
                    .raw_response()
                    .is_some_and(|response| response.status().as_u16() == 404) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error).context("could not read transcript object"),
        };
        if response
            .content_length()
            .is_some_and(|size| size > MAX_TRANSCRIPT_BYTES as i64)
        {
            bail!("transcript exceeds the maximum supported size");
        }
        let mut body = response.body;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.context("could not read transcript body")?;
            if bytes.len() + chunk.len() > MAX_TRANSCRIPT_BYTES {
                bail!("transcript exceeds the maximum supported size");
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(
            String::from_utf8(bytes).context("transcript is not UTF-8")?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_archive_layout_and_reject_other_objects() -> Result<()> {
        let reference = TranscriptRef {
            engine: "discord".into(),
            room_id: "42".into(),
            recording: "2026-09-24T21-35-08Z".into(),
        };
        let key = transcript_key("recording", &reference)?;
        assert_eq!(
            key,
            "recording/2026-09-24T21-35-08Z/discord/42/2026-09-24T21-35-08Z.txt"
        );
        assert_eq!(parse_transcript_key("recording", &key), Some(reference));
        assert!(parse_transcript_key("recording", &key.replace(".txt", ".wav")).is_none());
        assert!(
            transcript_key(
                "recording",
                &TranscriptRef {
                    engine: "discord".into(),
                    room_id: "../secret".into(),
                    recording: "123".into()
                }
            )
            .is_err()
        );
        Ok(())
    }
}
