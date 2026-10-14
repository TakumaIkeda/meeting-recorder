//! GUI-independent transcription of a recorded meeting folder.
//!
//! Runs whisper.cpp (`whisper-cli`) separately on `mic.wav` (自分) and `system.wav` (相手),
//! then merges the segments into one timeline. Both files start at the same instant (see the
//! `recorder` crate), so whisper's per-file offsets are directly comparable.
//! The result is saved as `transcript.json` in the meeting folder.

pub mod config;
pub mod gpu;
pub mod models;
pub mod transcript;
pub mod whisper;

use std::path::Path;

use anyhow::{Context, Result};

pub use config::{Environment, Whisper, WhisperConfig};
pub use transcript::{Line, Speaker, Transcript};

/// Progress callback: overall percentage 0-100.
pub type Progress<'a> = &'a mut dyn FnMut(u32);

const TRACKS: [(&str, Speaker); 2] = [("mic", Speaker::Me), ("system", Speaker::Them)];

/// Transcribe `dir/mic.wav` and `dir/system.wav`, write `dir/transcript.json` and return it.
pub fn transcribe_meeting(dir: &Path, config: &Whisper, progress: Progress) -> Result<Transcript> {
    let mut tracks = Vec::new();
    for (i, (name, speaker)) in TRACKS.into_iter().enumerate() {
        let base = i as u32 * 100 / TRACKS.len() as u32;
        let mut on_track = |p: u32| progress(base + p / TRACKS.len() as u32);
        let segments = match whisper::transcribe_track(dir, name, config, &mut on_track) {
            Ok(s) => s,
            // e.g. the GPU build failing on a driver problem: retry this track on the CPU build.
            Err(e) if config.fallback_cli.is_some() => {
                let log = dir.join(format!("whisper-{name}.log"));
                let _ = std::fs::rename(&log, dir.join(format!("whisper-{name}-gpu-failed.log")));
                let cpu = Whisper {
                    cli: config.fallback_cli.clone().unwrap(),
                    fallback_cli: None,
                    ..config.clone()
                };
                whisper::transcribe_track(dir, name, &cpu, &mut on_track).with_context(|| {
                    format!("CPU 版での再実行も失敗しました（GPU 版のエラー: {e:#}）")
                })?
            }
            Err(e) => return Err(e),
        };
        tracks.push((speaker, segments));
    }
    progress(100);
    let transcript = Transcript::merge(tracks);
    transcript.save(dir)?;
    Ok(transcript)
}
