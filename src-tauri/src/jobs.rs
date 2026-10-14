//! Meetings and their transcription status.
//!
//! The meetings folder is the source of truth: a meeting is done when it has `transcript.json`,
//! failed when it has `transcription-error.txt`, otherwise not yet transcribed. On top of that,
//! a queue runs one whisper-cli at a time and pushes live status to the UI as events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use transcriber::{Environment, WhisperConfig};

use crate::Paths;

pub const STATUS_EVENT: &str = "transcription-status";
/// Last transcription error, kept so a failure survives an app restart.
const ERROR_FILE: &str = "transcription-error.txt";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum JobStatus {
    /// Recorded but never transcribed (e.g. recorded before a model was available).
    Pending,
    Queued,
    Transcribing {
        progress: u32,
    },
    Done,
    Failed {
        error: String,
    },
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
    /// Start time as a Unix timestamp, for sorting regardless of UTC offset.
    #[serde(skip)]
    started_ts: i64,
}

/// Describe a meeting folder from its files alone.
pub fn read_info(dir: &Path) -> Result<MeetingInfo, String> {
    let meta = recorder::read_meta(dir).map_err(|e| format!("{e:#}"))?;
    let status = if dir.join(transcriber::transcript::TRANSCRIPT_FILE).is_file() {
        JobStatus::Done
    } else if let Ok(error) = std::fs::read_to_string(dir.join(ERROR_FILE)) {
        JobStatus::Failed { error }
    } else {
        JobStatus::Pending
    };
    Ok(MeetingInfo {
        dir: dir.to_path_buf(),
        started_at: meta.started_at.to_rfc3339(),
        // Missing if the app was killed while recording; the WAV header still has the length.
        duration_secs: meta
            .duration_secs
            .or_else(|| recorder::wav_duration_secs(&dir.join("mic.wav"))),
        status,
        seq: 0,
        started_ts: meta.started_at.timestamp(),
    })
}

#[derive(Clone)]
pub struct Jobs {
    meetings: Arc<Mutex<HashMap<PathBuf, MeetingInfo>>>,
    queue: Sender<PathBuf>,
    seq: Arc<AtomicU64>,
}

impl Jobs {
    /// Start the worker thread. Settings and installed runtimes are re-read for every job, so
    /// config edits or a GPU build downloaded meanwhile apply on the next run or retry.
    pub fn start(app: AppHandle, paths: Paths) -> Self {
        let config_path = paths.config.clone();
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
                        let env: Environment = paths.environment();
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
                        Ok(_) => {
                            let _ = std::fs::remove_file(dir.join(ERROR_FILE));
                            JobStatus::Done
                        }
                        Err(e) => {
                            let error = format!("{e:#}\n設定ファイル: {}", config_path.display());
                            let _ = std::fs::write(dir.join(ERROR_FILE), &error);
                            JobStatus::Failed { error }
                        }
                    };
                    worker.set_status(&app, &dir, status);
                }
            })
            .expect("failed to spawn transcriber thread");
        jobs
    }

    /// Queue `dir` for transcription (no-op if it is already queued or running).
    pub fn enqueue(&self, app: &AppHandle, dir: &Path) -> Result<MeetingInfo, String> {
        let from_disk = read_info(dir)?;
        let info = {
            let mut meetings = self.meetings.lock().unwrap();
            if let Some(m) = meetings.get(dir) {
                if matches!(m.status, JobStatus::Queued | JobStatus::Transcribing { .. }) {
                    return Ok(m.clone());
                }
            }
            let info = MeetingInfo {
                status: JobStatus::Queued,
                seq: self.next_seq(),
                ..from_disk
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

    /// Live status if the meeting went through the queue in this session, else from disk.
    pub fn get(&self, dir: &Path) -> Result<MeetingInfo, String> {
        match self.meetings.lock().unwrap().get(dir) {
            Some(m) => Ok(m.clone()),
            None => read_info(dir),
        }
    }

    /// Every meeting folder under `root`, newest first. `skip` is the folder being recorded.
    pub fn list(&self, root: &Path, skip: Option<&Path>) -> Vec<MeetingInfo> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        let live = self.meetings.lock().unwrap().clone();
        let mut list: Vec<MeetingInfo> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir() && Some(p.as_path()) != skip)
            .filter_map(|dir| live.get(&dir).cloned().or_else(|| read_info(&dir).ok()))
            .collect();
        list.sort_by(|a, b| {
            b.started_ts
                .cmp(&a.started_ts)
                .then_with(|| b.dir.cmp(&a.dir))
        });
        list
    }
}
