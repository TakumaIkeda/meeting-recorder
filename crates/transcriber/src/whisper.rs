//! Running `whisper-cli` on one track and parsing its JSON output (`-oj`).

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::config::WhisperConfig;

/// One whisper segment, in milliseconds from the start of the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Peak below this (~ -50 dBFS) means the track is silent; whisper tends to hallucinate
/// phrases like 「ご視聴ありがとうございました」 on silence, so such tracks are skipped.
const SILENCE_PEAK: i16 = 100;

/// Transcribe `dir/<name>.wav`; writes whisper's raw output to `dir/<name>.json`.
pub fn transcribe_track(
    dir: &Path,
    name: &str,
    config: &WhisperConfig,
    progress: &mut dyn FnMut(u32),
) -> Result<Vec<Segment>> {
    let wav = format!("{name}.wav");
    if is_silent(&dir.join(&wav))? {
        progress(100);
        return Ok(Vec::new());
    }
    let json_path = dir.join(format!("{name}.json"));
    let _ = std::fs::remove_file(&json_path);

    // Run inside the meeting folder with relative names so the paths whisper-cli sees are ASCII.
    let mut cmd = Command::new(&config.whisper_cli);
    cmd.current_dir(dir)
        .arg("-m")
        .arg(&config.model)
        .args([
            "-f",
            &wav,
            "-of",
            name,
            "-oj",
            "-pp",
            "-l",
            &config.language,
        ])
        .args(["-t", &config.threads().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow!(
                "whisper-cli が見つかりません: {}\n設定ファイルの \"whisper_cli\" に whisper-cli のパスを指定してください。",
                config.whisper_cli.display()
            )
        } else {
            anyhow!("whisper-cli を起動できません: {e}")
        }
    })?;

    // Keep the tail of stderr for error reporting, and parse progress lines as they arrive.
    let mut tail: Vec<String> = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).split(b'\n') {
            let line = String::from_utf8_lossy(&line?).trim_end().to_string();
            if let Some(p) = parse_progress(&line) {
                progress(p);
            } else if !line.is_empty() {
                tail.push(line);
                if tail.len() > 15 {
                    tail.remove(0);
                }
            }
        }
    }
    let status = child.wait()?;
    if !status.success() {
        bail!(
            "whisper-cli が失敗しました（{wav}, {status}）\n{}",
            tail.join("\n")
        );
    }
    let text = std::fs::read(&json_path)
        .with_context(|| format!("whisper-cli の出力 {} がありません", json_path.display()))?;
    progress(100);
    parse_output(&text).with_context(|| format!("{} を解析できません", json_path.display()))
}

fn is_silent(path: &Path) -> Result<bool> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("{} を開けません", path.display()))?;
    for s in reader.samples::<i16>() {
        if s?.unsigned_abs() > SILENCE_PEAK as u16 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `whisper_print_progress_callback: progress =  35%` → 35
pub(crate) fn parse_progress(line: &str) -> Option<u32> {
    let rest = line.split("progress =").nth(1)?;
    rest.trim().trim_end_matches('%').trim().parse().ok()
}

#[derive(Deserialize)]
struct Output {
    transcription: Vec<RawSegment>,
}

#[derive(Deserialize)]
struct RawSegment {
    offsets: Offsets,
    text: String,
}

#[derive(Deserialize)]
struct Offsets {
    from: u64,
    to: u64,
}

pub(crate) fn parse_output(json: &[u8]) -> Result<Vec<Segment>> {
    // whisper.cpp may cut a multi-byte character at a token boundary; don't fail on that.
    let text = String::from_utf8_lossy(json);
    let out: Output = serde_json::from_str(&text)?;
    Ok(out
        .transcription
        .into_iter()
        .map(|s| Segment {
            start_ms: s.offsets.from,
            end_ms: s.offsets.to,
            text: s.text.trim().to_string(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line() {
        assert_eq!(
            parse_progress("whisper_print_progress_callback: progress =  35%"),
            Some(35)
        );
        assert_eq!(
            parse_progress("whisper_print_progress_callback: progress = 100%"),
            Some(100)
        );
        assert_eq!(
            parse_progress("whisper_init_from_file: loading model"),
            None
        );
    }

    #[test]
    fn whisper_json() {
        let json = r#"{
            "systeminfo": "AVX = 1",
            "model": {"type": "large"},
            "result": {"language": "ja"},
            "transcription": [
                {"timestamps": {"from": "00:00:00,000", "to": "00:00:02,500"},
                 "offsets": {"from": 0, "to": 2500}, "text": " こんにちは。"},
                {"timestamps": {"from": "00:00:02,500", "to": "00:00:04,000"},
                 "offsets": {"from": 2500, "to": 4000}, "text": "よろしく\"お願い\"します"}
            ]
        }"#;
        let segs = parse_output(json.as_bytes()).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(
            segs[0],
            Segment {
                start_ms: 0,
                end_ms: 2500,
                text: "こんにちは。".into()
            }
        );
        assert_eq!(segs[1].text, "よろしく\"お願い\"します");
    }
}
