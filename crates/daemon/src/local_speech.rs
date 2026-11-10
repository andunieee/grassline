//! Local speech-to-text models: the catalog, their downloads, and inference.
//!
//! The models are the sherpa-onnx releases also used by Paseo. They run on the
//! daemon's CPU with ONNX Runtime; nothing leaves the machine.

use sherpa_onnx::{
    OfflineDolphinModelConfig, OfflineNemoEncDecCtcModelConfig, OfflineRecognizer,
    OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, OfflineTransducerModelConfig,
    OfflineWhisperModelConfig,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;

/// The default local model: accurate English on a laptop CPU.
pub const DEFAULT_LOCAL_STT_MODEL: &str = "parakeet-tdt-0.6b-v2-int8";

macro_rules! archive {
    ($name:literal) => {
        concat!(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/",
            $name,
            ".tar.bz2"
        )
    };
}

pub const LOCAL_STT_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "parakeet-tdt-0.6b-v2-int8",
        label: "Parakeet v2",
        description: "Accurate English transcription that runs well on a laptop CPU.",
        languages: &["en"],
        size: 482_440_461,
        archive_url: archive!("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8"),
        dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
        engine: Engine::Transducer {
            encoder: "encoder.int8.onnx",
            decoder: "decoder.int8.onnx",
            joiner: "joiner.int8.onnx",
            tokens: "tokens.txt",
        },
    },
    ModelSpec {
        id: "parakeet-tdt-110m-en-int8",
        label: "Parakeet Small",
        description: "Small, fast English model with punctuation. A quick download for any laptop.",
        languages: &["en"],
        size: 108_035_095,
        archive_url: archive!("sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8"),
        dir: "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8",
        engine: Engine::Transducer {
            encoder: "encoder.int8.onnx",
            decoder: "decoder.int8.onnx",
            joiner: "joiner.int8.onnx",
            tokens: "tokens.txt",
        },
    },
    ModelSpec {
        id: "parakeet-tdt-0.6b-v3-int8",
        label: "Parakeet v3",
        description: "Parakeet for 25 European languages, detecting which one you speak.",
        languages: &[
            "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv",
            "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
        ],
        size: 487_211_443,
        archive_url: archive!("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"),
        dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        engine: Engine::Transducer {
            encoder: "encoder.int8.onnx",
            decoder: "decoder.int8.onnx",
            joiner: "joiner.int8.onnx",
            tokens: "tokens.txt",
        },
    },
    ModelSpec {
        id: "whisper-tiny-en",
        label: "Whisper Tiny (English)",
        description: "Small English Whisper model. Quick to download, less accurate than Parakeet.",
        languages: &["en"],
        size: 118_071_777,
        archive_url: archive!("sherpa-onnx-whisper-tiny.en"),
        dir: "sherpa-onnx-whisper-tiny.en",
        engine: Engine::Whisper {
            encoder: "tiny.en-encoder.int8.onnx",
            decoder: "tiny.en-decoder.int8.onnx",
            tokens: "tiny.en-tokens.txt",
        },
    },
    ModelSpec {
        id: "whisper-base",
        label: "Whisper Base",
        description: "Compact Whisper model for 99 languages, detecting which one you speak.",
        languages: WHISPER_LANGUAGES,
        size: 207_557_382,
        archive_url: archive!("sherpa-onnx-whisper-base"),
        dir: "sherpa-onnx-whisper-base",
        engine: Engine::Whisper {
            encoder: "base-encoder.int8.onnx",
            decoder: "base-decoder.int8.onnx",
            tokens: "base-tokens.txt",
        },
    },
    ModelSpec {
        id: "whisper-turbo",
        label: "Whisper Turbo",
        description:
            "Whisper's most accurate model for 99 languages and Cantonese. Slower on a CPU.",
        languages: WHISPER_LANGUAGES,
        size: 563_790_207,
        archive_url: archive!("sherpa-onnx-whisper-turbo"),
        dir: "sherpa-onnx-whisper-turbo",
        engine: Engine::Whisper {
            encoder: "turbo-encoder.int8.onnx",
            decoder: "turbo-decoder.int8.onnx",
            tokens: "turbo-tokens.txt",
        },
    },
    ModelSpec {
        id: "sense-voice-int8",
        label: "SenseVoice",
        description:
            "Chinese, Cantonese, English, Japanese and Korean, detecting which one you speak.",
        languages: &["zh", "yue", "en", "ja", "ko"],
        size: 165_783_878,
        archive_url: archive!("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09"),
        dir: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09",
        engine: Engine::SenseVoice {
            model: "model.int8.onnx",
            tokens: "tokens.txt",
        },
    },
    ModelSpec {
        id: "dolphin-base-int8",
        label: "Dolphin",
        description: "Small model for 40 Asian and Middle Eastern languages and Chinese dialects.",
        languages: DOLPHIN_LANGUAGES,
        size: 80_671_385,
        archive_url: archive!("sherpa-onnx-dolphin-base-ctc-multi-lang-int8-2025-04-02"),
        dir: "sherpa-onnx-dolphin-base-ctc-multi-lang-int8-2025-04-02",
        engine: Engine::Dolphin {
            model: "model.int8.onnx",
            tokens: "tokens.txt",
        },
    },
    ModelSpec {
        id: "giga-am-v3-ru",
        label: "GigaAM v3 (Russian)",
        description: "Russian transcription with punctuation.",
        languages: &["ru"],
        size: 170_197_019,
        archive_url: archive!("sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16"),
        dir: "sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16",
        engine: Engine::Transducer {
            encoder: "encoder.int8.onnx",
            decoder: "decoder.onnx",
            joiner: "joiner.onnx",
            tokens: "tokens.txt",
        },
    },
    ModelSpec {
        id: "parakeet-tdt-ctc-0.6b-ja-int8",
        label: "Parakeet (Japanese)",
        description: "Japanese transcription with punctuation.",
        languages: &["ja"],
        size: 489_389_564,
        archive_url: archive!("sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8"),
        dir: "sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8",
        engine: Engine::NemoCtc {
            model: "model.int8.onnx",
            tokens: "tokens.txt",
        },
    },
];

#[derive(Debug)]
pub struct ModelSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub languages: &'static [&'static str],
    /// Size of the release archive, in bytes, shown before the download
    /// starts. Upstream re-uploads archives, so it is only an estimate.
    pub size: u64,
    pub archive_url: &'static str,
    /// The directory the archive extracts to.
    pub dir: &'static str,
    pub engine: Engine,
}

#[derive(Debug)]
pub enum Engine {
    Transducer {
        encoder: &'static str,
        decoder: &'static str,
        joiner: &'static str,
        tokens: &'static str,
    },
    Whisper {
        encoder: &'static str,
        decoder: &'static str,
        tokens: &'static str,
    },
    SenseVoice {
        model: &'static str,
        tokens: &'static str,
    },
    Dolphin {
        model: &'static str,
        tokens: &'static str,
    },
    NemoCtc {
        model: &'static str,
        tokens: &'static str,
    },
}

impl Engine {
    fn files(&self) -> Vec<&'static str> {
        match self {
            Engine::Transducer {
                encoder,
                decoder,
                joiner,
                tokens,
            } => vec![encoder, decoder, joiner, tokens],
            Engine::Whisper {
                encoder,
                decoder,
                tokens,
            } => vec![encoder, decoder, tokens],
            Engine::SenseVoice { model, tokens }
            | Engine::Dolphin { model, tokens }
            | Engine::NemoCtc { model, tokens } => vec![model, tokens],
        }
    }
}

pub fn find_model(id: &str) -> Option<&'static ModelSpec> {
    LOCAL_STT_MODELS.iter().find(|m| m.id == id)
}

/// A model's download in progress.
#[derive(Debug, Clone, Default)]
pub struct DownloadStatus {
    pub received: u64,
    pub total: u64,
    pub error: Option<String>,
}

/// The local speech runtime: where models live, how their downloads are going,
/// and the recognizer loaded last.
pub struct LocalSpeech {
    models_dir: PathBuf,
    downloads: Mutex<HashMap<&'static str, DownloadStatus>>,
    recognizer: Mutex<Option<(&'static str, Arc<OfflineRecognizer>)>>,
    /// One decode at a time: two phones can send at once, and the recognizer
    /// is a single native object.
    inference: Mutex<()>,
}

impl LocalSpeech {
    pub fn new(data_dir: &Path) -> Arc<Self> {
        Arc::new(LocalSpeech {
            models_dir: data_dir.join("models").join("local-speech"),
            downloads: Mutex::new(HashMap::new()),
            recognizer: Mutex::new(None),
            inference: Mutex::new(()),
        })
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    pub fn installed(&self, model: &ModelSpec) -> bool {
        let dir = self.models_dir.join(model.dir);
        model.engine.files().iter().all(|f| dir.join(f).is_file())
    }

    pub fn download_status(&self, id: &str) -> Option<DownloadStatus> {
        self.downloads.lock().unwrap().get(id).cloned()
    }

    /// Changes with the downloads, so the admin can redraw when they move.
    pub fn download_snapshot(&self) -> String {
        let downloads = self.downloads.lock().unwrap();
        let mut ids: Vec<_> = downloads.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .map(|id| {
                let s = &downloads[id];
                format!(
                    "{id}:{}:{}:{}",
                    s.received,
                    s.total,
                    s.error.as_deref().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    pub fn downloading(&self) -> bool {
        self.downloads
            .lock()
            .unwrap()
            .values()
            .any(|s| s.error.is_none())
    }

    /// Starts a download unless the model is already installed or on its way.
    pub fn start_download(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let model = find_model(id).ok_or_else(|| format!("unknown speech model: {id}"))?;
        if self.installed(model) {
            return Ok(());
        }
        let handle =
            tokio::runtime::Handle::try_current().map_err(|_| "the daemon is not running")?;
        {
            let mut downloads = self.downloads.lock().unwrap();
            if downloads.get(model.id).is_some_and(|s| s.error.is_none()) {
                return Ok(());
            }
            downloads.remove(model.id);
            downloads.insert(
                model.id,
                DownloadStatus {
                    received: 0,
                    total: model.size,
                    error: None,
                },
            );
        }
        let speech = self.clone();
        handle.spawn(async move {
            let result = speech.download(model).await;
            let mut downloads = speech.downloads.lock().unwrap();
            match result {
                Ok(()) => {
                    downloads.remove(model.id);
                }
                Err(e) => {
                    if let Some(s) = downloads.get_mut(model.id) {
                        s.error = Some(e);
                    }
                }
            }
        });
        Ok(())
    }

    async fn download(&self, model: &'static ModelSpec) -> Result<(), String> {
        let downloads_dir = self.models_dir.join(".downloads");
        tokio::fs::create_dir_all(&downloads_dir)
            .await
            .map_err(|e| format!("cannot make {}: {e}", downloads_dir.display()))?;
        let archive_path = downloads_dir.join(format!("{}.tar.bz2", model.id));
        let _ = tokio::fs::remove_file(&archive_path).await;

        let response = reqwest::get(model.archive_url)
            .await
            .map_err(|e| format!("cannot download {}: {e}", model.label))?;
        if !response.status().is_success() {
            return Err(format!(
                "cannot download {}: the server said {}",
                model.label,
                response.status()
            ));
        }
        let mut file = tokio::fs::File::create(&archive_path)
            .await
            .map_err(|e| format!("cannot write {}: {e}", archive_path.display()))?;
        if let Some(total) = response.content_length() {
            if let Some(s) = self.downloads.lock().unwrap().get_mut(model.id) {
                s.total = total;
            }
        }
        let mut response = response;
        let mut received = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("download of {} failed: {e}", model.label))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("cannot write {}: {e}", archive_path.display()))?;
            received += chunk.len() as u64;
            if let Some(s) = self.downloads.lock().unwrap().get_mut(model.id) {
                s.received = received;
            }
        }
        file.flush()
            .await
            .map_err(|e| format!("cannot write {}: {e}", archive_path.display()))?;
        drop(file);

        let (models_dir, spec) = (self.models_dir.clone(), model);
        let archive = archive_path.clone();
        tokio::task::spawn_blocking(move || extract(&models_dir, spec, &archive))
            .await
            .map_err(|e| format!("cannot unpack {}: {e}", model.label))??;

        let _ = tokio::fs::remove_file(&archive_path).await;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let model = find_model(id).ok_or_else(|| format!("unknown speech model: {id}"))?;
        if self.downloads.lock().unwrap().contains_key(model.id) {
            return Err("the model is still downloading".into());
        }
        let dir = self.models_dir.join(model.dir);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("cannot delete {}: {e}", dir.display()))?;
        }
        let mut recognizer = self.recognizer.lock().unwrap();
        if recognizer
            .as_ref()
            .is_some_and(|(loaded, _)| *loaded == model.id)
        {
            *recognizer = None;
        }
        Ok(())
    }

    /// Turns a 16 kHz mono WAV into text with `id`. CPU-heavy; call it from
    /// `spawn_blocking`.
    pub fn transcribe(&self, id: &str, wav: &[u8]) -> Result<String, String> {
        let model = find_model(id).ok_or_else(|| format!("unknown speech model: {id}"))?;
        if !self.installed(model) {
            return Err(format!(
                "{} is not downloaded yet; open the Voice settings on the daemon",
                model.label
            ));
        }
        let (sample_rate, samples) = wav_to_f32(wav)?;
        if samples.is_empty() {
            return Ok(String::new());
        }
        // A quiet recording is silence, not a failed decode.
        let peak = samples.iter().fold(0f32, |p, s| p.max(s.abs()));
        if peak < 0.009 {
            return Ok(String::new());
        }

        let recognizer = self.recognizer(model)?;
        let _inference = self.inference.lock().unwrap();
        let stream = recognizer.create_stream();
        stream.accept_waveform(sample_rate, &samples);
        recognizer.decode(&stream);
        let text = stream
            .get_result()
            .map(|r| r.text)
            .unwrap_or_default()
            .trim()
            .to_string();
        Ok(text)
    }

    fn recognizer(&self, model: &'static ModelSpec) -> Result<Arc<OfflineRecognizer>, String> {
        let mut loaded = self.recognizer.lock().unwrap();
        if let Some((id, recognizer)) = loaded.as_ref() {
            if *id == model.id {
                return Ok(recognizer.clone());
            }
        }

        let dir = self.models_dir.join(model.dir);
        let file = |name: &str| dir.join(name).to_string_lossy().into_owned();
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.num_threads = 2;
        config.model_config.provider = Some("cpu".into());
        match &model.engine {
            Engine::Transducer {
                encoder,
                decoder,
                joiner,
                tokens,
            } => {
                config.model_config.transducer = OfflineTransducerModelConfig {
                    encoder: Some(file(encoder)),
                    decoder: Some(file(decoder)),
                    joiner: Some(file(joiner)),
                };
                config.model_config.tokens = Some(file(tokens));
                config.model_config.model_type = Some("nemo_transducer".into());
            }
            Engine::Whisper {
                encoder,
                decoder,
                tokens,
            } => {
                config.model_config.whisper = OfflineWhisperModelConfig {
                    encoder: Some(file(encoder)),
                    decoder: Some(file(decoder)),
                    language: Some(String::new()),
                    task: Some("transcribe".into()),
                    tail_paddings: 0,
                    enable_token_timestamps: false,
                    enable_segment_timestamps: false,
                };
                config.model_config.tokens = Some(file(tokens));
            }
            Engine::SenseVoice { model, tokens } => {
                config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                    model: Some(file(model)),
                    language: Some("auto".into()),
                    use_itn: true,
                };
                config.model_config.tokens = Some(file(tokens));
            }
            Engine::Dolphin { model, tokens } => {
                config.model_config.dolphin = OfflineDolphinModelConfig {
                    model: Some(file(model)),
                };
                config.model_config.tokens = Some(file(tokens));
            }
            Engine::NemoCtc { model, tokens } => {
                config.model_config.nemo_ctc = OfflineNemoEncDecCtcModelConfig {
                    model: Some(file(model)),
                };
                config.model_config.tokens = Some(file(tokens));
            }
        }
        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| format!("cannot load {}; is the download complete?", model.label))?;
        let recognizer = Arc::new(recognizer);
        *loaded = Some((model.id, recognizer.clone()));
        Ok(recognizer)
    }
}

fn extract(models_dir: &Path, model: &ModelSpec, archive_path: &Path) -> Result<(), String> {
    let temp = models_dir
        .join(".downloads")
        .join(format!("{}-extract", model.id));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).map_err(|e| format!("cannot make {}: {e}", temp.display()))?;

    let file = std::fs::File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {e}", archive_path.display()))?;
    let decoder = bzip2::read::BzDecoder::new(std::io::BufReader::new(file));
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(&temp)
        .map_err(|e| format!("cannot unpack {}: {e}", model.label))?;

    let extracted = temp.join(model.dir);
    let source = if extracted.is_dir() {
        &extracted
    } else {
        &temp
    };
    let missing: Vec<_> = model
        .engine
        .files()
        .into_iter()
        .filter(|f| !source.join(f).is_file())
        .collect();
    if !missing.is_empty() {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(format!(
            "{} is missing {} after unpacking",
            model.label,
            missing.join(", ")
        ));
    }

    let destination = models_dir.join(model.dir);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)
            .map_err(|e| format!("cannot replace {}: {e}", destination.display()))?;
    }
    std::fs::rename(source, &destination)
        .map_err(|e| format!("cannot move {} into place: {e}", model.label))?;
    let _ = std::fs::remove_dir_all(&temp);
    Ok(())
}

/// Reads a PCM WAV file as mono f32 samples and its sample rate. The app
/// records 16-bit mono at 16 kHz; other common PCM shapes are accepted too.
fn wav_to_f32(bytes: &[u8]) -> Result<(i32, Vec<f32>), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("the voice message is not a WAV file".into());
    }
    let mut format = None;
    let mut data = None;
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let start = pos + 8;
        let end = start.saturating_add(len).min(bytes.len());
        match id {
            b"fmt " if end.saturating_sub(start) >= 16 => {
                let f = &bytes[start..end];
                format = Some((
                    u16::from_le_bytes(f[0..2].try_into().unwrap()),
                    u16::from_le_bytes(f[2..4].try_into().unwrap()) as usize,
                    u32::from_le_bytes(f[4..8].try_into().unwrap()) as i32,
                    u16::from_le_bytes(f[14..16].try_into().unwrap()),
                ));
            }
            b"data" => data = Some(&bytes[start..end]),
            _ => {}
        }
        pos = start.saturating_add(len).saturating_add(len & 1);
    }
    let (audio_format, channels, sample_rate, bits) =
        format.ok_or("the voice message has no WAV format")?;
    let data = data.ok_or("the voice message has no audio in its WAV file")?;
    if channels == 0 {
        return Err("the voice message has no audio channels".into());
    }
    let mut samples = match (audio_format, bits) {
        (1, 16) => data
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect::<Vec<_>>(),
        (1, 8) => data.iter().map(|b| (*b as f32 - 128.0) / 128.0).collect(),
        (3, 32) => data
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        _ => {
            return Err(format!(
                "the voice message is a WAV file with unsupported format {audio_format}/{bits}"
            ))
        }
    };
    if channels > 1 {
        samples = samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect();
    }
    Ok((sample_rate, samples))
}

/// 99 languages multilingual Whisper models transcribe.
const WHISPER_LANGUAGES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jv", "su",
];

/// Dolphin's 40 Eastern languages; Chinese dialects are under "zh".
const DOLPHIN_LANGUAGES: &[&str] = &[
    "zh", "ja", "th", "ru", "ko", "id", "vi", "yue", "hi", "ur", "ms", "uz", "ar", "fa", "bn",
    "ta", "te", "ug", "gu", "my", "tl", "kk", "or", "ne", "mn", "km", "jv", "lo", "si", "fil",
    "ps", "pa", "kab", "ba", "ks", "tg", "su", "mr", "ky", "az",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_entry_is_complete() {
        let mut ids = std::collections::HashSet::new();
        for model in LOCAL_STT_MODELS {
            assert!(ids.insert(model.id), "duplicate model id {}", model.id);
            assert!(model.size > 0);
            assert!(model.archive_url.ends_with(".tar.bz2"));
            assert!(!model.engine.files().is_empty());
        }
        assert!(find_model(DEFAULT_LOCAL_STT_MODEL).is_some());
    }

    #[test]
    fn reads_pcm16_wav_and_mixes_channels() {
        let mut wav = Vec::new();
        let samples: [i16; 4] = [0, 16384, -16384, 32767];
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&0u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&64_000u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        let (rate, decoded) = wav_to_f32(&wav).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(decoded.len(), 2);
        assert!((decoded[0] - 0.25).abs() < 0.001);
        assert!((decoded[1] - 0.25).abs() < 0.001);
    }

    #[test]
    fn rejects_other_files() {
        assert!(wav_to_f32(b"not a wav").is_err());
    }

    /// Downloads a model and runs it on the release's own test audio. Ignored
    /// because it needs the network and about 110 MB of disk.
    #[tokio::test]
    #[ignore = "downloads a speech model from GitHub"]
    async fn downloads_and_transcribes_a_model() {
        let dir =
            std::env::temp_dir().join(format!("grassline-speech-test-{}", std::process::id()));
        let speech = LocalSpeech::new(&dir);
        let id = "parakeet-tdt-110m-en-int8";
        speech.start_download(id).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        loop {
            if speech.installed(find_model(id).unwrap()) {
                break;
            }
            if let Some(status) = speech.download_status(id) {
                if let Some(error) = status.error {
                    panic!("download failed: {error}");
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "download did not finish"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let model = find_model(id).unwrap();
        // Loading the model catches a wrong engine config even when the
        // release has no test audio.
        speech
            .recognizer(model)
            .unwrap_or_else(|e| panic!("cannot load {id}: {e}"));
        let test_wavs = speech.models_dir().join(model.dir).join("test_wavs");
        let wav = std::fs::read_dir(&test_wavs).ok().and_then(|dir| {
            dir.flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == "wav"))
        });
        let Some(wav) = wav.and_then(|p| std::fs::read(p).ok()) else {
            let _ = std::fs::remove_dir_all(dir);
            return;
        };
        let speech = speech.clone();
        let text = tokio::task::spawn_blocking(move || speech.transcribe(id, &wav))
            .await
            .unwrap()
            .unwrap();
        assert!(!text.trim().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
