use std::{
    env, fmt, fs,
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
    pub discord: Option<DiscordSettings>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordSettings {
    pub token: ValueSource,
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
    pub bucket: ValueSource,
    pub region: String,
    pub endpoint: Option<ValueSource>,
    pub access_key_id: ValueSource,
    pub secret_access_key: ValueSource,
    pub session_token: Option<ValueSource>,
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
    pub url: ValueSource,
    pub api_key: Option<ValueSource>,
    pub model: String,
    pub language: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueSource {
    value: Option<String>,
    env: Option<String>,
}

impl fmt::Debug for ValueSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut output = formatter.debug_struct("ValueSource");
        if let Some(name) = &self.env {
            output.field("env", name);
        } else {
            output.field("value", &"<redacted>");
        }
        output.finish()
    }
}

fn validate_env_name(field: &str, name: &str) -> Result<()> {
    let mut chars = name.chars();
    let valid_first = chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic());
    if !valid_first || !chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        bail!("{field} must be an environment variable name");
    }
    Ok(())
}

impl ValueSource {
    fn validate(&self, field: &str) -> Result<()> {
        match (&self.value, &self.env) {
            (Some(value), None) if !value.trim().is_empty() => Ok(()),
            (None, Some(name)) => validate_env_name(&format!("{field}.env"), name),
            (Some(_), Some(_)) => bail!("{field} must contain only value or env"),
            (None, None) => bail!("{field} must contain value or env"),
            (Some(_), None) => bail!("{field}.value must not be empty"),
        }
    }

    pub fn required(&self) -> Result<String> {
        self.required_with(|name| env::var(name).ok())
    }

    fn required_with(&self, lookup: impl FnOnce(&str) -> Option<String>) -> Result<String> {
        self.resolve_with(lookup).with_context(|| match &self.env {
            Some(name) => format!("{name} is required"),
            None => "configured value is required".to_owned(),
        })
    }

    pub fn optional(&self) -> Option<String> {
        self.resolve_with(|name| env::var(name).ok())
    }

    fn resolve_with(&self, lookup: impl FnOnce(&str) -> Option<String>) -> Option<String> {
        if let Some(name) = &self.env {
            return lookup(name)
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.trim().to_owned());
        }
        self.value.clone()
    }
}

impl S3Settings {
    pub fn resolved_bucket(&self) -> Result<String> {
        self.bucket.required()
    }

    pub fn resolved_endpoint(&self) -> Result<Option<String>> {
        self.endpoint
            .as_ref()
            .map(ValueSource::required)
            .transpose()
    }
}

impl TranscriptionSettings {
    pub fn resolved_url(&self) -> Result<String> {
        self.url.required()
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
        if self.app.data_dir.as_os_str().is_empty() {
            bail!("app.data_dir must not be empty");
        }
        if self.logging.filter.trim().is_empty() {
            bail!("logging.filter must not be empty");
        }
        if self.engine.provider == EngineProvider::Discord {
            self.discord()?;
        }
        if let Some(discord) = &self.engine.discord {
            discord.token.validate("engine.discord.token")?;
        }
        self.transcription.url.validate("transcription.url")?;
        if let Some(api_key) = &self.transcription.api_key {
            api_key.validate("transcription.api_key")?;
        }
        if self.transcription.model.trim().is_empty()
            || self.transcription.language.trim().is_empty()
        {
            bail!("transcription settings must not be empty");
        }
        if self.storage.provider == StorageProvider::S3 {
            self.s3()?;
        }
        if let Some(s3) = &self.storage.s3 {
            s3.bucket.validate("storage.s3.bucket")?;
            if s3.region.trim().is_empty() {
                bail!("storage.s3.region must not be empty");
            }
            if let Some(endpoint) = &s3.endpoint {
                endpoint.validate("storage.s3.endpoint")?;
            }
            s3.access_key_id.validate("storage.s3.access_key_id")?;
            s3.secret_access_key
                .validate("storage.s3.secret_access_key")?;
            if let Some(session_token) = &s3.session_token {
                session_token.validate("storage.s3.session_token")?;
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

    pub fn discord(&self) -> Result<&DiscordSettings> {
        self.engine
            .discord
            .as_ref()
            .context("[engine.discord] is required for the Discord engine")
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
        assert_eq!(settings.s3()?.bucket.env.as_deref(), Some("S3_BUCKET"));
        assert_eq!(settings.s3()?.prefix, "recording");
        assert_eq!(
            settings
                .s3()?
                .endpoint
                .as_ref()
                .and_then(|source| source.env.as_deref()),
            Some("S3_ENDPOINT")
        );
        assert_eq!(
            settings.transcription.url.env.as_deref(),
            Some("TRANSCRIPTION_URL")
        );
        assert_eq!(settings.app.data_dir, Path::new("data"));
        assert_eq!(
            settings.discord()?.token.env.as_deref(),
            Some("DISCORD_TOKEN")
        );
        assert_eq!(
            settings.s3()?.access_key_id.env.as_deref(),
            Some("S3_ACCESS_KEY_ID")
        );
        assert_eq!(
            settings
                .transcription
                .api_key
                .as_ref()
                .and_then(|source| source.env.as_deref()),
            Some("TRANSCRIPTION_API_KEY")
        );
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
            "token = { env = \"DISCORD_TOKEN\" }",
            "token = { env = \"BAD-NAME\" }",
        );
        assert!(AppSettings::parse(&config).is_err());
    }

    #[test]
    fn literal_values_are_supported() -> Result<()> {
        let config = include_str!("../config.toml")
            .replace(
                "endpoint = { env = \"S3_ENDPOINT\" }",
                "endpoint = { value = \"https://s3.example.test\" }",
            )
            .replace(
                "url = { env = \"TRANSCRIPTION_URL\" }",
                "url = { value = \"https://api.example.test/audio\" }",
            )
            .replace(
                "token = { env = \"DISCORD_TOKEN\" }",
                "token = { value = \"test-discord-token\" }",
            );
        let settings = AppSettings::parse(&config)?;
        assert_eq!(
            settings.s3()?.resolved_endpoint()?.as_deref(),
            Some("https://s3.example.test")
        );
        assert_eq!(
            settings.transcription.resolved_url()?,
            "https://api.example.test/audio"
        );
        assert_eq!(settings.discord()?.token.required()?, "test-discord-token");
        Ok(())
    }

    #[test]
    fn environment_value_resolves_and_missing_value_fails() -> Result<()> {
        let settings = AppSettings::parse(include_str!("../config.toml"))?;
        let endpoint = settings.s3()?.endpoint.as_ref().context("S3 endpoint")?;
        let resolved = endpoint.required_with(|name| {
            (name == "S3_ENDPOINT").then(|| " https://s3.example.test ".into())
        })?;
        assert_eq!(resolved, "https://s3.example.test");
        let missing = settings.transcription.url.required_with(|_| None);
        assert_eq!(
            missing.unwrap_err().to_string(),
            "TRANSCRIPTION_URL is required"
        );
        assert_eq!(
            settings
                .transcription
                .api_key
                .as_ref()
                .and_then(|source| source.resolve_with(|_| None)),
            None
        );
        Ok(())
    }

    #[test]
    fn ambiguous_and_invalid_value_sources_are_rejected() {
        let config = include_str!("../config.toml").replace(
            "endpoint = { env = \"S3_ENDPOINT\" }",
            "endpoint = { env = \"S3_ENDPOINT\", value = \"https://s3.example.test\" }",
        );
        assert!(AppSettings::parse(&config).is_err());

        let config = include_str!("../config.toml").replace(
            "url = { env = \"TRANSCRIPTION_URL\" }",
            "url = { env = \"BAD-NAME\" }",
        );
        assert!(AppSettings::parse(&config).is_err());

        let config = include_str!("../config.toml").replace(
            "token = { env = \"DISCORD_TOKEN\" }",
            "token = { value = \"\" }",
        );
        assert!(AppSettings::parse(&config).is_err());
    }
}
