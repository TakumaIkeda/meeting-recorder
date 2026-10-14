//! Transcribe an existing meeting folder without the GUI.
//!
//!     cargo run -p transcriber --example transcribe -- <会議フォルダ> <モデル.bin> [whisper-cli のパス]

use std::path::PathBuf;

use transcriber::{transcribe_meeting, Speaker, Whisper};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: transcribe <会議フォルダ> <モデル.bin> [whisper-cli]";
    let dir = PathBuf::from(args.next().ok_or_else(|| anyhow::anyhow!(usage))?);
    let model = PathBuf::from(args.next().ok_or_else(|| anyhow::anyhow!(usage))?);
    let config = Whisper {
        cli: args.next().unwrap_or_else(|| "whisper-cli".into()).into(),
        model,
        language: "ja".into(),
        threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
    };

    let transcript = transcribe_meeting(&dir, &config, &mut |p| eprint!("\r文字起こし中 {p:3}%"))?;
    eprintln!();
    for l in &transcript.lines {
        let s = l.start_ms / 1000;
        let who = match l.speaker {
            Speaker::Me => "自分",
            Speaker::Them => "相手",
        };
        println!(
            "[{:02}:{:02}:{:02}] {who}: {}",
            s / 3600,
            s / 60 % 60,
            s % 60,
            l.text
        );
    }
    println!("→ {}", dir.join("transcript.json").display());
    Ok(())
}
