pub const SAMPLES_PER_TICK: usize = 960;

/// Mix 20 ms of mono 48 kHz PCM, including silence when nobody speaks.
pub fn mix_tick<'a>(voices: impl IntoIterator<Item = &'a [i16]>) -> Vec<i16> {
    let mut mixed = [0_i32; SAMPLES_PER_TICK];
    for voice in voices {
        for (out, sample) in mixed.iter_mut().zip(voice.iter()) {
            *out += i32::from(*sample);
        }
    }
    mixed
        .into_iter()
        .map(|sample| sample.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_timing_during_silence() {
        assert_eq!(mix_tick(std::iter::empty()).len(), SAMPLES_PER_TICK);
        assert!(
            mix_tick(std::iter::empty())
                .iter()
                .all(|sample| *sample == 0)
        );
    }

    #[test]
    fn mixes_and_clips_simultaneous_speakers() {
        let first = [20_000, -20_000, 10];
        let second = [20_000, -20_000, -5];
        let output = mix_tick([first.as_slice(), second.as_slice()]);
        assert_eq!(&output[..4], &[i16::MAX, i16::MIN, 5, 0]);
    }
}
