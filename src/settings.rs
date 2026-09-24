use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub app: AppSection,
    pub engine: EngineSettings,
    pub logging: LoggingSettings,
    pub storage: StorageSettings,
    pub transcription: TranscriptionSettings,
    pub secrets: SecretEnvSettings,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSection {
    pub data_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineSettings {
    pub provider: EngineProvider,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EngineProvider {
    Discord,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingSettings {
    pub filter: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageSettings {
    pub provider: StorageProvider,
    pub s3: Option<S3Settings>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageProvider {
    Local,
    S3,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Settings {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub prefix: String,
    #[serde(default = "default_true")]
    pub force_path_style: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionSettings {
    pub url: String,
    pub model: String,
    pub language: String,
}

/// Names of environment variables that hold credentials; no values are stored here.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretEnvSettings {
    pub discord_token_env: String,
    pub transcription_api_key_env: String,
    pub s3_access_key_id_env: String,
    pub s3_secret_access_key_env: String,
    pub s3_session_token_env: String,
}

impl SecretEnvSettings {
    pub fn optional(name: &str) -> Option<String> {
        env::var(name).ok().filter(|value| !value.trim().is_empty())
    }

    pub fn required(name: &str) -> Result<String> {
        Self::optional(name).with_context(|| format!("{name} is required"))
    }

    fn validate(&self) -> Result<()> {
        for (field, name) in [
            ("discord_token_env", &self.discord_token_env),
            ("transcription_api_key_env", &self.transcription_api_key_env),
            ("s3_access_key_id_env", &self.s3_access_key_id_env),
            ("s3_secret_access_key_env", &self.s3_secret_access_key_env),
            ("s3_session_token_env", &self.s3_session_token_env),
        ] {
            let mut chars = name.chars();
            let valid_first = chars
                .next()
                .is_some_and(|c| c == '_' || c.is_ascii_alphabetic());
            if !valid_first || !chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
                bail!("secrets.{field} must be an environment variable name");
            }
        }
        Ok(())
    }
}

impl AppSettings {
    pub fn load(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .with_context(|| format!("could not find application config at {}", path.display()))?;
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let mut settings = Self::parse(&contents)?;
        if settings.app.data_dir.is_relative() {
            settings.app.data_dir = path
                .parent()
                .context("config path has no parent")?
                .join(&settings.app.data_dir);
        }
        Ok(settings)
    }

    fn parse(contents: &str) -> Result<Self> {
        let settings: Self = toml::from_str(contents).context("invalid application config TOML")?;
        settings.validate()?;
        Ok(settings)
    }

    fn validate(&self) -> Result<()> {
        self.secrets.validate()?;
        if self.app.data_dir.as_os_str().is_empty() {
            bail!("app.data_dir must not be empty");
        }
        if self.logging.filter.trim().is_empty() {
            bail!("logging.filter must not be empty");
        }
        if self.transcription.url.trim().is_empty()
            || self.transcription.model.trim().is_empty()
            || self.transcription.language.trim().is_empty()
        {
            bail!("transcription settings must not be empty");
        }
        if self.storage.provider == StorageProvider::S3 {
            self.s3()?;
        }
        if let Some(s3) = &self.storage.s3 {
            if s3.bucket.trim().is_empty() || s3.region.trim().is_empty() {
                bail!("storage.s3.bucket and storage.s3.region must not be empty");
            }
            if s3
                .endpoint
                .as_ref()
                .is_some_and(|endpoint| endpoint.trim().is_empty())
            {
                bail!("storage.s3.endpoint must not be empty when set");
            }
        }
        Ok(())
    }

    pub fn s3(&self) -> Result<&S3Settings> {
        self.storage
            .s3
            .as_ref()
            .context("[storage.s3] is required for the S3 provider or --archive")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_config_is_valid() -> Result<()> {
        let settings = AppSettings::parse(include_str!("../config.toml"))?;
        assert_eq!(settings.storage.provider, StorageProvider::S3);
        assert_eq!(settings.engine.provider, EngineProvider::Discord);
        assert_eq!(settings.s3()?.bucket, "discord-voice-bot");
        assert_eq!(settings.s3()?.prefix, "recording");
        assert_eq!(settings.app.data_dir, Path::new("data"));
        assert_eq!(settings.secrets.discord_token_env, "DISCORD_TOKEN");
        Ok(())
    }

    #[test]
    fn s3_requires_provider_settings() {
        let mut config: toml::Value =
            toml::from_str(include_str!("../config.toml")).expect("valid checked-in TOML");
        config["storage"]
            .as_table_mut()
            .expect("storage table")
            .remove("s3");
        assert!(AppSettings::parse(&config.to_string()).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let config = include_str!("../config.toml").replace("data_dir =", "data_directory =");
        assert!(AppSettings::parse(&config).is_err());
    }

    #[test]
    fn invalid_secret_environment_name_is_rejected() {
        let config = include_str!("../config.toml").replace(
            "discord_token_env = \"DISCORD_TOKEN\"",
            "discord_token_env = \"BAD-NAME\"",
        );
        assert!(AppSettings::parse(&config).is_err());
    }
}
