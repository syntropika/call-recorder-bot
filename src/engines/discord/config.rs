use std::{collections::HashMap, io::ErrorKind, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct GuildConfig {
    pub voice_channel_id: u64,
    pub notice_channel_id: u64,
}

pub type Configs = HashMap<u64, GuildConfig>;

pub async fn load(path: &Path) -> Result<Configs> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid config JSON"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(error).context("cannot read config JSON"),
    }
}

pub async fn save(path: &Path, configs: &Configs) -> Result<()> {
    let parent = path.parent().context("config path has no parent")?;
    tokio::fs::create_dir_all(parent).await?;
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, serde_json::to_vec_pretty(configs)?).await?;
    tokio::fs::rename(&temporary, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn settings_survive_a_restart() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "call-recorder-bot-config-test-{}",
            std::process::id()
        ));
        let path = root.join("config.json");
        let configs = HashMap::from([(
            42,
            GuildConfig {
                voice_channel_id: 10,
                notice_channel_id: 11,
            },
        )]);
        save(&path, &configs).await?;
        let loaded = load(&path).await?;
        assert_eq!(loaded.get(&42).map(|cfg| cfg.voice_channel_id), Some(10));
        assert_eq!(loaded.get(&42).map(|cfg| cfg.notice_channel_id), Some(11));
        tokio::fs::remove_dir_all(root).await?;
        Ok(())
    }
}
