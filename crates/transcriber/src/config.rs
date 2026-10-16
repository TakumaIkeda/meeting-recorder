//! whisper.cpp settings (`config.json`) and how they resolve to concrete paths.
//!
//! Every field is optional: by default the whisper-cli bundled with the app and a model
//! downloaded into the models folder are used. Setting a path overrides that.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::models::MODELS;

/// Contents of `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WhisperConfig {
    /// Path to `whisper-cli(.exe)`. `null` (or a bare file name) uses the bundled one, falling
    /// back to PATH.
    pub whisper_cli: Option<PathBuf>,
    /// Path to a ggml model. `null` uses a model downloaded into the models folder.
    pub model: Option<PathBuf>,
    pub language: String,
    /// CPU threads; 0 = number of logical CPUs.
    pub threads: usize,
    /// Use the GPU (CUDA) build when it has been downloaded. `false` forces the CPU build.
    pub use_gpu: bool,
    /// Voice activity detection: only transcribe stretches with speech. Prevents whisper from
    /// hallucinating (「ご視聴ありがとうございました」, repeated phrases) during silence.
    pub vad: bool,
    /// Tokens of previous text fed back as context (whisper-cli `-mc`). 0 stops repetition loops
    /// from carrying over between segments; -1 is whisper's default (full context).
    pub max_context: i32,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            whisper_cli: None,
            model: None,
            language: "ja".into(),
            threads: 0,
            use_gpu: true,
            vad: true,
            max_context: 0,
        }
    }
}

/// Where the app keeps things that `null` settings fall back to.
#[derive(Debug, Clone)]
pub struct Environment {
    /// whisper-cli shipped with the app, if any.
    pub bundled_cli: Option<PathBuf>,
    /// whisper-cli of the downloaded GPU (CUDA) build, if installed.
    pub gpu_cli: Option<PathBuf>,
    /// Silero VAD model shipped with the app, if any.
    pub vad_model: Option<PathBuf>,
    /// Folder that downloaded models are saved to.
    pub models_dir: PathBuf,
}

/// Fully resolved settings for one transcription run.
#[derive(Debug, Clone)]
pub struct Whisper {
    pub cli: PathBuf,
    /// Tried if `cli` fails (the CPU build when `cli` is the GPU build).
    pub fallback_cli: Option<PathBuf>,
    pub model: PathBuf,
    pub language: String,
    pub threads: usize,
    /// VAD model to pass with `--vad`; `None` disables VAD.
    pub vad_model: Option<PathBuf>,
    pub max_context: i32,
}

fn cli_name() -> &'static str {
    if cfg!(windows) {
        "whisper-cli.exe"
    } else {
        "whisper-cli"
    }
}

impl WhisperConfig {
    /// Load `path`, creating it with defaults if missing.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let text = fs::read_to_string(path)
                .with_context(|| format!("{} を読み込めません", path.display()))?;
            return serde_json::from_str(&text)
                .with_context(|| format!("{} の形式が正しくありません", path.display()));
        }
        let config = Self::default();
        config.save(path)?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("{} を書き込めません", path.display()))
    }

    /// The whisper-cli to run: an explicit path, else the GPU build (if downloaded and enabled),
    /// else the bundled CPU build, else a PATH lookup.
    pub fn cli_path(&self, env: &Environment) -> PathBuf {
        if let Some(p) = self.explicit_cli() {
            return p.to_path_buf();
        }
        if let Some(gpu) = env.gpu_cli.as_ref().filter(|p| self.use_gpu && p.is_file()) {
            return gpu.clone();
        }
        self.cpu_cli_path(env)
    }

    /// A bare file name (e.g. an older default "whisper-cli.exe") means "find it for me".
    fn explicit_cli(&self) -> Option<&Path> {
        self.whisper_cli
            .as_deref()
            .filter(|p| p.components().count() > 1)
    }

    fn cpu_cli_path(&self, env: &Environment) -> PathBuf {
        match &env.bundled_cli {
            Some(bundled) if bundled.is_file() => bundled.clone(),
            _ => self
                .whisper_cli
                .clone()
                .unwrap_or_else(|| cli_name().into()),
        }
    }

    /// The model to use: an explicit path (even if missing), else the first known model that
    /// exists in the models folder.
    pub fn model_path(&self, env: &Environment) -> Option<PathBuf> {
        if let Some(p) = &self.model {
            return Some(p.clone());
        }
        MODELS
            .iter()
            .map(|m| env.models_dir.join(m.file))
            .find(|p| p.is_file())
    }

    pub fn threads(&self) -> usize {
        if self.threads > 0 {
            self.threads
        } else {
            std::thread::available_parallelism().map_or(4, |n| n.get())
        }
    }

    /// Resolve to concrete paths, failing with an actionable message if there is no model.
    pub fn resolve(&self, env: &Environment) -> Result<Whisper> {
        let Some(model) = self.model_path(env).filter(|p| p.is_file()) else {
            match &self.model {
                Some(p) => bail!(
                    "whisper のモデルが見つかりません: {}\nconfig.json の \"model\" を直すか、null にして画面からモデルをダウンロードしてください。",
                    p.display()
                ),
                None => bail!("文字起こしモデルがありません。画面の「モデルをダウンロード」から取得してください。"),
            }
        };
        let cli = self.cli_path(env);
        let cpu = self.cpu_cli_path(env);
        let fallback_cli = (self.explicit_cli().is_none() && cli != cpu).then_some(cpu);
        Ok(Whisper {
            cli,
            fallback_cli,
            model,
            language: self.language.clone(),
            threads: self.threads(),
            vad_model: env.vad_model.clone().filter(|p| self.vad && p.is_file()),
            max_context: self.max_context,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(dir: &Path, bundled: bool) -> Environment {
        let cli = dir.join("bundled").join(cli_name());
        if bundled {
            fs::create_dir_all(cli.parent().unwrap()).unwrap();
            fs::write(&cli, b"").unwrap();
        }
        Environment {
            bundled_cli: Some(cli),
            gpu_cli: None,
            vad_model: None,
            models_dir: dir.join("models"),
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("whisper-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn cli_resolution_order() {
        let d = tmp("cli");
        let with_bundle = env(&d, true);
        let bundled = with_bundle.bundled_cli.clone().unwrap();
        let mut c = WhisperConfig::default();
        assert_eq!(c.cli_path(&with_bundle), bundled);
        // Older configs stored a bare name as the default; it must not shadow the bundle.
        c.whisper_cli = Some(cli_name().into());
        assert_eq!(c.cli_path(&with_bundle), bundled);
        c.whisper_cli = Some(d.join("custom").join(cli_name()));
        assert_eq!(c.cli_path(&with_bundle), d.join("custom").join(cli_name()));

        let d2 = tmp("cli-nobundle");
        let c = WhisperConfig::default();
        assert_eq!(c.cli_path(&env(&d2, false)), PathBuf::from(cli_name()));
        fs::remove_dir_all(&d).unwrap();
        fs::remove_dir_all(&d2).unwrap();
    }

    #[test]
    fn gpu_build_is_preferred_with_cpu_fallback() {
        let d = tmp("gpu");
        let mut e = env(&d, true);
        let bundled = e.bundled_cli.clone().unwrap();
        fs::create_dir_all(&e.models_dir).unwrap();
        fs::write(e.models_dir.join(MODELS[0].file), b"x").unwrap();
        let gpu = d.join("gpu").join(cli_name());
        fs::create_dir_all(gpu.parent().unwrap()).unwrap();
        fs::write(&gpu, b"").unwrap();
        e.gpu_cli = Some(gpu.clone());

        let w = WhisperConfig::default().resolve(&e).unwrap();
        assert_eq!(
            (w.cli, w.fallback_cli),
            (gpu.clone(), Some(bundled.clone()))
        );

        let cpu_only = WhisperConfig {
            use_gpu: false,
            ..WhisperConfig::default()
        };
        let w = cpu_only.resolve(&e).unwrap();
        assert_eq!((w.cli, w.fallback_cli), (bundled, None));

        // An explicit whisper-cli always wins and has no fallback.
        let custom = d.join("custom").join(cli_name());
        let explicit = WhisperConfig {
            whisper_cli: Some(custom.clone()),
            ..WhisperConfig::default()
        };
        let w = explicit.resolve(&e).unwrap();
        assert_eq!((w.cli, w.fallback_cli), (custom, None));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn model_resolution_and_errors() {
        let d = tmp("model");
        let e = env(&d, true);
        let c = WhisperConfig::default();
        let err = c.resolve(&e).unwrap_err().to_string();
        assert!(err.contains("モデルをダウンロード"), "{err}");

        fs::create_dir_all(&e.models_dir).unwrap();
        let small = e.models_dir.join("ggml-small.bin");
        fs::write(&small, b"x").unwrap();
        assert_eq!(c.resolve(&e).unwrap().model, small);
        // The recommended model wins when several are present.
        let turbo = e.models_dir.join(MODELS[0].file);
        fs::write(&turbo, b"x").unwrap();
        assert_eq!(c.resolve(&e).unwrap().model, turbo);

        let explicit = WhisperConfig {
            model: Some(d.join("missing.bin")),
            ..WhisperConfig::default()
        };
        assert!(explicit
            .resolve(&e)
            .unwrap_err()
            .to_string()
            .contains("missing.bin"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn reads_old_config_format() {
        let json = r#"{"whisper_cli": "C:\\tools\\whisper-cli.exe", "model": "D:\\m\\ggml.bin", "language": "ja", "threads": 4}"#;
        let c: WhisperConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.threads, 4);
        assert!(c.model.is_some());
    }
}
