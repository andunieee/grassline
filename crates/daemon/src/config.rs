//! The daemon's settings, kept as JSON and edited from the admin interface.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Directories whose subdirectories are projects.
    pub project_roots: Vec<String>,
    /// Directories that are projects themselves.
    pub project_dirs: Vec<String>,
    pub default_provider: String,
    /// Lets agents run tools without asking; nobody is there to answer.
    pub auto_approve: bool,
    /// Overrides for the agent binaries, by provider name.
    pub binaries: std::collections::BTreeMap<String, String>,
    pub admin_http: Listener,
    /// Serve the admin interface over iroh to the keys in `admin_keys`.
    pub admin_iroh: bool,
    /// Iroh keys (hex) allowed into the admin interface from outside this
    /// machine.
    pub admin_keys: Vec<String>,
    /// Accept remote controllers (the Android app) over iroh.
    pub mobile: bool,
    pub paired: Vec<Paired>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listener {
    pub enabled: bool,
    pub addr: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Paired {
    pub key: String,
    pub name: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            project_roots: vec![],
            project_dirs: vec![],
            default_provider: "claude".into(),
            auto_approve: false,
            binaries: Default::default(),
            // Localhost only until the admin says otherwise.
            admin_http: Listener {
                enabled: true,
                addr: "127.0.0.1:7878".into(),
            },
            admin_iroh: false,
            admin_keys: vec![],
            mobile: true,
            paired: vec![],
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                eprintln!("{}: {e}; using defaults", path.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(self).expect("serializable"),
        )?;
        std::fs::rename(tmp, path)
    }

    pub fn options(&self, provider: grassline_agents::Provider) -> grassline_agents::Options {
        grassline_agents::Options {
            binary: self
                .binaries
                .get(provider.name())
                .cloned()
                .filter(|b| !b.is_empty()),
            auto_approve: self.auto_approve,
        }
    }
}

fn dir_from_env(var: &str, fallback: &[&str]) -> PathBuf {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => {
            let mut p = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            p.extend(fallback);
            p
        }
    }
}

/// `$XDG_CONFIG_HOME/grassline` (or `~/.config/grassline`).
pub fn config_dir() -> PathBuf {
    dir_from_env("XDG_CONFIG_HOME", &[".config"]).join("grassline")
}

/// `$XDG_DATA_HOME/grassline` (or `~/.local/share/grassline`).
pub fn data_dir() -> PathBuf {
    dir_from_env("XDG_DATA_HOME", &[".local", "share"]).join("grassline")
}
