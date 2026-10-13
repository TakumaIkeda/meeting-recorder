//! Merging both tracks into a single speaker-labelled transcript.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::whisper::Segment;

pub const TRANSCRIPT_FILE: &str = "transcript.json";

/// Consecutive segments of the same speaker closer than this are joined into one line.
const JOIN_GAP_MS: u64 = 1_500;
/// ...unless the joined line would get longer than this many characters.
const JOIN_MAX_CHARS: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    /// mic.wav
    Me,
    /// system.wav
    Them,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: Speaker,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub lines: Vec<Line>,
}

/// Whisper's non-speech markers such as `[BLANK_AUDIO]`, `(音楽)` or `（拍手）`.
fn is_non_speech(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    let pairs = [
        ('[', ']'),
        ('(', ')'),
        ('（', '）'),
        ('【', '】'),
        ('♪', '♪'),
    ];
    pairs
        .iter()
        .any(|&(open, close)| t.starts_with(open) && t.ends_with(close) && t.chars().count() >= 2)
}

impl Transcript {
    pub fn merge(tracks: Vec<(Speaker, Vec<Segment>)>) -> Self {
        let mut all: Vec<Line> = tracks
            .into_iter()
            .flat_map(|(speaker, segs)| {
                segs.into_iter().map(move |s| Line {
                    start_ms: s.start_ms,
                    end_ms: s.end_ms.max(s.start_ms),
                    speaker,
                    text: s.text,
                })
            })
            .filter(|l| !is_non_speech(&l.text))
            .collect();
        // Stable sort keeps whisper's order within a track for equal start times.
        all.sort_by_key(|l| l.start_ms);

        let mut lines: Vec<Line> = Vec::with_capacity(all.len());
        for line in all {
            if let Some(prev) = lines.last_mut() {
                let joinable = prev.speaker == line.speaker
                    && line.start_ms <= prev.end_ms + JOIN_GAP_MS
                    && prev.text.chars().count() + line.text.chars().count() <= JOIN_MAX_CHARS;
                if joinable {
                    prev.text.push_str(&line.text);
                    prev.end_ms = prev.end_ms.max(line.end_ms);
                    continue;
                }
            }
            lines.push(line);
        }
        Self { lines }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join(TRANSCRIPT_FILE);
        fs::write(&path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("{} を書き込めません", path.display()))
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(TRANSCRIPT_FILE);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("{} を読み込めません", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("{} の形式が正しくありません", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: u64, end: u64, text: &str) -> Segment {
        Segment {
            start_ms: start,
            end_ms: end,
            text: text.into(),
        }
    }

    #[test]
    fn interleaves_tracks_by_time() {
        let t = Transcript::merge(vec![
            (
                Speaker::Me,
                vec![
                    seg(65_000, 68_000, "検証環境ですが"),
                    seg(122_000, 125_000, "木曜までに"),
                ],
            ),
            (
                Speaker::Them,
                vec![
                    seg(12_000, 15_000, "始めます"),
                    seg(100_000, 103_000, "今週中ですか"),
                ],
            ),
        ]);
        let order: Vec<_> = t.lines.iter().map(|l| (l.start_ms, l.speaker)).collect();
        assert_eq!(
            order,
            vec![
                (12_000, Speaker::Them),
                (65_000, Speaker::Me),
                (100_000, Speaker::Them),
                (122_000, Speaker::Me)
            ]
        );
    }

    #[test]
    fn joins_close_segments_of_same_speaker_only() {
        let t = Transcript::merge(vec![
            (
                Speaker::Me,
                vec![
                    seg(0, 1_000, "はい、"),
                    seg(1_500, 3_000, "金曜に返します。"),
                    seg(10_000, 11_000, "以上です。"),
                ],
            ),
            (Speaker::Them, vec![seg(5_000, 6_000, "お願いします。")]),
        ]);
        let texts: Vec<_> = t.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["はい、金曜に返します。", "お願いします。", "以上です。"]
        );
        assert_eq!(t.lines[0].end_ms, 3_000);
    }

    #[test]
    fn drops_non_speech_markers() {
        let t = Transcript::merge(vec![(
            Speaker::Them,
            vec![
                seg(0, 1, "[BLANK_AUDIO]"),
                seg(2, 3, "（拍手）"),
                seg(4, 5, " "),
                seg(9_000, 9_500, "(笑)ありがとう"),
            ],
        )]);
        assert_eq!(t.lines.len(), 1);
        assert_eq!(t.lines[0].text, "(笑)ありがとう");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("transcript-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let t = Transcript::merge(vec![(Speaker::Me, vec![seg(0, 1000, "テスト")])]);
        t.save(&dir).unwrap();
        assert_eq!(Transcript::load(&dir).unwrap(), t);
        fs::remove_dir_all(&dir).unwrap();
    }
}
