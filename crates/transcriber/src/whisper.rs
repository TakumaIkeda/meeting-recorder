//! Running `whisper-cli` on one track and parsing its JSON output (`-oj`).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::config::Whisper;

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
/// A segment whose loudest 100 ms stays below this RMS (~ -55 dBFS) has no speech in it: it is a
/// hallucination on (near-)silence. Deliberately low, so quiet but real speech is kept.
const SPEECH_RMS: f32 = 0.0018;
const WINDOW_MS: u64 = 100;

/// RMS level per 100 ms window of a 16-bit mono WAV, plus its peak.
pub(crate) struct Energy {
    windows: Vec<f32>,
    peak: i16,
}

impl Energy {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let mut reader = hound::WavReader::open(path)
            .with_context(|| format!("{} を開けません", path.display()))?;
        let per_window = (reader.spec().sample_rate as u64 * WINDOW_MS / 1000).max(1) as usize;
        let mut windows = Vec::new();
        let (mut sum, mut n, mut peak) = (0f64, 0usize, 0i16);
        for s in reader.samples::<i16>() {
            let s = s?;
            peak = peak.max(s.saturating_abs());
            let x = s as f64 / i16::MAX as f64;
            sum += x * x;
            n += 1;
            if n == per_window {
                windows.push((sum / n as f64).sqrt() as f32);
                (sum, n) = (0.0, 0);
            }
        }
        if n > 0 {
            windows.push((sum / n as f64).sqrt() as f32);
        }
        Ok(Self { windows, peak })
    }

    fn is_silent(&self) -> bool {
        self.peak <= SILENCE_PEAK
    }

    /// Loudest window between `start_ms` and `end_ms`.
    fn max_rms(&self, start_ms: u64, end_ms: u64) -> f32 {
        let from = ((start_ms / WINDOW_MS) as usize).min(self.windows.len());
        let to = (end_ms.div_ceil(WINDOW_MS) as usize).clamp(from, self.windows.len());
        self.windows[from..to].iter().copied().fold(0.0, f32::max)
    }
}

/// Drop segments that have no audible sound under them.
pub(crate) fn drop_silent_segments(segments: Vec<Segment>, energy: &Energy) -> Vec<Segment> {
    segments
        .into_iter()
        .filter(|s| energy.max_rms(s.start_ms, s.end_ms.max(s.start_ms + 1)) >= SPEECH_RMS)
        .collect()
}

/// Transcribe `dir/<name>.wav`; writes whisper's raw output to `dir/<name>.json` and its
/// console output to `dir/whisper-<name>.log`.
pub fn transcribe_track(
    dir: &Path,
    name: &str,
    config: &Whisper,
    progress: &mut dyn FnMut(u32),
) -> Result<Vec<Segment>> {
    let energy = Energy::read(&dir.join(format!("{name}.wav")))?;
    if energy.is_silent() {
        progress(100);
        return Ok(Vec::new());
    }
    let segments = match run_cli(dir, name, config, true, progress) {
        // A whisper-cli set in config.json may predate --vad / -mc: run it without them.
        Err(RunError::UnknownArgument(_)) => run_cli(dir, name, config, false, progress),
        other => other,
    }
    .map_err(RunError::into_anyhow)?;
    Ok(drop_silent_segments(segments, &energy))
}

enum RunError {
    UnknownArgument(anyhow::Error),
    Other(anyhow::Error),
}

impl RunError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            RunError::UnknownArgument(e) | RunError::Other(e) => e,
        }
    }
}

impl From<anyhow::Error> for RunError {
    fn from(e: anyhow::Error) -> Self {
        RunError::Other(e)
    }
}

impl From<std::io::Error> for RunError {
    fn from(e: std::io::Error) -> Self {
        RunError::Other(e.into())
    }
}

fn run_cli(
    dir: &Path,
    name: &str,
    config: &Whisper,
    anti_hallucination: bool,
    progress: &mut dyn FnMut(u32),
) -> Result<Vec<Segment>, RunError> {
    let wav = format!("{name}.wav");
    let json_path = dir.join(format!("{name}.json"));
    let _ = std::fs::remove_file(&json_path);

    // Run inside the meeting folder with relative names so the paths whisper-cli sees are ASCII.
    let mut cmd = Command::new(&config.cli);
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
        .args(["-t", &config.threads.to_string()]);
    if anti_hallucination {
        cmd.args(["-mc", &config.max_context.to_string()]);
        if let Some(vad) = &config.vad_model {
            cmd.arg("--vad").arg("-vm").arg(vad);
        }
    }
    cmd.stdin(Stdio::null())
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
                "whisper-cli が見つかりません: {}\nアプリに同梱されていない場合は npm run fetch-whisper を実行してビルドし直すか、config.json の \"whisper_cli\" にパスを指定してください。",
                config.cli.display()
            )
        } else {
            anyhow!("whisper-cli を起動できません: {e}")
        }
    })?;

    // Log everything, keep the tail of stderr for error reporting, and parse progress lines.
    let log_path = dir.join(format!("whisper-{name}.log"));
    let mut log = std::fs::File::create(&log_path)
        .with_context(|| format!("{} を作成できません", log_path.display()))?;
    let mut tail: Vec<String> = Vec::new();
    let mut unknown_argument = false;
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).split(b'\n') {
            let line = String::from_utf8_lossy(&line?).trim_end().to_string();
            let _ = writeln!(log, "{line}");
            if let Some(p) = parse_progress(&line) {
                progress(p);
            } else if !line.is_empty() {
                unknown_argument |= line.contains("unknown argument");
                tail.push(line);
                if tail.len() > 15 {
                    tail.remove(0);
                }
            }
        }
    }
    let status = child.wait()?;
    // 0xC0000135 STATUS_DLL_NOT_FOUND: typically the Visual C++ runtime is missing.
    if status.code() == Some(0xC000_0135_u32 as i32) {
        return Err(anyhow!(
            "whisper-cli の実行に必要な DLL が見つかりません。\nMicrosoft Visual C++ 再頒布可能パッケージ (x64) をインストールしてください: https://aka.ms/vs/17/release/vc_redist.x64.exe"
        )
        .into());
    }
    if !status.success() {
        let e = anyhow!(
            "whisper-cli が失敗しました（{wav}, {status}）\n{}",
            tail.join("\n")
        );
        return Err(if unknown_argument && anti_hallucination {
            RunError::UnknownArgument(e)
        } else {
            RunError::Other(e)
        });
    }
    let text = std::fs::read(&json_path)
        .with_context(|| format!("whisper-cli の出力 {} がありません", json_path.display()))?;
    progress(100);
    Ok(parse_output(&text).with_context(|| format!("{} を解析できません", json_path.display()))?)
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

    fn energy_from(levels: &[f32]) -> Energy {
        Energy {
            windows: levels.to_vec(),
            peak: i16::MAX,
        }
    }

    #[test]
    fn drops_segments_over_silence_only() {
        // 0-1 s speech, 1-3 s near-silence (room noise at -70 dBFS), 3-4 s quiet speech (-45 dBFS).
        let mut levels = vec![0.1; 10];
        levels.extend(vec![0.0003; 20]);
        levels.extend(vec![0.0056; 10]);
        let e = energy_from(&levels);
        let segs = vec![
            Segment {
                start_ms: 0,
                end_ms: 1_000,
                text: "話した".into(),
            },
            Segment {
                start_ms: 1_200,
                end_ms: 2_800,
                text: "ご視聴ありがとうございました".into(),
            },
            Segment {
                start_ms: 3_000,
                end_ms: 4_000,
                text: "小さい声".into(),
            },
            Segment {
                start_ms: 2_500,
                end_ms: 3_100,
                text: "またがる".into(),
            },
            Segment {
                start_ms: 9_000,
                end_ms: 9_500,
                text: "ファイルの外".into(),
            },
        ];
        let kept: Vec<_> = drop_silent_segments(segs, &e)
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(kept, vec!["話した", "小さい声", "またがる"]);
    }

    #[test]
    fn energy_windows_from_wav() {
        let path = std::env::temp_dir().join(format!("energy-{}.wav", std::process::id()));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..3_200 {
            // 100 ms of a loud square wave, then 100 ms of silence.
            w.write_sample(if i < 1_600 {
                if i % 2 == 0 {
                    16_384i16
                } else {
                    -16_384
                }
            } else {
                0
            })
            .unwrap();
        }
        w.finalize().unwrap();
        let e = Energy::read(&path).unwrap();
        assert_eq!(e.windows.len(), 2);
        assert!((e.windows[0] - 0.5).abs() < 0.01 && e.windows[1] == 0.0);
        assert!(!e.is_silent());
        std::fs::remove_file(&path).unwrap();
    }

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
