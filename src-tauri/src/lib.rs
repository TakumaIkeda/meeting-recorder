//! Tauri shell: exposes the GUI-independent `recorder` / `transcriber` crates as commands and
//! pushes recording progress and transcription status to the frontend as events.

mod jobs;
mod setup;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use jobs::{JobStatus, Jobs, MeetingInfo};
use recorder::{DefaultDevices, DeviceInfo, Recorder};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use transcriber::{Line, Transcript};

const PROGRESS_EVENT: &str = "recording-progress";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

struct Active {
    recorder: Recorder,
    stop_ticker: Arc<AtomicBool>,
    ticker: JoinHandle<()>,
}

impl Active {
    fn stop(self) -> anyhow::Result<recorder::RecordingSummary> {
        self.stop_ticker.store(true, Ordering::Relaxed);
        let _ = self.ticker.join();
        self.recorder.stop()
    }
}

#[derive(Default, Clone)]
struct Recording(Arc<Mutex<Option<Active>>>);

/// Folder layout under `<Documents>/MeetingRecorder`.
#[derive(Clone, Serialize)]
pub(crate) struct Paths {
    /// `meetings/` — one folder per recording.
    output_root: PathBuf,
    /// `config.json` — whisper-cli / model settings.
    pub(crate) config: PathBuf,
    /// `models/` — where models are downloaded to.
    pub(crate) models: PathBuf,
    /// whisper-cli bundled as a resource (`src-tauri/whisper/`, see scripts/fetch-whisper.mjs).
    pub(crate) bundled_cli: Option<PathBuf>,
}

impl Paths {
    fn resolve(app: &AppHandle) -> Result<Self, String> {
        let base = app
            .path()
            .document_dir()
            .map_err(|e| format!("ドキュメントフォルダを取得できません: {e}"))?
            .join("MeetingRecorder");
        let bundled_cli = app
            .path()
            .resource_dir()
            .ok()
            .map(|d| d.join("whisper").join("whisper-cli.exe"));
        Ok(Self {
            output_root: base.join("meetings"),
            config: base.join("config.json"),
            models: base.join("models"),
            bundled_cli,
        })
    }

    pub(crate) fn environment(&self) -> transcriber::Environment {
        transcriber::Environment {
            bundled_cli: self.bundled_cli.clone(),
            models_dir: self.models.clone(),
        }
    }

    /// Only allow commands to touch meeting folders under `output_root`.
    fn meeting_dir(&self, dir: &str) -> Result<PathBuf, String> {
        let dir = Path::new(dir)
            .canonicalize()
            .map_err(|e| format!("フォルダが見つかりません: {dir} ({e})"))?;
        let root = self.output_root.canonicalize().map_err(|e| e.to_string())?;
        if dir.parent() != Some(root.as_path()) {
            return Err("会議フォルダではありません".into());
        }
        Ok(dir)
    }
}

#[derive(Clone, Serialize)]
struct Progress {
    elapsed_ms: u64,
    mic_level: f32,
    system_level: f32,
    errors: Vec<String>,
}

#[derive(Serialize)]
struct StartedRecording {
    dir: PathBuf,
    mic: DeviceInfo,
    system: DeviceInfo,
}

#[derive(Serialize)]
struct MeetingDetail {
    info: MeetingInfo,
    lines: Vec<Line>,
}

/// Run blocking work off the main thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_paths(paths: State<'_, Paths>) -> Paths {
    paths.inner().clone()
}

#[tauri::command]
async fn get_devices() -> Result<DefaultDevices, String> {
    blocking(|| Ok(recorder::default_devices())).await
}

#[tauri::command]
async fn start_recording(
    app: AppHandle,
    recording: State<'_, Recording>,
    paths: State<'_, Paths>,
) -> Result<StartedRecording, String> {
    let recording = recording.inner().clone();
    let root = paths.output_root.clone();
    blocking(move || {
        let mut slot = recording.0.lock().unwrap();
        if slot.is_some() {
            return Err("すでに録音中です".into());
        }
        let recorder = Recorder::start(&root).map_err(|e| format!("{e:#}"))?;

        let meters = recorder.meters();
        let stop_ticker = Arc::new(AtomicBool::new(false));
        let stop = stop_ticker.clone();
        let ticker = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(PROGRESS_INTERVAL);
                let _ = app.emit(
                    PROGRESS_EVENT,
                    Progress {
                        elapsed_ms: meters.elapsed().as_millis() as u64,
                        mic_level: meters.mic.take(),
                        system_level: meters.system.take(),
                        errors: meters.take_errors(),
                    },
                );
            }
        });

        let started = StartedRecording {
            dir: recorder.dir().to_path_buf(),
            mic: recorder.mic_device().clone(),
            system: recorder.system_device().clone(),
        };
        *slot = Some(Active {
            recorder,
            stop_ticker,
            ticker,
        });
        Ok(started)
    })
    .await
}

/// Stop recording and queue the folder for transcription.
#[tauri::command]
async fn stop_recording(
    app: AppHandle,
    recording: State<'_, Recording>,
    jobs: State<'_, Jobs>,
) -> Result<MeetingInfo, String> {
    let recording = recording.inner().clone();
    let jobs = jobs.inner().clone();
    blocking(move || {
        let active = recording
            .0
            .lock()
            .unwrap()
            .take()
            .ok_or("録音していません")?;
        let summary = active.stop().map_err(|e| format!("{e:#}"))?;
        jobs.enqueue(&app, &summary.dir)
    })
    .await
}

#[tauri::command]
fn retry_transcription(
    app: AppHandle,
    dir: String,
    jobs: State<'_, Jobs>,
    paths: State<'_, Paths>,
) -> Result<MeetingInfo, String> {
    let dir = paths.meeting_dir(&dir)?;
    jobs.enqueue(&app, &dir)
}

#[tauri::command]
fn list_meetings(jobs: State<'_, Jobs>) -> Vec<MeetingInfo> {
    jobs.list()
}

#[tauri::command]
async fn get_meeting(
    dir: String,
    jobs: State<'_, Jobs>,
    paths: State<'_, Paths>,
) -> Result<MeetingDetail, String> {
    let dir = paths.meeting_dir(&dir)?;
    let jobs = jobs.inner().clone();
    blocking(move || {
        let transcript = Transcript::load(&dir).map_err(|e| format!("{e:#}"))?;
        let info = match jobs.get(&dir) {
            Some(info) => info,
            None => {
                let meta = recorder::read_meta(&dir).map_err(|e| format!("{e:#}"))?;
                MeetingInfo {
                    dir: dir.clone(),
                    started_at: meta.started_at.to_rfc3339(),
                    duration_secs: meta.duration_secs,
                    status: JobStatus::Done,
                    seq: 0,
                }
            }
        };
        Ok(MeetingDetail {
            info,
            lines: transcript.lines,
        })
    })
    .await
}

#[tauri::command]
fn open_folder(dir: String, paths: State<'_, Paths>) -> Result<(), String> {
    let dir = paths.meeting_dir(&dir)?;
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(&dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("フォルダを開けません: {e}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Recording::default())
        .setup(|app| {
            let paths = Paths::resolve(app.handle())?;
            let jobs = Jobs::start(
                app.handle().clone(),
                paths.config.clone(),
                paths.environment(),
            );
            app.manage(paths);
            app.manage(jobs);
            app.manage(setup::Downloads::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_paths,
            get_devices,
            start_recording,
            stop_recording,
            retry_transcription,
            list_meetings,
            get_meeting,
            open_folder,
            setup::get_setup,
            setup::start_model_download,
            setup::cancel_model_download,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Finalize the WAV files if the window is closed while recording.
            if let RunEvent::Exit = event {
                let recording = app.state::<Recording>();
                let active = recording.0.lock().unwrap().take();
                if let Some(active) = active {
                    if let Err(e) = active.stop() {
                        eprintln!("録音の終了処理に失敗しました: {e:#}");
                    }
                }
            }
        });
}
