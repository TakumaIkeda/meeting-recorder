//! whisper.cpp settings, stored as editable JSON next to the meetings folder.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WhisperConfig {
    /// Path to `whisper-cli(.exe)`, or a bare name to look it up on PATH.
    pub whisper_cli: PathBuf,
    /// Path to a ggml model, e.g. `ggml-large-v3-turbo.bin`.
    pub model: PathBuf,
    pub language: String,
    /// CPU threads; 0 = number of logical CPUs.
    pub threads: usize,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            whisper_cli: PathBuf::from(if cfg!(windows) {
                "whisper-cli.exe"
            } else {
                "whisper-cli"
            }),
            model: PathBuf::from("ggml-large-v3-turbo.bin"),
            language: "ja".into(),
            threads: 0,
        }
    }
}

impl WhisperConfig {
    /// Load `path`, creating it with defaults (model under `default_model_dir`) if missing.
    pub fn load_or_create(path: &Path, default_model_dir: &Path) -> Result<Self> {
        if path.exists() {
            let text = fs::read_to_string(path)
                .with_context(|| format!("{} を読み込めません", path.display()))?;
            return serde_json::from_str(&text)
                .with_context(|| format!("{} の形式が正しくありません", path.display()));
        }
        let config = Self {
            model: default_model_dir.join("ggml-large-v3-turbo.bin"),
            ..Self::default()
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(&config)?)
            .with_context(|| format!("{} を作成できません", path.display()))?;
        Ok(config)
    }

    pub fn threads(&self) -> usize {
        if self.threads > 0 {
            self.threads
        } else {
            std::thread::available_parallelism().map_or(4, |n| n.get())
        }
    }

    /// Fail early with an actionable message instead of a cryptic whisper-cli error.
    pub fn validate(&self) -> Result<()> {
        if !self.model.is_file() {
            bail!(
                "whisper のモデルが見つかりません: {}\n設定ファイルの \"model\" に ggml モデルのパスを指定してください。",
                self.model.display()
            );
        }
        Ok(())
    }
}
