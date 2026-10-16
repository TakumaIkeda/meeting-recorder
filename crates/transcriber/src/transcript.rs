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

/// Phrases whisper is known to invent on silence (learned from video subtitles). A segment that
/// consists of only one of these is dropped.
const HALLUCINATIONS: &[&str] = &[
    "ご視聴ありがとうございました",
    "最後までご視聴いただきありがとうございました",
    "チャンネル登録よろしくお願いします",
    "チャンネル登録お願いします",
];

/// Text without whitespace and punctuation, for comparisons.
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !"。、．，.,!?！？・…ー〜~「」『』\"'".contains(*c))
        .collect()
}

fn is_hallucination(text: &str) -> bool {
    let t = normalize(text);
    HALLUCINATIONS.iter().any(|h| t == normalize(h))
}

/// Minimum consecutive repetitions of a phrase inside one segment to treat it as a loop.
const LOOP_MIN_REPEATS: usize = 4;
const LOOP_MAX_PHRASE_CHARS: usize = 30;

/// Collapse whisper's repetition loops inside a segment, e.g.
/// 「そうですね。そうですね。そうですね。そうですね。」 → 「そうですね。」
fn collapse_loops(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    'outer: while i < chars.len() {
        for len in 1..=LOOP_MAX_PHRASE_CHARS.min((chars.len() - i) / LOOP_MIN_REPEATS) {
            let phrase = &chars[i..i + len];
            let mut repeats = 1;
            while i + (repeats + 1) * len <= chars.len()
                && &chars[i + repeats * len..i + (repeats + 1) * len] == phrase
            {
                repeats += 1;
            }
            if repeats >= LOOP_MIN_REPEATS {
                out.extend(phrase);
                i += repeats * len;
                continue 'outer;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Clean one track's segments: drop non-speech markers and known hallucinations, collapse
/// loops, and drop a segment that just repeats the previous one.
fn clean_track(segs: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::with_capacity(segs.len());
    for mut s in segs {
        s.text = collapse_loops(s.text.trim());
        if is_non_speech(&s.text) || is_hallucination(&s.text) {
            continue;
        }
        if out
            .last()
            .is_some_and(|prev| normalize(&prev.text) == normalize(&s.text))
        {
            continue;
        }
        out.push(s);
    }
    out
}

impl Transcript {
    pub fn merge(tracks: Vec<(Speaker, Vec<Segment>)>) -> Self {
        let mut all: Vec<Line> = tracks
            .into_iter()
            .flat_map(|(speaker, segs)| {
                clean_track(segs).into_iter().map(move |s| Line {
                    start_ms: s.start_ms,
                    end_ms: s.end_ms.max(s.start_ms),
                    speaker,
                    text: s.text,
                })
            })
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
    fn collapses_repetition_loops() {
        assert_eq!(
            collapse_loops("そうですね。そうですね。そうですね。そうですね。"),
            "そうですね。"
        );
        assert_eq!(
            collapse_loops("はい、ありがとうありがとうありがとうありがとうございます"),
            "はい、ありがとうございます"
        );
        assert_eq!(collapse_loops("ーーーーーー"), "ー");
        // Ordinary text and short natural repetition stay as they are.
        assert_eq!(
            collapse_loops("はいはいはい、了解です。"),
            "はいはいはい、了解です。"
        );
        assert_eq!(
            collapse_loops("木曜までには終わらせます。"),
            "木曜までには終わらせます。"
        );
    }

    #[test]
    fn drops_hallucinations_and_repeated_segments() {
        let t = Transcript::merge(vec![(
            Speaker::Me,
            vec![
                seg(0, 1_000, "検証環境ですが"),
                seg(5_000, 6_000, "ご視聴ありがとうございました。"),
                seg(8_000, 9_000, "了解です。"),
                seg(11_000, 12_000, "了解です"),
                seg(14_000, 15_000, "了解です。"),
                seg(
                    30_000,
                    31_000,
                    "では、ご視聴ありがとうございましたと言って締めます",
                ),
            ],
        )]);
        let texts: Vec<_> = t.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "検証環境ですが",
                "了解です。",
                "では、ご視聴ありがとうございましたと言って締めます"
            ]
        );
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
