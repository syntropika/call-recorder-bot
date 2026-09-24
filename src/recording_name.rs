use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDateTime, Utc};

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const NAME_FORMAT: &str = "%Y-%m-%dT%H-%M-%SZ";
const NAME_LENGTH: usize = 20;

/// A UTC timestamp that sorts by recording start time and is safe in filenames.
pub fn format(recording_id: u128) -> Result<String> {
    let seconds = i64::try_from(recording_id / NANOS_PER_SECOND)
        .context("recording timestamp exceeds the supported date range")?;
    let datetime = DateTime::<Utc>::from_timestamp(seconds, 0)
        .context("recording timestamp is outside the supported date range")?;
    Ok(datetime.format(NAME_FORMAT).to_string())
}

/// Accept timestamp names with optional collision counters and legacy numeric names.
pub fn parse(name: &str) -> Result<u128> {
    if let Ok(recording_id) = name.parse::<u128>() {
        return Ok(recording_id);
    }
    let timestamp = name
        .get(..NAME_LENGTH)
        .context("recording name must start with a UTC timestamp")?;
    let suffix = name
        .get(NAME_LENGTH..)
        .context("recording name contains invalid characters")?;
    if !suffix.is_empty() {
        let counter = suffix
            .strip_prefix('-')
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value >= 2)
            .context("recording collision suffix must be a number of at least 2")?;
        if suffix != format!("-{counter}") {
            bail!("recording collision suffix is not canonical");
        }
    }
    let datetime = NaiveDateTime::parse_from_str(timestamp, NAME_FORMAT)
        .context("recording name must be a UTC timestamp or legacy numeric ID")?
        .and_utc();
    let seconds = u128::try_from(datetime.timestamp())
        .context("recording timestamp must be on or after the Unix epoch")?;
    let recording_id = seconds * NANOS_PER_SECOND;
    if format(recording_id)? != timestamp {
        bail!("recording name is not a canonical UTC timestamp");
    }
    Ok(recording_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_names_round_trip_and_sort_chronologically() -> Result<()> {
        let earlier = 1_735_689_599_000_000_001_u128;
        let later = earlier + 2_000_000_000;
        assert_eq!(format(earlier)?, "2024-12-31T23-59-59Z");
        assert_eq!(format(later)?, "2025-01-01T00-00-01Z");
        assert!(format(earlier)? < format(later)?);
        assert_eq!(parse(&format(earlier)?)?, earlier - 1);
        assert_eq!(parse(&format!("{}-2", format(earlier)?))?, earlier - 1);
        assert_eq!(parse(&earlier.to_string())?, earlier);
        Ok(())
    }
}
