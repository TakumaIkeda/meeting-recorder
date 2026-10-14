//! First-run setup: is whisper-cli available, is there a model, and downloading one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use transcriber::models::{self, ModelSpec, MODELS};
use transcriber::WhisperConfig;

use crate::Paths;

pub const DOWNLOAD_EVENT: &str = "model-download";

#[derive(Serialize)]
pub struct SetupStatus {
    cli_path: PathBuf,
    cli_bundled: bool,
    cli_found: bool,
    model_path: Option<PathBuf>,
    model_found: bool,
    models: &'static [ModelSpec],
    config_path: PathBuf,
    models_dir: PathBuf,
    download: Option<DownloadState>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DownloadPhase {
    Running,
    Done,
    Cancelled,
    Failed { error: String },
}

#[derive(Clone, Serialize)]
pub struct DownloadState {
    id: String,
    downloaded: u64,
    total: Option<u64>,
    #[serde(flatten)]
    phase: DownloadPhase,
}

#[derive(Default)]
pub struct Downloads {
    current: Arc<Mutex<Option<DownloadState>>>,
    cancel: Arc<AtomicBool>,
}

/// Whether `cli` can be run: an existing file, or a bare name found on PATH.
fn cli_exists(cli: &Path) -> bool {
    if cli.components().count() > 1 {
        return cli.is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(cli).is_file()))
        .unwrap_or(false)
}

#[tauri::command]
pub fn get_setup(
    paths: State<'_, Paths>,
    downloads: State<'_, Downloads>,
) -> Result<SetupStatus, String> {
    let config = WhisperConfig::load_or_create(&paths.config).map_err(|e| format!("{e:#}"))?;
    let env = paths.environment();
    let cli_path = config.cli_path(&env);
    let model_path = config.model_path(&env);
    Ok(SetupStatus {
        cli_bundled: env.bundled_cli.as_deref() == Some(cli_path.as_path()),
        cli_found: cli_exists(&cli_path),
        cli_path,
        model_found: model_path.as_deref().is_some_and(Path::is_file),
        model_path,
        models: MODELS,
        config_path: paths.config.clone(),
        models_dir: paths.models.clone(),
        download: downloads.current.lock().unwrap().clone(),
    })
}

#[tauri::command]
pub fn start_model_download(
    app: AppHandle,
    id: String,
    paths: State<'_, Paths>,
    downloads: State<'_, Downloads>,
) -> Result<(), String> {
    let spec = models::find(&id).ok_or_else(|| format!("不明なモデルです: {id}"))?;
    {
        let mut current = downloads.current.lock().unwrap();
        if matches!(
            current.as_ref().map(|d| &d.phase),
            Some(DownloadPhase::Running)
        ) {
            return Err("すでにダウンロード中です".into());
        }
        *current = Some(DownloadState {
            id: id.clone(),
            downloaded: 0,
            total: None,
            phase: DownloadPhase::Running,
        });
    }
    downloads.cancel.store(false, Ordering::Relaxed);
    let current = downloads.current.clone();
    let cancel = downloads.cancel.clone();
    let models_dir = paths.models.clone();
    let config_path = paths.config.clone();

    let update = move |app: &AppHandle, f: &dyn Fn(&mut DownloadState)| {
        let state = {
            let mut cur = current.lock().unwrap();
            let Some(s) = cur.as_mut() else { return };
            f(s);
            s.clone()
        };
        let _ = app.emit(DOWNLOAD_EVENT, &state);
    };
    update(&app, &|_| {});

    thread::Builder::new()
        .name("model-download".into())
        .spawn(move || {
            let mut last_emit = Instant::now() - Duration::from_secs(1);
            let result = models::download(
                spec,
                &models_dir,
                &mut |done, total| {
                    if last_emit.elapsed() >= Duration::from_millis(200) {
                        last_emit = Instant::now();
                        update(&app, &|s| {
                            s.downloaded = done;
                            s.total = total;
                        });
                    }
                },
                &cancel,
            )
            .and_then(|path| {
                // A config pointing at a model that doesn't exist (e.g. copied from another PC)
                // would shadow the download; switch it back to "use the models folder".
                if path.is_some() {
                    let mut config = WhisperConfig::load_or_create(&config_path)?;
                    if config.model.as_deref().is_some_and(|m| !m.is_file()) {
                        config.model = None;
                        config.save(&config_path)?;
                    }
                }
                Ok(path)
            });
            let phase = match result {
                Ok(Some(_)) => DownloadPhase::Done,
                Ok(None) => DownloadPhase::Cancelled,
                Err(e) => DownloadPhase::Failed {
                    error: format!("{e:#}"),
                },
            };
            update(&app, &|s| {
                s.phase = phase.clone();
                if matches!(s.phase, DownloadPhase::Done) {
                    s.total = Some(s.total.unwrap_or(s.downloaded));
                    s.downloaded = s.total.unwrap_or(s.downloaded);
                }
            });
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn cancel_model_download(downloads: State<'_, Downloads>) {
    downloads.cancel.store(true, Ordering::Relaxed);
}
