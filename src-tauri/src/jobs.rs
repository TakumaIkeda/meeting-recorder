//! Transcription queue: one whisper-cli run at a time, status pushed to the UI as events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use transcriber::WhisperConfig;

pub const STATUS_EVENT: &str = "transcription-status";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Transcribing { progress: u32 },
    Done,
    Failed { error: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct MeetingInfo {
    pub dir: PathBuf,
    pub started_at: String,
    pub duration_secs: Option<f64>,
    pub status: JobStatus,
}

#[derive(Clone)]
pub struct Jobs {
    meetings: Arc<Mutex<HashMap<PathBuf, MeetingInfo>>>,
    queue: Sender<PathBuf>,
}

impl Jobs {
    /// Start the worker thread. `config_path` is re-read for every job so edits apply on retry.
    pub fn start(app: AppHandle, config_path: PathBuf, model_dir: PathBuf) -> Self {
        let (queue, rx) = mpsc::channel::<PathBuf>();
        let meetings: Arc<Mutex<HashMap<PathBuf, MeetingInfo>>> = Arc::default();
        let jobs = Self { meetings, queue };
        let worker = jobs.clone();
        thread::Builder::new()
            .name("transcriber".into())
            .spawn(move || {
                for dir in rx {
                    worker.set_status(&app, &dir, JobStatus::Transcribing { progress: 0 });
                    let result = WhisperConfig::load_or_create(&config_path, &model_dir).and_then(
                        |config| {
                            let mut last = 0;
                            transcriber::transcribe_meeting(&dir, &config, &mut |p| {
                                if p != last {
                                    last = p;
                                    worker.set_status(
                                        &app,
                                        &dir,
                                        JobStatus::Transcribing { progress: p },
                                    );
                                }
                            })
                        },
                    );
                    let status = match result {
                        Ok(_) => JobStatus::Done,
                        Err(e) => JobStatus::Failed {
                            error: format!("{e:#}\n設定ファイル: {}", config_path.display()),
                        },
                    };
                    worker.set_status(&app, &dir, status);
                }
            })
            .expect("failed to spawn transcriber thread");
        jobs
    }

    /// Queue `dir` for transcription (no-op if it is already queued or running).
    pub fn enqueue(&self, app: &AppHandle, dir: &Path) -> Result<MeetingInfo, String> {
        let meta = recorder::read_meta(dir).map_err(|e| format!("{e:#}"))?;
        let info = {
            let mut meetings = self.meetings.lock().unwrap();
            if let Some(m) = meetings.get(dir) {
                if matches!(m.status, JobStatus::Queued | JobStatus::Transcribing { .. }) {
                    return Ok(m.clone());
                }
            }
            let info = MeetingInfo {
                dir: dir.to_path_buf(),
                started_at: meta.started_at.to_rfc3339(),
                duration_secs: meta.duration_secs,
                status: JobStatus::Queued,
            };
            meetings.insert(dir.to_path_buf(), info.clone());
            info
        };
        let _ = app.emit(STATUS_EVENT, &info);
        self.queue
            .send(dir.to_path_buf())
            .map_err(|e| e.to_string())?;
        Ok(info)
    }

    fn set_status(&self, app: &AppHandle, dir: &Path, status: JobStatus) {
        let info = {
            let mut meetings = self.meetings.lock().unwrap();
            let Some(m) = meetings.get_mut(dir) else {
                return;
            };
            m.status = status;
            m.clone()
        };
        let _ = app.emit(STATUS_EVENT, &info);
    }

    pub fn get(&self, dir: &Path) -> Option<MeetingInfo> {
        self.meetings.lock().unwrap().get(dir).cloned()
    }

    /// Meetings handled since the app started, newest first.
    pub fn list(&self) -> Vec<MeetingInfo> {
        let mut list: Vec<_> = self.meetings.lock().unwrap().values().cloned().collect();
        list.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        list
    }
}
