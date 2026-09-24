use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufWriter, ErrorKind},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context, Result};

use crate::recording_name;

use super::{RecordingArtifact, RecordingOrigin, RecordingStorage, RecordingWriter, SpeakerTrack};

type WavFile = hound::WavWriter<std::io::BufWriter<fs::File>>;

fn wav_spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

fn private_wav_from_file(path: &Path, file: fs::File) -> Result<WavFile> {
    let wav = hound::WavWriter::new(BufWriter::new(file), wav_spec())
        .context("could not initialize WAV file")?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(wav)
}

fn create_private_wav(path: &Path) -> Result<WavFile> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .context("could not create WAV file")?;
    private_wav_from_file(path, file)
}

fn write_samples(wav: &mut WavFile, samples: &[i16]) -> Result<()> {
    for &sample in samples {
        wav.write_sample(sample)?;
    }
    Ok(())
}

fn write_silence(wav: &mut WavFile, count: usize) -> Result<()> {
    for _ in 0..count {
        wav.write_sample(0_i16)?;
    }
    Ok(())
}

pub struct LocalWavStorage {
    root: PathBuf,
}

impl LocalWavStorage {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn load_speaker_tracks(recording: &Path) -> Result<Vec<SpeakerTrack>> {
        let manifest = recording.with_extension("speakers.json");
        if !manifest.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&manifest).context("could not read speaker manifest")?;
        let mut tracks: Vec<SpeakerTrack> =
            serde_json::from_slice(&bytes).context("could not parse speaker manifest")?;
        for track in &mut tracks {
            if track.participant_id.is_none()
                && let Some(user_id) = track.user_id
            {
                track.participant_id = Some(user_id.to_string());
                track.participant_label = Some(format!("Discord user <@{user_id}>"));
            }
        }
        Ok(tracks)
    }
}

impl RecordingStorage for LocalWavStorage {
    fn start(
        &self,
        origin: RecordingOrigin,
        recording_id: u128,
    ) -> Result<Box<dyn RecordingWriter>> {
        fs::create_dir_all(&self.root)?;
        #[cfg(unix)]
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        let engine_folder = self.root.join(&origin.engine);
        fs::create_dir_all(&engine_folder)?;
        #[cfg(unix)]
        fs::set_permissions(&engine_folder, fs::Permissions::from_mode(0o700))?;
        let folder = engine_folder.join(&origin.room_id);
        fs::create_dir_all(&folder)?;
        #[cfg(unix)]
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))?;
        let timestamp = recording_name::format(recording_id)?;
        let mut counter = 1_u32;
        let (recording_name, path, file) = loop {
            let recording_name = if counter == 1 {
                timestamp.clone()
            } else {
                format!("{timestamp}-{counter}")
            };
            let path = folder.join(format!("{recording_name}.wav"));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => break (recording_name, path, file),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    counter = counter
                        .checked_add(1)
                        .context("too many recordings in one second")?;
                }
                Err(err) => return Err(err).context("could not create recording WAV file"),
            }
        };
        let wav = private_wav_from_file(&path, file)?;
        let folder = fs::canonicalize(folder)?;
        let path = fs::canonicalize(path)?;
        Ok(Box::new(LocalWavWriter {
            origin,
            recording_id,
            recording_name,
            folder,
            path,
            wav,
            samples_written: 0,
            tracks: HashMap::new(),
            participants: HashMap::new(),
        }))
    }
}

struct LocalWavWriter {
    origin: RecordingOrigin,
    recording_id: u128,
    recording_name: String,
    folder: PathBuf,
    path: PathBuf,
    wav: WavFile,
    samples_written: usize,
    tracks: HashMap<u32, SpeakerWavWriter>,
    participants: HashMap<u32, Option<(String, String)>>,
}

struct SpeakerWavWriter {
    path: PathBuf,
    wav: WavFile,
    start_sample: u64,
}

impl RecordingWriter for LocalWavWriter {
    fn write_tick(&mut self, mixed: &[i16], speakers: &[(u32, Vec<i16>)]) -> Result<()> {
        write_samples(&mut self.wav, mixed)?;
        let present: HashSet<u32> = speakers.iter().map(|(source_id, _)| *source_id).collect();
        for (source_id, track) in &mut self.tracks {
            if !present.contains(source_id) {
                write_silence(&mut track.wav, mixed.len())?;
            }
        }
        for (source_id, samples) in speakers {
            if !self.tracks.contains_key(source_id) {
                let path = self
                    .folder
                    .join(format!("{}.source-{}.wav", self.recording_name, source_id));
                let wav = create_private_wav(&path)?;
                self.tracks.insert(
                    *source_id,
                    SpeakerWavWriter {
                        path,
                        wav,
                        start_sample: self.samples_written as u64,
                    },
                );
            }
            let track = self
                .tracks
                .get_mut(source_id)
                .expect("speaker track was created");
            write_samples(&mut track.wav, &samples[..samples.len().min(mixed.len())])?;
            if samples.len() < mixed.len() {
                write_silence(&mut track.wav, mixed.len() - samples.len())?;
            }
        }
        self.samples_written += mixed.len();
        Ok(())
    }

    fn identify_speaker(&mut self, source_id: u32, participant_id: String, label: String) {
        self.participants
            .entry(source_id)
            .and_modify(|known| {
                if known
                    .as_ref()
                    .is_some_and(|(known_id, _)| known_id != &participant_id)
                {
                    *known = None;
                }
            })
            .or_insert(Some((participant_id, label)));
    }

    fn finish(self: Box<Self>) -> Result<RecordingArtifact> {
        self.wav.finalize().context("could not finalize WAV file")?;
        let mut speaker_tracks = Vec::new();
        for (source_id, track) in self.tracks {
            track
                .wav
                .finalize()
                .context("could not finalize speaker WAV")?;
            let participant = self.participants.get(&source_id).and_then(Option::as_ref);
            speaker_tracks.push(SpeakerTrack {
                source_id,
                user_id: None,
                participant_id: participant.map(|(id, _)| id.clone()),
                participant_label: participant.map(|(_, label)| label.clone()),
                location: track.path.display().to_string(),
                start_sample: track.start_sample,
            });
        }
        speaker_tracks.sort_by_key(|track| track.source_id);
        let manifest = self.path.with_extension("speakers.json");
        let pending = self.path.with_extension("speakers.json.partial");
        fs::write(&pending, serde_json::to_vec_pretty(&speaker_tracks)?)?;
        #[cfg(unix)]
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o600))?;
        fs::rename(pending, manifest).context("could not finalize speaker manifest")?;
        Ok(RecordingArtifact {
            origin: self.origin,
            recording_id: self.recording_id,
            location: self.path.display().to_string(),
            speaker_tracks,
        })
    }

    fn abort(self: Box<Self>) -> Result<()> {
        let path = self.path.clone();
        let track_paths: Vec<_> = self
            .tracks
            .values()
            .map(|track| track.path.clone())
            .collect();
        drop(self.tracks);
        drop(self.wav);
        fs::remove_file(path).context("could not delete failed recording")?;
        for track_path in track_paths {
            fs::remove_file(track_path).context("could not delete failed speaker track")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_adapter_writes_valid_mono_wav() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("call-recorder-bot-test-{}", std::process::id()));
        let store = LocalWavStorage::new(root.clone());
        let mut writer = store.start(RecordingOrigin::new("discord", 42)?, 7)?;
        writer.write_tick(&[0, 1234, -1234], &[(99, vec![0, 1234, -1234])])?;
        writer.identify_speaker(99, "123".into(), "Discord user <@123>".into());
        writer.write_tick(&[7, 8, 9], &[(100, vec![7, 8, 9])])?;
        writer.identify_speaker(100, "124".into(), "Discord user <@124>".into());
        let artifact = writer.finish()?;
        assert_eq!(artifact.speaker_tracks.len(), 2);
        assert_eq!(
            artifact.speaker_tracks[0].participant_id.as_deref(),
            Some("123")
        );
        assert_eq!(
            artifact.speaker_tracks[1].participant_id.as_deref(),
            Some("124")
        );
        assert_eq!(artifact.speaker_tracks[0].start_sample, 0);
        assert_eq!(artifact.speaker_tracks[1].start_sample, 3);
        assert_eq!(
            LocalWavStorage::load_speaker_tracks(Path::new(&artifact.location))?.len(),
            2
        );
        let mut wav = hound::WavReader::open(&artifact.location)?;
        assert_eq!(wav.spec().channels, 1);
        assert_eq!(wav.spec().sample_rate, 48_000);
        assert_eq!(
            wav.samples::<i16>()
                .collect::<std::result::Result<Vec<_>, _>>()?,
            [0, 1234, -1234, 7, 8, 9]
        );
        let mut first = hound::WavReader::open(&artifact.speaker_tracks[0].location)?;
        assert_eq!(
            first
                .samples::<i16>()
                .collect::<std::result::Result<Vec<_>, _>>()?,
            [0, 1234, -1234, 0, 0, 0]
        );
        let mut second = hound::WavReader::open(&artifact.speaker_tracks[1].location)?;
        assert_eq!(
            second
                .samples::<i16>()
                .collect::<std::result::Result<Vec<_>, _>>()?,
            [7, 8, 9]
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn recordings_started_in_the_same_second_keep_distinct_files() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "call-recorder-bot-collision-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let store = LocalWavStorage::new(root.clone());
        let first = store
            .start(
                RecordingOrigin::new("discord", 42)?,
                1_735_689_599_000_000_001,
            )?
            .finish()?;
        let second = store
            .start(
                RecordingOrigin::new("discord", 42)?,
                1_735_689_599_999_999_999,
            )?
            .finish()?;
        assert_ne!(first.location, second.location);
        assert!(first.location.ends_with("2024-12-31T23-59-59Z.wav"));
        assert!(second.location.ends_with("2024-12-31T23-59-59Z-2.wav"));
        assert!(Path::new(&first.location).exists());
        assert!(Path::new(&second.location).exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn legacy_manifest_retains_discord_speaker_labels() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "call-recorder-bot-legacy-manifest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir_all(&root)?;
        let wav = root.join("old.wav");
        fs::write(
            wav.with_extension("speakers.json"),
            r#"[{"ssrc":7,"user_id":123,"location":"old.ssrc-7.wav","start_sample":0}]"#,
        )?;
        let tracks = LocalWavStorage::load_speaker_tracks(&wav)?;
        assert_eq!(tracks[0].participant_id.as_deref(), Some("123"));
        assert_eq!(
            tracks[0].participant_label.as_deref(),
            Some("Discord user <@123>")
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
