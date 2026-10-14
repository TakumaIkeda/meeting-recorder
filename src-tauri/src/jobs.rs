//! Transcription queue: one whisper-cli run at a time, status pushed to the UI as events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use transcriber::{Environment, WhisperConfig};

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
    /// Increases with every status change. Command results and events can reach the UI out of
    /// order (the worker may already be running when `stop_recording` returns), so the UI keeps
    /// whichever copy has the highest `seq`.
    pub seq: u64,
}

#[derive(Clone)]
pub struct Jobs {
    meetings: Arc<Mutex<HashMap<PathBuf, MeetingInfo>>>,
    queue: Sender<PathBuf>,
    seq: Arc<AtomicU64>,
}

impl Jobs {
    /// Start the worker thread. `config_path` is re-read for every job so edits apply on retry.
    pub fn start(app: AppHandle, config_path: PathBuf, env: Environment) -> Self {
        let (queue, rx) = mpsc::channel::<PathBuf>();
        let meetings: Arc<Mutex<HashMap<PathBuf, MeetingInfo>>> = Arc::default();
        let jobs = Self {
            meetings,
            queue,
            seq: Arc::default(),
        };
        let worker = jobs.clone();
        thread::Builder::new()
            .name("transcriber".into())
            .spawn(move || {
                for dir in rx {
                    worker.set_status(&app, &dir, JobStatus::Transcribing { progress: 0 });
                    let run = || {
                        WhisperConfig::load_or_create(&config_path)
                            .and_then(|config| config.resolve(&env))
                            .and_then(|config| {
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
                            })
                    };
                    // A panic must not kill the worker, or every later job would stay queued.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                        .unwrap_or_else(|_| {
                            Err(anyhow::anyhow!("文字起こし処理が異常終了しました"))
                        });
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
                seq: self.next_seq(),
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
            m.seq = self.next_seq();
            m.clone()
        };
        let _ = app.emit(STATUS_EVENT, &info);
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
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
