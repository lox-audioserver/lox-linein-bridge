use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_DIR_SYSTEM: &str = "/etc/lox-linein-bridge";
const CONFIG_DIR_FALLBACK: &str = ".config/lox-linein-bridge";
const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub bridge_id: String,
    #[serde(default)]
    pub preferred_server_name: Option<String>,
    #[serde(default)]
    pub preferred_server_mac: Option<String>,
}

pub fn preferred_config_path() -> PathBuf {
    PathBuf::from(CONFIG_DIR_SYSTEM).join(CONFIG_FILE)
}

pub fn fallback_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(CONFIG_DIR_FALLBACK)
        .join(CONFIG_FILE))
}

pub fn write_config(config: &Config) -> Result<PathBuf> {
    let contents = toml::to_string_pretty(config).context("serialize config")?;
    let preferred = preferred_config_path();
    if try_write(&preferred, &contents).is_ok() {
        return Ok(preferred);
    }

    let fallback = fallback_config_path()?;
    try_write(&fallback, &contents).context("write fallback config")?;
    Ok(fallback)
}

pub fn load_or_create_config() -> Result<(Config, PathBuf)> {
    let preferred = preferred_config_path();
    if preferred.exists() {
        let data = fs::read_to_string(&preferred).context("read config")?;
        let config = toml::from_str(&data).context("parse config")?;
        return Ok((config, preferred));
    }

    let fallback = fallback_config_path()?;
    if fallback.exists() {
        let data = fs::read_to_string(&fallback).context("read fallback config")?;
        let config = toml::from_str(&data).context("parse fallback config")?;
        return Ok((config, fallback));
    }

    let config = Config {
        bridge_id: uuid::Uuid::new_v4().to_string(),
        preferred_server_name: None,
        preferred_server_mac: None,
    };
    let path = write_config(&config)?;
    Ok((config, path))
}

fn try_write(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}
