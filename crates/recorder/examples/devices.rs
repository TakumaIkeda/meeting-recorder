//! Step 1: list devices and check that mic + WASAPI loopback capture actually deliver audio.
//!
//!     cargo run -p recorder --example devices
//!
//! Play some audio (e.g. a YouTube video) while it runs so the loopback probe has data.

use std::time::Duration;

use recorder::devices::{list_all_devices, probe, SourceKind};

fn main() -> anyhow::Result<()> {
    let defaults = recorder::default_devices();
    println!("Host: {}", defaults.host);
    println!();
    println!("== 全デバイス ==");
    for d in list_all_devices()? {
        let mut marks = Vec::new();
        if d.is_default_input {
            marks.push("既定の入力");
        }
        if d.is_default_output {
            marks.push("既定の出力");
        }
        println!(
            "- {} {}",
            d.name,
            if marks.is_empty() {
                String::new()
            } else {
                format!("[{}]", marks.join(", "))
            }
        );
        if let Some(i) = &d.input {
            println!(
                "    input : {} Hz, {} ch, {}",
                i.sample_rate, i.channels, i.sample_format
            );
        }
        if let Some(o) = &d.output {
            println!(
                "    output: {} Hz, {} ch, {}",
                o.sample_rate, o.channels, o.sample_format
            );
        }
    }

    println!();
    println!("== 3 秒間キャプチャして確認（システム音声側は何か再生しておいてください）==");
    for (label, kind) in [
        ("マイク", SourceKind::Mic),
        ("システム音声(ループバック)", SourceKind::System),
    ] {
        match probe(kind, Duration::from_secs(3)) {
            Ok(r) => {
                let secs = r.frames as f64 / r.device.sample_rate as f64;
                println!(
                    "{label}: {} / {} Hz {} ch {} → callbacks={} 受信={:.2} 秒 peak={:.3} ({:.1} dBFS)",
                    r.device.name,
                    r.device.sample_rate,
                    r.device.channels,
                    r.device.sample_format,
                    r.callbacks,
                    secs,
                    r.peak,
                    20.0 * r.peak.max(1e-6).log10()
                );
                if r.frames == 0 {
                    println!("  ⚠ データが届いていません（ループバックの場合、無音中はデータが来ないのが正常です）");
                }
                for e in r.errors {
                    println!("  エラー: {e}");
                }
            }
            Err(e) => println!("{label}: 失敗: {e:#}"),
        }
    }
    Ok(())
}
