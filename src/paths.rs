//! Locations of config, models and logs under `%APPDATA%\VoicePaste\`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::BaseDirs;

pub fn app_dir() -> Result<PathBuf> {
    let base = BaseDirs::new().context("cannot determine user directories")?;
    Ok(base.config_dir().join("VoicePaste"))
}

pub fn models_dir() -> Result<PathBuf> {
    Ok(app_dir()?.join("models"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(app_dir()?.join("config.toml"))
}

pub fn logs_dir() -> Result<PathBuf> {
    Ok(app_dir()?.join("logs"))
}
