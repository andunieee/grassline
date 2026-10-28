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
    /// Lets agents run tools without asking. Otherwise they ask the paired
    /// app.
    pub auto_approve: bool,
    /// Overrides for the agent binaries, by provider name.
    pub binaries: std::collections::BTreeMap<String, String>,
    pub admin_http: Listener,
    /// Serve the admin interface over iroh to the keys in `admin_keys`.
    pub admin_iroh: bool,
    /// Iroh keys (hex) allowed into the admin interface from outside this
    /// machine.
    pub admin_keys: Vec<String>,
    pub paired: Vec<Paired>,
    /// Turns the app's voice messages into text.
    pub transcription: Transcription,
}

/// A speech-to-text service the app calls itself, with the key given here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transcription {
    /// A name from [`SPEECH_PROVIDERS`], or "custom"; empty when off.
    pub provider: String,
    pub key: String,
    /// Overrides the provider's model.
    pub model: String,
    /// The endpoint of a "custom" provider.
    pub url: String,
}

/// A speech-to-text service with an OpenAI-style `audio/transcriptions`
/// endpoint.
pub struct SpeechProvider {
    pub name: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    pub model: &'static str,
}

pub const SPEECH_PROVIDERS: &[SpeechProvider] = &[
    SpeechProvider {
        name: "openai",
        label: "OpenAI",
        url: "https://api.openai.com/v1/audio/transcriptions",
        model: "gpt-transcribe",
    },
    SpeechProvider {
        name: "openrouter",
        label: "OpenRouter",
        url: "https://openrouter.ai/api/v1/audio/transcriptions",
        model: "openai/gpt-transcribe",
    },
    SpeechProvider {
        name: "groq",
        label: "Groq",
        url: "https://api.groq.com/openai/v1/audio/transcriptions",
        model: "whisper-large-v3-turbo",
    },
    SpeechProvider {
        name: "mistral",
        label: "Mistral",
        url: "https://api.mistral.ai/v1/audio/transcriptions",
        model: "voxtral-mini-latest",
    },
];

impl Transcription {
    /// What the app needs to call the service, once it is set up.
    pub fn resolve(&self) -> Option<grassline_proto::Transcription> {
        let (url, model) = match SPEECH_PROVIDERS.iter().find(|p| p.name == self.provider) {
            Some(p) => (p.url.to_string(), p.model),
            None if self.provider == "custom" => (self.url.trim().to_string(), ""),
            None => return None,
        };
        let model = Some(self.model.trim())
            .filter(|m| !m.is_empty())
            .unwrap_or(model);
        if url.is_empty() || model.is_empty() || self.key.trim().is_empty() {
            return None;
        }
        Some(grassline_proto::Transcription {
            url,
            model: model.into(),
            key: self.key.trim().into(),
        })
    }
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
            paired: vec![],
            transcription: Transcription::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_needs_a_key_and_a_model() {
        let mut t = Transcription {
            provider: "groq".into(),
            ..Default::default()
        };
        assert_eq!(t.resolve(), None);
        t.key = "k".into();
        assert_eq!(t.resolve().unwrap().model, "whisper-large-v3-turbo");
        t.provider = "custom".into();
        t.url = "http://localhost:8000/v1/audio/transcriptions".into();
        assert_eq!(t.resolve(), None);
        t.model = "whisper-1".into();
        assert_eq!(t.resolve().unwrap().url, t.url);
        t.provider = String::new();
        assert_eq!(t.resolve(), None);
    }
}
