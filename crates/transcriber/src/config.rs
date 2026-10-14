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
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            whisper_cli: None,
            model: None,
            language: "ja".into(),
            threads: 0,
        }
    }
}

/// Where the app keeps things that `null` settings fall back to.
#[derive(Debug, Clone)]
pub struct Environment {
    /// whisper-cli shipped with the app, if any.
    pub bundled_cli: Option<PathBuf>,
    /// Folder that downloaded models are saved to.
    pub models_dir: PathBuf,
}

/// Fully resolved settings for one transcription run.
#[derive(Debug, Clone)]
pub struct Whisper {
    pub cli: PathBuf,
    pub model: PathBuf,
    pub language: String,
    pub threads: usize,
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

    /// The whisper-cli to run: an explicit path, else the bundled one, else a PATH lookup.
    pub fn cli_path(&self, env: &Environment) -> PathBuf {
        if let Some(p) = &self.whisper_cli {
            // A bare file name (e.g. an older default "whisper-cli.exe") means "find it for me".
            if p.components().count() > 1 {
                return p.clone();
            }
        }
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
        Ok(Whisper {
            cli: self.cli_path(env),
            model,
            language: self.language.clone(),
            threads: self.threads(),
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
