//! First-run setup: whisper-cli and model availability, model / GPU build downloads.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use transcriber::gpu::{self, GpuPack, CUDA_PACK};
use transcriber::models::{self, ModelSpec, MODELS};
use transcriber::WhisperConfig;

use crate::Paths;

pub const DOWNLOAD_EVENT: &str = "model-download";
/// Download id used for the GPU build (models use their own ids).
const GPU_DOWNLOAD_ID: &str = "gpu-cuda";

/// Which whisper-cli transcription will use.
#[derive(Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Engine {
    /// The downloaded CUDA build.
    Gpu,
    /// The CPU build bundled with the app.
    Cpu,
    /// A path set in config.json, or found on PATH.
    Custom,
}

#[derive(Serialize)]
pub struct SetupStatus {
    cli_path: PathBuf,
    cli_found: bool,
    engine: Engine,
    model_path: Option<PathBuf>,
    model_found: bool,
    models: &'static [ModelSpec],
    config_path: PathBuf,
    models_dir: PathBuf,
    /// An NVIDIA driver is installed, so the GPU build can be used.
    nvidia: bool,
    gpu_installed: bool,
    use_gpu: bool,
    gpu_pack: GpuPack,
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

fn load_config(paths: &Paths) -> Result<WhisperConfig, String> {
    WhisperConfig::load_or_create(&paths.config).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn get_setup(
    paths: State<'_, Paths>,
    downloads: State<'_, Downloads>,
) -> Result<SetupStatus, String> {
    let config = load_config(&paths)?;
    let env = paths.environment();
    let cli_path = config.cli_path(&env);
    let model_path = config.model_path(&env);
    let engine = if env.gpu_cli.as_deref() == Some(cli_path.as_path()) {
        Engine::Gpu
    } else if env.bundled_cli.as_deref() == Some(cli_path.as_path()) {
        Engine::Cpu
    } else {
        Engine::Custom
    };
    Ok(SetupStatus {
        cli_found: cli_exists(&cli_path),
        cli_path,
        engine,
        model_found: model_path.as_deref().is_some_and(Path::is_file),
        model_path,
        models: MODELS,
        config_path: paths.config.clone(),
        models_dir: paths.models.clone(),
        nvidia: gpu::nvidia_driver_present(),
        gpu_installed: env.gpu_cli.is_some(),
        use_gpu: config.use_gpu,
        gpu_pack: CUDA_PACK,
        download: downloads.current.lock().unwrap().clone(),
    })
}

type Job =
    Box<dyn FnOnce(&mut dyn FnMut(u64, Option<u64>), &AtomicBool) -> anyhow::Result<bool> + Send>;

/// Run one download at a time in the background, reporting progress as `model-download` events.
/// `job` returns `Ok(false)` when cancelled.
fn run_download(app: AppHandle, downloads: &Downloads, id: &str, job: Job) -> Result<(), String> {
    {
        let mut current = downloads.current.lock().unwrap();
        if matches!(
            current.as_ref().map(|d| &d.phase),
            Some(DownloadPhase::Running)
        ) {
            return Err("すでにダウンロード中です".into());
        }
        *current = Some(DownloadState {
            id: id.to_string(),
            downloaded: 0,
            total: None,
            phase: DownloadPhase::Running,
        });
    }
    downloads.cancel.store(false, Ordering::Relaxed);
    let current = downloads.current.clone();
    let cancel = downloads.cancel.clone();

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
        .name("download".into())
        .spawn(move || {
            let mut last_emit = Instant::now() - Duration::from_secs(1);
            let result = job(
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
            );
            let phase = match result {
                Ok(true) => DownloadPhase::Done,
                Ok(false) => DownloadPhase::Cancelled,
                Err(e) => DownloadPhase::Failed {
                    error: format!("{e:#}"),
                },
            };
            update(&app, &|s| {
                s.phase = phase.clone();
                if matches!(s.phase, DownloadPhase::Done) {
                    s.downloaded = s.total.unwrap_or(s.downloaded);
                }
            });
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn start_model_download(
    app: AppHandle,
    id: String,
    paths: State<'_, Paths>,
    downloads: State<'_, Downloads>,
) -> Result<(), String> {
    let spec = models::find(&id).ok_or_else(|| format!("不明なモデルです: {id}"))?;
    let models_dir = paths.models.clone();
    let config_path = paths.config.clone();
    run_download(
        app,
        &downloads,
        &id,
        Box::new(move |progress, cancel| {
            let Some(_) = models::download(spec, &models_dir, progress, cancel)? else {
                return Ok(false);
            };
            // A config pointing at a model that doesn't exist (e.g. copied from another PC)
            // would shadow the download; switch it back to "use the models folder".
            let mut config = WhisperConfig::load_or_create(&config_path)?;
            if config.model.as_deref().is_some_and(|m| !m.is_file()) {
                config.model = None;
                config.save(&config_path)?;
            }
            Ok(true)
        }),
    )
}

#[tauri::command]
pub fn start_gpu_download(
    app: AppHandle,
    paths: State<'_, Paths>,
    downloads: State<'_, Downloads>,
) -> Result<(), String> {
    let runtime = paths.runtime.clone();
    let vc_runtime = paths
        .bundled_cli
        .as_ref()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    run_download(
        app,
        &downloads,
        GPU_DOWNLOAD_ID,
        Box::new(move |progress, cancel| {
            Ok(CUDA_PACK
                .install(&runtime, vc_runtime.as_deref(), progress, cancel)?
                .is_some())
        }),
    )
}

#[tauri::command]
pub fn cancel_model_download(downloads: State<'_, Downloads>) {
    downloads.cancel.store(true, Ordering::Relaxed);
}

/// Turn use of the downloaded GPU build on or off (`use_gpu` in config.json).
#[tauri::command]
pub fn set_use_gpu(enabled: bool, paths: State<'_, Paths>) -> Result<(), String> {
    let mut config = load_config(&paths)?;
    config.use_gpu = enabled;
    config.save(&paths.config).map_err(|e| format!("{e:#}"))
}
