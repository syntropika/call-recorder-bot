use std::{collections::HashMap, io::Cursor, path::Path, sync::Arc};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{storage::RecordingArtifact, transcription::Transcriber};

/// Processes a finalized recording and returns its completed transcript, when enabled.
#[async_trait]
pub trait RecordingProcessor: Send + Sync {
    async fn on_completed(&self, artifact: RecordingArtifact) -> Result<Option<String>>;
}

pub struct NoopProcessor;

#[async_trait]
impl RecordingProcessor for NoopProcessor {
    async fn on_completed(&self, _artifact: RecordingArtifact) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Transcribes a local WAV in bounded chunks and writes a sibling text file.
pub struct TranscribingProcessor {
    transcriber: Arc<dyn Transcriber>,
    progress: Option<mpsc::UnboundedSender<TranscriptionProgress>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptionProgress {
    Started {
        total_chunks: usize,
    },
    ChunkCompleted {
        completed: usize,
        total_chunks: usize,
    },
}

impl TranscribingProcessor {
    pub fn new(transcriber: Arc<dyn Transcriber>) -> Self {
        Self {
            transcriber,
            progress: None,
        }
    }

    pub fn with_progress(mut self, sender: mpsc::UnboundedSender<TranscriptionProgress>) -> Self {
        self.progress = Some(sender);
        self
    }

    fn report(&self, progress: TranscriptionProgress) {
        if let Some(sender) = &self.progress {
            let _ = sender.send(progress);
        }
    }
}

const CHUNK_SECONDS: u32 = 180;

#[async_trait]
impl RecordingProcessor for TranscribingProcessor {
    async fn on_completed(&self, artifact: RecordingArtifact) -> Result<Option<String>> {
        let path = Path::new(&artifact.location);
        let labeled = !artifact.speaker_tracks.is_empty();
        let mut inputs = Vec::new();
        let mut speaker_headers = Vec::new();
        let mut participant_labels = HashMap::new();
        if labeled {
            for track in &artifact.speaker_tracks {
                let number = match &track.participant_id {
                    Some(participant_id) => {
                        if let Some(number) = participant_labels.get(participant_id) {
                            *number
                        } else {
                            let number = speaker_headers.len() + 1;
                            participant_labels.insert(participant_id.clone(), number);
                            let label = track
                                .participant_label
                                .as_deref()
                                .filter(|label| !label.trim().is_empty())
                                .unwrap_or(participant_id);
                            speaker_headers
                                .push(format!("Speaker {number}: {}", normalized_text(label)));
                            number
                        }
                    }
                    None => {
                        let number = speaker_headers.len() + 1;
                        speaker_headers.push(format!(
                            "Speaker {number}: Unknown source (ID {})",
                            track.source_id
                        ));
                        number
                    }
                };
                let label = format!("Speaker {number}");
                inputs.push(TrackInput::open(
                    Path::new(&track.location),
                    Some(label),
                    track.start_sample,
                )?);
            }
        } else {
            inputs.push(TrackInput::open(path, None, 0)?);
        }
        let total_chunks: usize = inputs.iter().map(|input| input.chunks).sum();
        self.report(TranscriptionProgress::Started { total_chunks });
        let mut parts = Vec::new();
        let mut utterances = Vec::new();
        let mut rejected_phrases = HashMap::new();
        let mut completed = 0;
        for input in &mut inputs {
            let mut chunk_index = 0;
            while let Some(mut chunk) = next_wav_chunk(&mut input.reader, input.max_samples)? {
                let current = completed + 1;
                if self.progress.is_none() {
                    tracing::info!(engine = %artifact.origin.engine, room = %artifact.origin.room_id, recording = artifact.recording_id, chunk = current, total_chunks, source = ?input.label, "transcription chunk started");
                }
                if chunk.contains_signal {
                    let result = self
                        .transcriber
                        .transcribe_wav(std::mem::take(&mut chunk.wav))
                        .await
                        .with_context(|| {
                            format!("could not transcribe chunk {current}/{total_chunks}")
                        })?;
                    if let Some(label) = &input.label {
                        let offset =
                            input.start_seconds + (chunk_index * CHUNK_SECONDS as usize) as f64;
                        if result.segments.is_empty() {
                            push_utterance(&mut utterances, offset, label, &result.text);
                        } else {
                            for segment in result.segments {
                                if segment.end.is_finite() && segment.end >= segment.start {
                                    if chunk.signal_near(segment.start, segment.end) {
                                        push_utterance(
                                            &mut utterances,
                                            offset + segment.start,
                                            label,
                                            &segment.text,
                                        );
                                    } else {
                                        let text = normalized_text(&segment.text);
                                        if !text.is_empty() && text.len() <= 30 {
                                            *rejected_phrases.entry(text).or_insert(0) += 1;
                                        }
                                    }
                                }
                            }
                        }
                    } else if !result.text.trim().is_empty() {
                        parts.push(result.text.trim().to_owned());
                    }
                }
                chunk_index += 1;
                completed += 1;
                if self.progress.is_none() {
                    tracing::info!(
                        engine = %artifact.origin.engine,
                        room = %artifact.origin.room_id,
                        recording = artifact.recording_id,
                        chunk = completed,
                        total_chunks,
                        "transcription chunk completed"
                    );
                }
                self.report(TranscriptionProgress::ChunkCompleted {
                    completed,
                    total_chunks,
                });
            }
            if chunk_index != input.chunks {
                bail!("speaker track ended before all audio chunks could be read");
            }
        }
        discard_phrases_repeated_in_silence(&mut utterances, &rejected_phrases);
        utterances.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.label.cmp(&b.label)));
        let transcript = if labeled {
            let conversation = utterances
                .into_iter()
                .map(|item| format!("[{}] {}: {}", timestamp(item.start), item.label, item.text))
                .collect::<Vec<_>>()
                .join("\n");
            if conversation.is_empty() {
                speaker_headers.join("\n")
            } else {
                format!("{}\n\n{conversation}", speaker_headers.join("\n"))
            }
        } else {
            parts.join("\n")
        };
        let output = path.with_extension("txt");
        let partial = path.with_extension("txt.partial");
        tokio::fs::write(&partial, &transcript)
            .await
            .context("could not write transcript")?;
        #[cfg(unix)]
        tokio::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o600)).await?;
        tokio::fs::rename(&partial, &output)
            .await
            .context("could not finalize transcript")?;
        tracing::info!(engine = %artifact.origin.engine, room = %artifact.origin.room_id, recording = artifact.recording_id, path = %output.display(), "transcript saved");
        Ok(Some(transcript))
    }
}

struct TrackInput {
    reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
    label: Option<String>,
    max_samples: usize,
    chunks: usize,
    start_seconds: f64,
}

impl TrackInput {
    fn open(path: &Path, label: Option<String>, start_sample: u64) -> Result<Self> {
        let reader = hound::WavReader::open(path)
            .with_context(|| format!("could not open recording track {}", path.display()))?;
        let spec = reader.spec();
        if spec.channels != 1
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
            || spec.sample_rate == 0
        {
            bail!("transcription expects a 16-bit mono PCM WAV");
        }
        let max_samples = spec.sample_rate as usize * CHUNK_SECONDS as usize;
        let chunks = (reader.duration() as usize).div_ceil(max_samples);
        Ok(Self {
            reader,
            label,
            max_samples,
            chunks,
            start_seconds: start_sample as f64 / spec.sample_rate as f64,
        })
    }
}

struct Utterance {
    start: f64,
    label: String,
    text: String,
}

fn push_utterance(items: &mut Vec<Utterance>, start: f64, label: &str, text: &str) {
    if !start.is_finite() || text.trim().is_empty() {
        return;
    }
    items.push(Utterance {
        start: start.max(0.0),
        label: label.to_owned(),
        text: normalized_text(text),
    });
}

fn normalized_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn discard_phrases_repeated_in_silence(
    utterances: &mut Vec<Utterance>,
    rejected_phrases: &HashMap<String, usize>,
) {
    let mut accepted_counts = HashMap::new();
    for utterance in utterances.iter() {
        *accepted_counts.entry(utterance.text.clone()).or_insert(0) += 1;
    }
    utterances.retain(|utterance| {
        let rejected = rejected_phrases.get(&utterance.text).copied().unwrap_or(0);
        let accepted = accepted_counts.get(&utterance.text).copied().unwrap_or(0);
        rejected < 20 || rejected <= accepted
    });
}

fn timestamp(seconds: f64) -> String {
    let seconds = seconds as u64;
    if seconds >= 3600 {
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

struct WavChunk {
    wav: Vec<u8>,
    contains_signal: bool,
    signal_windows: Vec<bool>,
    window_samples: usize,
    sample_rate: u32,
}

impl WavChunk {
    fn signal_near(&self, start: f64, end: f64) -> bool {
        if !start.is_finite() || !end.is_finite() || start < 0.0 {
            return false;
        }
        let duration =
            self.signal_windows.len() as f64 * self.window_samples as f64 / self.sample_rate as f64;
        if start > duration + 1.0 {
            return false;
        }
        let first = ((start - 1.0).max(0.0) * self.sample_rate as f64 / self.window_samples as f64)
            as usize;
        let last = (((end + 1.0).min(duration) * self.sample_rate as f64
            / self.window_samples as f64) as usize)
            .min(self.signal_windows.len().saturating_sub(1));
        self.signal_windows
            .get(first..=last)
            .is_some_and(|windows| windows.iter().any(|&active| active))
    }
}

fn next_wav_chunk<R: std::io::Read + std::io::Seek>(
    reader: &mut hound::WavReader<R>,
    max_samples: usize,
) -> Result<Option<WavChunk>> {
    let mut cursor = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut cursor, reader.spec())?;
    let mut count = 0;
    let mut contains_signal = false;
    let sample_rate = reader.spec().sample_rate;
    let window_samples = (sample_rate as usize / 10).max(1);
    let mut signal_windows = Vec::new();
    for sample in reader.samples::<i16>().take(max_samples) {
        let sample = sample?;
        contains_signal |= sample != 0;
        if count % window_samples == 0 {
            signal_windows.push(false);
        }
        if sample != 0 {
            *signal_windows.last_mut().expect("signal window exists") = true;
        }
        writer.write_sample(sample)?;
        count += 1;
    }
    writer.finalize()?;
    if count == 0 {
        return Ok(None);
    }
    Ok(Some(WavChunk {
        wav: cursor.into_inner(),
        contains_signal,
        signal_windows,
        window_samples,
        sample_rate,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        storage::{LocalWavStorage, RecordingOrigin, RecordingStorage, SpeakerTrack},
        transcription::{TranscriptSegment, TranscriptionResult},
    };

    struct StubTranscriber;

    struct StubDiarizedTranscriber;

    struct StubWithSilentHallucination;

    #[async_trait]
    impl Transcriber for StubTranscriber {
        async fn transcribe_wav(&self, wav: Vec<u8>) -> Result<TranscriptionResult> {
            let reader = hound::WavReader::new(Cursor::new(wav))?;
            assert_eq!(reader.duration(), 1);
            Ok(TranscriptionResult {
                text: "hello world".into(),
                segments: Vec::new(),
            })
        }
    }

    #[async_trait]
    impl Transcriber for StubDiarizedTranscriber {
        async fn transcribe_wav(&self, wav: Vec<u8>) -> Result<TranscriptionResult> {
            let reader = hound::WavReader::new(Cursor::new(wav))?;
            let marker = reader
                .into_samples::<i16>()
                .find(|sample| sample.as_ref().map_or(true, |value| *value != 0))
                .expect("track contains speech")?;
            let (start, text) = match marker {
                101 => (1.5, "later speaker"),
                202 => (0.2, "earlier speaker"),
                _ => bail!("unexpected track marker"),
            };
            Ok(TranscriptionResult {
                text: text.into(),
                segments: vec![TranscriptSegment {
                    start,
                    end: start + 0.1,
                    text: text.into(),
                }],
            })
        }
    }

    #[async_trait]
    impl Transcriber for StubWithSilentHallucination {
        async fn transcribe_wav(&self, _wav: Vec<u8>) -> Result<TranscriptionResult> {
            Ok(TranscriptionResult {
                text: "spoken words Thank you.".into(),
                segments: vec![
                    TranscriptSegment {
                        start: 1.2,
                        end: 1.7,
                        text: "spoken words".into(),
                    },
                    TranscriptSegment {
                        start: 5.0,
                        end: 5.5,
                        text: "Thank you.".into(),
                    },
                ],
            })
        }
    }

    #[test]
    fn splits_wav_without_losing_samples() -> Result<()> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for sample in 0..7_i16 {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;
        let mut reader = hound::WavReader::new(Cursor::new(cursor.into_inner()))?;
        let mut restored = Vec::new();
        for expected in [3, 3, 1] {
            let chunk = next_wav_chunk(&mut reader, 3)?.expect("expected WAV chunk");
            let mut part = hound::WavReader::new(Cursor::new(chunk.wav))?;
            let values: Vec<i16> = part
                .samples::<i16>()
                .collect::<std::result::Result<_, _>>()?;
            assert_eq!(values.len(), expected);
            restored.extend(values);
        }
        assert!(next_wav_chunk(&mut reader, 3)?.is_none());
        assert_eq!(restored, (0..7).collect::<Vec<_>>());
        Ok(())
    }

    #[tokio::test]
    async fn reports_completed_chunks_and_saves_transcript() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "call-recorder-bot-progress-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&root).await?;
        let wav_path = root.join("test.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec)?;
        writer.write_sample(123_i16)?;
        writer.finalize()?;

        let (sender, mut receiver) = mpsc::unbounded_channel();
        let processor = TranscribingProcessor::new(Arc::new(StubTranscriber)).with_progress(sender);
        processor
            .on_completed(RecordingArtifact {
                origin: RecordingOrigin::new("discord", 42)?,
                recording_id: 7,
                location: wav_path.display().to_string(),
                speaker_tracks: Vec::new(),
            })
            .await?;
        assert_eq!(
            receiver.recv().await,
            Some(TranscriptionProgress::Started { total_chunks: 1 })
        );
        assert_eq!(
            receiver.recv().await,
            Some(TranscriptionProgress::ChunkCompleted {
                completed: 1,
                total_chunks: 1,
            })
        );
        assert_eq!(
            tokio::fs::read_to_string(wav_path.with_extension("txt")).await?,
            "hello world"
        );
        tokio::fs::remove_dir_all(root).await?;
        Ok(())
    }

    #[tokio::test]
    async fn orders_speaker_tracks_by_time_and_uses_participant_labels() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "call-recorder-bot-speakers-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let store = LocalWavStorage::new(root.join("recordings"));
        let mut writer = store.start(RecordingOrigin::new("discord", 42)?, 7)?;
        writer.write_tick(&vec![0_i16; 48_000], &[(1, vec![0_i16; 48_000])])?;
        writer.write_tick(
            &vec![303_i16; 48_000],
            &[(1, vec![101_i16; 48_000]), (2, vec![202_i16; 48_000])],
        )?;
        writer.identify_speaker(1, "alex".into(), "Alex".into());
        writer.identify_speaker(2, "sam".into(), "Sam".into());
        let mut artifact = writer.finish()?;
        assert_eq!(artifact.speaker_tracks[1].start_sample, 48_000);
        artifact.speaker_tracks =
            LocalWavStorage::load_speaker_tracks(Path::new(&artifact.location))?;
        let path = Path::new(&artifact.location).with_extension("txt");
        TranscribingProcessor::new(Arc::new(StubDiarizedTranscriber))
            .on_completed(artifact)
            .await?;
        assert_eq!(
            tokio::fs::read_to_string(path).await?,
            "Speaker 1: Alex\nSpeaker 2: Sam\n\n[00:01] Speaker 2: earlier speaker\n[00:01] Speaker 1: later speaker"
        );
        tokio::fs::remove_dir_all(root).await?;
        Ok(())
    }

    #[tokio::test]
    async fn omits_segments_in_silent_parts_of_a_speaker_track() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "call-recorder-bot-silent-segments-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&root).await?;
        let track_path = root.join("track.wav");
        let mut writer = hound::WavWriter::create(
            &track_path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )?;
        for sample in 0..800 {
            writer.write_sample(if (100..200).contains(&sample) {
                123_i16
            } else {
                0
            })?;
        }
        writer.finalize()?;
        let mixed_path = root.join("mixed.wav");
        TranscribingProcessor::new(Arc::new(StubWithSilentHallucination))
            .on_completed(RecordingArtifact {
                origin: RecordingOrigin::new("discord", 42)?,
                recording_id: 7,
                location: mixed_path.display().to_string(),
                speaker_tracks: vec![SpeakerTrack {
                    source_id: 1,
                    user_id: None,
                    participant_id: None,
                    participant_label: None,
                    location: track_path.display().to_string(),
                    start_sample: 0,
                }],
            })
            .await?;
        assert_eq!(
            tokio::fs::read_to_string(mixed_path.with_extension("txt")).await?,
            "Speaker 1: Unknown source (ID 1)\n\n[00:01] Speaker 1: spoken words"
        );
        tokio::fs::remove_dir_all(root).await?;
        Ok(())
    }

    #[test]
    fn suppresses_short_phrase_when_silence_rejections_dominate() {
        let mut utterances = vec![
            Utterance {
                start: 1.0,
                label: "Speaker 1".into(),
                text: "Thank you.".into(),
            },
            Utterance {
                start: 2.0,
                label: "Speaker 1".into(),
                text: "Actual speech".into(),
            },
        ];
        let rejected = HashMap::from([("Thank you.".into(), 21)]);
        discard_phrases_repeated_in_silence(&mut utterances, &rejected);
        assert_eq!(utterances.len(), 1);
        assert_eq!(utterances[0].text, "Actual speech");
    }
}
