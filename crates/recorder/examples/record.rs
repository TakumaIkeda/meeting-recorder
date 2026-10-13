//! Step 2: record mic + system audio without any GUI.
//!
//!     cargo run -p recorder --example record -- [秒数] [出力先ルート]
//!
//! Default: 10 seconds into ./meetings. Prints elapsed time and input levels while recording.

use std::time::Duration;

use recorder::Recorder;

fn bar(level: f32) -> String {
    // -60 dBFS .. 0 dBFS → 0 .. 20 chars
    let db = 20.0 * level.max(1e-6).log10();
    let n = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 20.0) as usize;
    format!("{:<20}", "#".repeat(n))
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let secs: u64 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(10);
    let root = args.next().unwrap_or_else(|| "meetings".into());

    let rec = Recorder::start(&root)?;
    println!("録音開始: {}", rec.dir().display());
    println!("  マイク      : {:?}", rec.mic_device());
    println!("  システム音声: {:?}", rec.system_device());

    let meters = rec.meters();
    while meters.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(200));
        let e = meters.elapsed().as_secs();
        print!(
            "\r{:02}:{:02}:{:02}  mic [{}]  sys [{}]",
            e / 3600,
            e / 60 % 60,
            e % 60,
            bar(meters.mic.take()),
            bar(meters.system.take())
        );
        use std::io::Write;
        std::io::stdout().flush()?;
        for err in meters.take_errors() {
            println!("\nエラー: {err}");
        }
    }
    println!();

    let summary = rec.stop()?;
    println!("保存先: {}", summary.dir.display());
    for t in [&summary.mic, &summary.system] {
        let reader = hound::WavReader::open(&t.path)?;
        println!(
            "  {} : {:.2} 秒 ({} Hz, {} ch, {} bit) 無音補完 {:.2} 秒 / 不連続 {} 回",
            t.path.display(),
            reader.duration() as f64 / reader.spec().sample_rate as f64,
            reader.spec().sample_rate,
            reader.spec().channels,
            reader.spec().bits_per_sample,
            t.inserted_silence_secs,
            t.discontinuities
        );
    }
    Ok(())
}
