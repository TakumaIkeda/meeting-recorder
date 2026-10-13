//! Tauri shell: exposes the GUI-independent `recorder` crate as commands and streams
//! elapsed time / input levels to the frontend as events.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use recorder::{DefaultDevices, DeviceInfo, Recorder};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};

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
struct AppState(Arc<Mutex<Option<Active>>>);

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
struct StoppedRecording {
    dir: PathBuf,
    duration_secs: f64,
}

/// `<Documents>/MeetingRecorder/meetings`
fn output_root(app: &AppHandle) -> Result<PathBuf, String> {
    let docs = app
        .path()
        .document_dir()
        .map_err(|e| format!("ドキュメントフォルダを取得できません: {e}"))?;
    Ok(docs.join("MeetingRecorder").join("meetings"))
}

/// Run blocking audio work off the main thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_devices() -> Result<DefaultDevices, String> {
    blocking(|| Ok(recorder::default_devices())).await
}

#[tauri::command]
async fn start_recording(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StartedRecording, String> {
    let state = state.inner().clone();
    let root = output_root(&app)?;
    blocking(move || {
        let mut slot = state.0.lock().unwrap();
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

#[tauri::command]
async fn stop_recording(state: State<'_, AppState>) -> Result<StoppedRecording, String> {
    let state = state.inner().clone();
    blocking(move || {
        let active = state.0.lock().unwrap().take().ok_or("録音していません")?;
        let summary = active.stop().map_err(|e| format!("{e:#}"))?;
        Ok(StoppedRecording {
            dir: summary.dir,
            duration_secs: summary.duration_secs,
        })
    })
    .await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_devices,
            start_recording,
            stop_recording
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Finalize the WAV files if the window is closed while recording.
            if let RunEvent::Exit = event {
                let state = app.state::<AppState>();
                let active = state.0.lock().unwrap().take();
                if let Some(active) = active {
                    if let Err(e) = active.stop() {
                        eprintln!("録音の終了処理に失敗しました: {e:#}");
                    }
                }
            }
        });
}
