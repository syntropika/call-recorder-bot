use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use aws_sdk_s3::{
    Client,
    config::{BehaviorVersion, Credentials, Region, RequestChecksumCalculation},
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart},
};
use tokio::io::AsyncReadExt;

use crate::{
    processing::RecordingProcessor,
    recording_name,
    settings::{S3Settings, SecretEnvSettings},
};

use super::{RecordingArtifact, SpeakerTrack};

const SINGLE_PUT_LIMIT: u64 = 64 * 1024 * 1024;
const MIN_PART_SIZE: u64 = 16 * 1024 * 1024;

fn object_key(prefix: &str, artifact: &RecordingArtifact, extension: &str) -> Result<String> {
    let name = Path::new(&artifact.location)
        .file_stem()
        .and_then(|name| name.to_str())
        .context("recording path must have a UTF-8 file stem")?;
    recording_name::parse(name)?;
    let name = format!(
        "{name}/{}/{}/{name}.{extension}",
        artifact.origin.engine, artifact.origin.room_id
    );
    if prefix.is_empty() {
        Ok(name)
    } else {
        Ok(format!("{prefix}/{name}"))
    }
}

/// Archives finalized local recordings through the standard S3 API.
/// The local spool is retained if any upload or processing step fails.
pub struct S3ArchiveProcessor {
    client: Client,
    bucket: String,
    prefix: String,
    next: Arc<dyn RecordingProcessor>,
    remove_spool_on_success: bool,
}

impl S3ArchiveProcessor {
    pub fn new(
        settings: &S3Settings,
        secrets: &SecretEnvSettings,
        next: Arc<dyn RecordingProcessor>,
        remove_spool_on_success: bool,
    ) -> Result<Self> {
        let access_key = SecretEnvSettings::required(&secrets.s3_access_key_id_env)?;
        let secret_key = SecretEnvSettings::required(&secrets.s3_secret_access_key_env)?;
        let mut config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(settings.region.clone()))
            .credentials_provider(Credentials::new(
                access_key,
                secret_key,
                SecretEnvSettings::optional(&secrets.s3_session_token_env),
                None,
                "configured S3 credentials",
            ))
            .force_path_style(settings.force_path_style)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired);
        if let Some(endpoint) = &settings.endpoint {
            config = config.endpoint_url(endpoint.clone());
        }
        Ok(Self {
            client: Client::from_conf(config.build()),
            bucket: settings.bucket.clone(),
            prefix: settings.prefix.trim_matches('/').to_owned(),
            next,
            remove_spool_on_success,
        })
    }

    fn key(&self, artifact: &RecordingArtifact, extension: &str) -> Result<String> {
        object_key(&self.prefix, artifact, extension)
    }

    fn object_uri(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket)
    }

    async fn upload_file(&self, key: &str, path: &Path, content_type: &str) -> Result<()> {
        let size = tokio::fs::metadata(path).await?.len();
        if size <= SINGLE_PUT_LIMIT {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(key)
                .content_type(content_type)
                .body(ByteStream::from_path(path).await?)
                .send()
                .await
                .with_context(|| format!("could not upload {key}"))?;
            return Ok(());
        }

        // Keep every non-final part the same size for R2 and within S3's 10,000 part limit.
        let part_size = MIN_PART_SIZE.max(size.div_ceil(10_000));
        let created = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .send()
            .await
            .with_context(|| format!("could not start multipart upload for {key}"))?;
        let upload_id = created
            .upload_id()
            .context("multipart upload has no upload ID")?;
        let upload = async {
            let mut file = tokio::fs::File::open(path).await?;
            let mut remaining = size;
            let mut parts = Vec::new();
            while remaining > 0 {
                let length = remaining.min(part_size) as usize;
                let mut bytes = vec![0; length];
                file.read_exact(&mut bytes).await?;
                let number = i32::try_from(parts.len() + 1)?;
                let result = self
                    .client
                    .upload_part()
                    .bucket(&self.bucket)
                    .key(key)
                    .upload_id(upload_id)
                    .part_number(number)
                    .body(ByteStream::from(bytes))
                    .send()
                    .await
                    .with_context(|| format!("could not upload part {number} of {key}"))?;
                parts.push(
                    CompletedPart::builder()
                        .part_number(number)
                        .e_tag(result.e_tag().context("uploaded part has no ETag")?)
                        .build(),
                );
                remaining -= length as u64;
            }
            self.client
                .complete_multipart_upload()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(upload_id)
                .multipart_upload(
                    CompletedMultipartUpload::builder()
                        .set_parts(Some(parts))
                        .build(),
                )
                .send()
                .await
                .with_context(|| format!("could not complete multipart upload for {key}"))?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if upload.is_err()
            && let Err(err) = self
                .client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(upload_id)
                .send()
                .await
        {
            tracing::warn!(%err, key, "could not abort failed multipart upload");
        }
        upload
    }

    async fn archive_audio(&self, artifact: &RecordingArtifact) -> Result<()> {
        let mut remote_tracks: Vec<SpeakerTrack> = artifact.speaker_tracks.clone();
        for track in &mut remote_tracks {
            let key = self.key(artifact, &format!("source-{}.wav", track.source_id))?;
            self.upload_file(&key, Path::new(&track.location), "audio/wav")
                .await?;
            track.location = self.object_uri(&key);
        }
        let manifest_key = self.key(artifact, "speakers.json")?;
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&manifest_key)
            .content_type("application/json")
            .body(ByteStream::from(serde_json::to_vec_pretty(&remote_tracks)?))
            .send()
            .await
            .with_context(|| format!("could not upload {manifest_key}"))?;
        let wav_key = self.key(artifact, "wav")?;
        self.upload_file(&wav_key, Path::new(&artifact.location), "audio/wav")
            .await?;
        tracing::info!(location = %self.object_uri(&wav_key), "recording archived");
        Ok(())
    }

    async fn remove_spool(&self, artifact: &RecordingArtifact) -> Result<()> {
        let wav = PathBuf::from(&artifact.location);
        for track in &artifact.speaker_tracks {
            tokio::fs::remove_file(&track.location).await?;
        }
        tokio::fs::remove_file(wav.with_extension("speakers.json")).await?;
        if tokio::fs::try_exists(wav.with_extension("txt")).await? {
            tokio::fs::remove_file(wav.with_extension("txt")).await?;
        }
        tokio::fs::remove_file(wav).await?;
        Ok(())
    }
}

#[async_trait]
impl RecordingProcessor for S3ArchiveProcessor {
    async fn on_completed(&self, artifact: RecordingArtifact) -> Result<()> {
        if artifact.recording_id == 0 || artifact.origin.room_id.is_empty() {
            bail!("S3 archival requires a recording ID and room ID");
        }
        self.archive_audio(&artifact).await?;
        self.next.on_completed(artifact.clone()).await?;
        let transcript = Path::new(&artifact.location).with_extension("txt");
        if tokio::fs::try_exists(&transcript).await? {
            let transcript_key = self.key(&artifact, "txt")?;
            self.upload_file(&transcript_key, &transcript, "text/plain; charset=utf-8")
                .await?;
        }
        if self.remove_spool_on_success
            && let Err(err) = self.remove_spool(&artifact).await
        {
            tracing::warn!(%err, location = %artifact.location, "could not remove archived local spool");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::RecordingOrigin;

    #[test]
    fn keys_sort_by_start_time_and_include_engine_and_room() -> Result<()> {
        let first = RecordingArtifact {
            origin: RecordingOrigin::new("discord", "42")?,
            recording_id: 1,
            location: "2026-09-24T21-35-08Z.wav".into(),
            speaker_tracks: Vec::new(),
        };
        let mut second = first.clone();
        second.location = "2026-09-24T21-35-09Z.wav".into();
        let first_key = object_key("recording", &first, "wav")?;
        let second_key = object_key("recording", &second, "wav")?;
        assert_eq!(
            first_key,
            "recording/2026-09-24T21-35-08Z/discord/42/2026-09-24T21-35-08Z.wav"
        );
        assert!(first_key < second_key);
        Ok(())
    }
}
