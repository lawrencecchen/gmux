use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct AppConfig {
    pub entries: Vec<EntryConfig>,
    pub default_editor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct EntryConfig {
    pub path: PathBuf,
    pub editor: Option<String>,
}

const CONFIG_DIR: &str = "gmux";
const LEGACY_CONFIG_DIR: &str = "quickswitch";
const CONFIG_FILE_NAME: &str = "config.json";

pub fn load_config() -> Result<AppConfig> {
    let primary = config_file_path()?;
    if primary.exists() {
        return read_config(&primary);
    }

    let legacy = legacy_config_file_path()?;
    if legacy.exists() {
        return read_config(&legacy);
    }

    Ok(AppConfig::default())
}

pub fn save_config(config: &AppConfig) -> Result<()> {
    let path = config_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(config)?;
    fs::write(&path, json)
        .with_context(|| format!("failed to write config at {}", path.display()))?;
    Ok(())
}

fn config_file_path() -> Result<PathBuf> {
    Ok(config_base_dir()?.join(CONFIG_DIR).join(CONFIG_FILE_NAME))
}

fn legacy_config_file_path() -> Result<PathBuf> {
    Ok(config_base_dir()?
        .join(LEGACY_CONFIG_DIR)
        .join(CONFIG_FILE_NAME))
}

fn config_base_dir() -> Result<PathBuf> {
    dirs::config_dir().context("unable to determine config dir")
}

fn read_config(path: &Path) -> Result<AppConfig> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;

    if data.trim().is_empty() {
        return Ok(AppConfig::default());
    }

    let config: AppConfig = serde_json::from_str(&data)
        .with_context(|| format!("failed to parse config at {}", path.display()))?;
    Ok(config)
}
