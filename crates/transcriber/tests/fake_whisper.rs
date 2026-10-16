//! End-to-end run of `transcribe_meeting` against a stand-in `whisper-cli` shell script.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use transcriber::{transcribe_meeting, Speaker, Whisper};

fn write_wav(path: &Path, amplitude: i16) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    // 3 s, so the fake segments (1-2 s) have audio under them.
    for i in 0..48_000 {
        w.write_sample(if i % 2 == 0 { amplitude } else { -amplitude })
            .unwrap();
    }
    w.finalize().unwrap();
}

const FAKE: &str = r#"#!/bin/sh
# Find the -of argument and emit whisper.cpp-style progress + JSON.
while [ $# -gt 0 ]; do
  if [ "$1" = "-of" ]; then out="$2"; fi
  shift
done
echo "whisper_init_from_file: loading model" >&2
echo "whisper_print_progress_callback: progress =  50%" >&2
cat > "$out.json" <<JSON
{"transcription": [
  {"offsets": {"from": 1000, "to": 2000}, "text": " $out の発言"},
  {"offsets": {"from": 5000, "to": 6000}, "text": "[BLANK_AUDIO]"}
]}
JSON
"#;

#[test]
fn transcribes_both_tracks_and_skips_silent_ones() {
    let root = std::env::temp_dir().join(format!("fake-whisper-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let dir = root.join("2026-10-13_1030");
    fs::create_dir_all(&dir).unwrap();
    let cli = root.join("whisper-cli");
    fs::write(&cli, FAKE).unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
    let model = root.join("model.bin");
    fs::write(&model, b"").unwrap();
    let config = Whisper {
        cli: cli.clone(),
        fallback_cli: None,
        model: model.clone(),
        language: "ja".into(),
        threads: 2,
        vad_model: None,
        max_context: 0,
    };

    write_wav(&dir.join("mic.wav"), 3000);
    write_wav(&dir.join("system.wav"), 3000);
    let mut progress = Vec::new();
    let t = transcribe_meeting(&dir, &config, &mut |p| progress.push(p)).unwrap();
    assert_eq!(t.lines.len(), 2);
    assert_eq!(t.lines[0].speaker, Speaker::Me);
    assert_eq!(t.lines[0].text, "mic の発言");
    assert_eq!(t.lines[1].speaker, Speaker::Them);
    assert!(progress.contains(&25) && progress.contains(&75) && progress.last() == Some(&100));
    assert!(dir.join("transcript.json").is_file());

    // A silent system track is not sent to whisper at all.
    write_wav(&dir.join("system.wav"), 0);
    let t = transcribe_meeting(&dir, &config, &mut |_| {}).unwrap();
    assert_eq!(t.lines.len(), 1);

    // A failing primary build (e.g. GPU) falls back to the CPU build per track.
    write_wav(&dir.join("system.wav"), 3000);
    let broken = root.join("broken-cli");
    fs::write(
        &broken,
        "#!/bin/sh\necho 'ggml_cuda_init: failed' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o755)).unwrap();
    let gpu_first = Whisper {
        cli: broken.clone(),
        fallback_cli: Some(cli.clone()),
        ..config.clone()
    };
    let t = transcribe_meeting(&dir, &gpu_first, &mut |_| {}).unwrap();
    assert_eq!(t.lines.len(), 2);
    assert!(dir.join("whisper-mic-gpu-failed.log").is_file());

    // Missing whisper-cli gives an actionable error.
    let bad = Whisper {
        cli: root.join("nope"),
        ..config
    };
    let err = format!(
        "{:#}",
        transcribe_meeting(&dir, &bad, &mut |_| {}).unwrap_err()
    );
    assert!(err.contains("whisper-cli が見つかりません"), "{err}");

    fs::remove_dir_all(&root).unwrap();
}

/// Records its arguments; rejects --vad like a whisper-cli that predates VAD support.
const OLD_CLI: &str = r#"#!/bin/sh
echo "$@" >> "$(pwd)/args.log"
for a in "$@"; do
  if [ "$a" = "--vad" ]; then echo "error: unknown argument: --vad" >&2; exit 1; fi
done
while [ $# -gt 0 ]; do
  if [ "$1" = "-of" ]; then out="$2"; fi
  shift
done
cat > "$out.json" <<JSON
{"transcription": [{"offsets": {"from": 1000, "to": 2000}, "text": " $out"}]}
JSON
"#;

#[test]
fn passes_vad_flags_and_retries_without_them_on_old_cli() {
    let root = std::env::temp_dir().join(format!("fake-whisper-vad-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let dir = root.join("2026-10-16_1000");
    fs::create_dir_all(&dir).unwrap();
    let cli = root.join("whisper-cli");
    fs::write(&cli, OLD_CLI).unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
    let vad = root.join("ggml-silero.bin");
    fs::write(&vad, b"").unwrap();
    write_wav(&dir.join("mic.wav"), 3000);
    write_wav(&dir.join("system.wav"), 0);

    let config = Whisper {
        cli,
        fallback_cli: None,
        model: root.join("model.bin"),
        language: "ja".into(),
        threads: 2,
        vad_model: Some(vad.clone()),
        max_context: 0,
    };
    let t = transcribe_meeting(&dir, &config, &mut |_| {}).unwrap();
    assert_eq!(t.lines.len(), 1);

    let args = fs::read_to_string(dir.join("args.log")).unwrap();
    let runs: Vec<&str> = args.lines().collect();
    assert_eq!(runs.len(), 2, "{args}");
    assert!(
        runs[0].contains("-mc 0") && runs[0].contains(&format!("--vad -vm {}", vad.display())),
        "{}",
        runs[0]
    );
    assert!(
        !runs[1].contains("--vad") && !runs[1].contains("-mc"),
        "{}",
        runs[1]
    );
    fs::remove_dir_all(&root).unwrap();
}
