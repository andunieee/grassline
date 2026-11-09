//! The daemon's settings, kept as JSON and edited from the admin interface.

use grassline_proto::Permissions;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// What the daemon calls itself; empty uses this machine's hostname.
    pub name: String,
    /// Directories whose subdirectories are projects.
    pub project_roots: Vec<String>,
    /// Directories that are projects themselves.
    pub project_dirs: Vec<String>,
    pub default_provider: String,
    /// What the agents of new sessions may do without asking; the app
    /// changes it for each session, and [`Config::model_permissions`] keeps
    /// what was last picked with a model.
    pub permissions: Permissions,
    /// What the agent may do without asking, by model (`provider/model`):
    /// the mode last picked with that model.
    pub model_permissions: std::collections::BTreeMap<String, Permissions>,
    /// The model new sessions use, by provider name; the last one the app
    /// picked.
    pub models: std::collections::BTreeMap<String, String>,
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
    /// What the app needs to turn a voice message into text, once it is set up.
    pub fn resolve(&self) -> Option<grassline_proto::Transcription> {
        if self.provider == "local" {
            let model = match self.model.trim() {
                "" => crate::local_speech::DEFAULT_LOCAL_STT_MODEL,
                model => model,
            };
            crate::local_speech::find_model(model)?;
            return Some(grassline_proto::Transcription::Local {
                model: model.into(),
            });
        }
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
        Some(grassline_proto::Transcription::Remote {
            url,
            model: model.into(),
            key: self.key.trim().into(),
        })
    }

    /// The local model this setting uses, if it is the local provider.
    pub fn local_model(&self) -> Option<&'static crate::local_speech::ModelSpec> {
        if self.provider != "local" {
            return None;
        }
        let model = match self.model.trim() {
            "" => crate::local_speech::DEFAULT_LOCAL_STT_MODEL,
            model => model,
        };
        crate::local_speech::find_model(model)
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
            name: String::new(),
            project_roots: vec![],
            project_dirs: vec![],
            default_provider: "claude".into(),
            permissions: Permissions::Auto,
            model_permissions: Default::default(),
            models: Default::default(),
            binaries: Default::default(),
            // Localhost only until the admin says otherwise.
            admin_http: Listener {
                enabled: true,
                addr: "127.0.0.1:7878".into(),
            },
            // On, but only the keys in `admin_keys` get in.
            admin_iroh: true,
            admin_keys: vec![],
            paired: vec![],
            transcription: Transcription::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let parsed = serde_json::from_str::<serde_json::Value>(&s).and_then(|v| {
                    let mut c: Config = serde_json::from_value(v.clone())?;
                    // It was a switch before.
                    if v.get("permissions").is_none() && v["auto_approve"] == true {
                        c.permissions = Permissions::All;
                    }
                    Ok(c)
                });
                parsed.unwrap_or_else(|e| {
                    eprintln!("{}: {e}; using defaults", path.display());
                    Config::default()
                })
            }
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

    /// The name to show the app and the admin interface: the configured one,
    /// or this machine's hostname.
    pub fn machine_name(&self) -> String {
        match self.name.trim() {
            "" => hostname(),
            name => name.into(),
        }
    }

    /// What this daemon calls itself: the product and the machine.
    pub fn daemon_name(&self) -> String {
        format!("grassline@{}", self.machine_name())
    }

    /// What the agent may do without asking with a model: the mode last
    /// picked with it, or the configured default.
    pub fn permissions_for(&self, provider: &str, model: &str) -> Permissions {
        self.model_permissions
            .get(&model_key(provider, model))
            .copied()
            .unwrap_or(self.permissions)
    }

    /// Remembers the mode picked with a model.
    pub fn remember_permissions(&mut self, provider: &str, model: &str, permissions: Permissions) {
        self.model_permissions
            .insert(model_key(provider, model), permissions);
    }

    pub fn options(&self, provider: grassline_agents::Provider) -> grassline_agents::Options {
        grassline_agents::Options {
            binary: self
                .binaries
                .get(provider.name())
                .cloned()
                .filter(|b| !b.is_empty()),
            ..Default::default()
        }
    }
}

/// This machine's name.
pub fn hostname() -> String {
    ["/etc/hostname", "/proc/sys/kernel/hostname"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "grassline".into())
}

/// The key a model's remembered mode is stored under.
fn model_key(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
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
    fn auto_approve_becomes_all() {
        let dir = std::env::temp_dir().join(format!("grassline-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"auto_approve":true}"#).unwrap();
        assert_eq!(Config::load(&path).permissions, Permissions::All);
        std::fs::write(&path, r#"{"auto_approve":true,"permissions":"auto"}"#).unwrap();
        assert_eq!(Config::load(&path).permissions, Permissions::Auto);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn auto_is_the_default_mode() {
        assert_eq!(Config::default().permissions, Permissions::Auto);
    }

    #[test]
    fn name_defaults_to_the_hostname() {
        let mut c = Config::default();
        assert_eq!(c.machine_name(), hostname());
        assert_eq!(c.daemon_name(), format!("grassline@{}", hostname()));
        c.name = " Studio ".into();
        assert_eq!(c.machine_name(), "Studio");
        assert_eq!(c.daemon_name(), "grassline@Studio");
        c.name = "  ".into();
        assert_eq!(c.machine_name(), hostname());
    }

    #[test]
    fn transcription_needs_a_key_and_a_model() {
        let mut t = Transcription {
            provider: "groq".into(),
            ..Default::default()
        };
        assert_eq!(t.resolve(), None);
        t.key = "k".into();
        assert!(matches!(
            t.resolve(),
            Some(grassline_proto::Transcription::Remote { model, .. })
                if model == "whisper-large-v3-turbo"
        ));
        t.provider = "custom".into();
        t.url = "http://localhost:8000/v1/audio/transcriptions".into();
        assert_eq!(t.resolve(), None);
        t.model = "whisper-1".into();
        assert!(matches!(
            t.resolve(),
            Some(grassline_proto::Transcription::Remote { url, .. }) if url == t.url
        ));
        t.provider = String::new();
        assert_eq!(t.resolve(), None);

        t.provider = "local".into();
        t.model = crate::local_speech::DEFAULT_LOCAL_STT_MODEL.into();
        assert!(matches!(
            t.resolve(),
            Some(grassline_proto::Transcription::Local { model })
                if model == crate::local_speech::DEFAULT_LOCAL_STT_MODEL
        ));
        t.model = "not-a-model".into();
        assert_eq!(t.resolve(), None);
    }
}
