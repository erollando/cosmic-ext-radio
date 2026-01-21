use crate::models::StationRef;
use anyhow::{Context, Result};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub last_station: Option<StationRef>,
    #[serde(default)]
    pub last_server: Option<String>,
    #[serde(default)]
    pub favorites: Vec<StationRef>,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("Failed to read config: {path:?}")),
        };
        let text = String::from_utf8_lossy(&bytes);
        toml::from_str(&text).with_context(|| format!("Invalid config TOML: {path:?}"))
    }

    pub fn save_atomic(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            ensure_private_dir(parent)?;
        }
        let data = toml::to_string_pretty(self).context("Failed to serialize config")?;

        let parent = path.parent().context("Config path has no parent")?;
        let suffix: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(10)
            .map(char::from)
            .collect();
        let tmp = parent.join(format!(
            ".{}.tmp.{suffix}",
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("config.toml")
        ));

        {
            let mut file = fs::File::create(&tmp).with_context(|| format!("Create temp file: {tmp:?}"))?;
            file.write_all(data.as_bytes())
                .with_context(|| format!("Write temp file: {tmp:?}"))?;
            file.sync_all()
                .with_context(|| format!("Sync temp file: {tmp:?}"))?;
        }

        fs::rename(&tmp, &path).with_context(|| format!("Atomic rename to: {path:?}"))?;

        if let Some(dir) = path.parent() {
            let dir_file = fs::File::open(dir).with_context(|| format!("Open config dir: {dir:?}"))?;
            let _ = dir_file.sync_all();
        }

        Ok(())
    }

    pub fn toggle_favorite(&mut self, station: StationRef) {
        if let Some(idx) = self
            .favorites
            .iter()
            .position(|s| s.stationuuid == station.stationuuid)
        {
            self.favorites.remove(idx);
        } else {
            self.favorites.push(station);
        }
    }
}

fn config_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
        })
        .context("Could not determine XDG config directory")?;
    Ok(base.join("radiowidget").join("config.toml"))
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    fs::create_dir_all(path).with_context(|| format!("Create config dir: {path:?}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("Set permissions on config dir: {path:?}"))?;
    }
    Ok(())
}
