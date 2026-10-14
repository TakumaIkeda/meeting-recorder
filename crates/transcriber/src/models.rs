//! Known whisper.cpp ggml models and downloading them with checksum verification.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha1::{Digest, Sha1};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ModelSpec {
    pub id: &'static str,
    pub file: &'static str,
    pub label: &'static str,
    pub note: &'static str,
    pub size_mib: u32,
    /// From the whisper.cpp models README.
    pub sha1: &'static str,
}

/// In order of preference when several are present.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "large-v3-turbo-q5_0",
        file: "ggml-large-v3-turbo-q5_0.bin",
        label: "large-v3-turbo (q5_0)",
        note: "おすすめ。精度と速さのバランスが良い",
        size_mib: 547,
        sha1: "e050f7970618a659205450ad97eb95a18d69c9ee",
    },
    ModelSpec {
        id: "large-v3-turbo",
        file: "ggml-large-v3-turbo.bin",
        label: "large-v3-turbo",
        note: "量子化なし。わずかに高精度だがサイズが大きい",
        size_mib: 1536,
        sha1: "4af2b29d7ec73d781377bfd1758ca957a807e941",
    },
    ModelSpec {
        id: "small",
        file: "ggml-small.bin",
        label: "small",
        note: "軽量・高速。非力な PC 向け（精度は下がる）",
        size_mib: 466,
        sha1: "55356645c2b361a969dfd0ef2c5a50d530afd8d5",
    },
];

const BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

pub fn url(spec: &ModelSpec) -> String {
    format!("{BASE_URL}/{}", spec.file)
}

/// Download `spec` into `dir`. Returns `None` if `cancel` was set.
pub fn download(
    spec: &ModelSpec,
    dir: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
    cancel: &AtomicBool,
) -> Result<Option<PathBuf>> {
    download_from(
        &url(spec),
        spec.sha1,
        &dir.join(spec.file),
        progress,
        cancel,
    )
}

/// Stream `url` into `dest` via `dest.part`, verifying the SHA-1 before renaming into place.
pub fn download_from(
    url: &str,
    sha1: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
    cancel: &AtomicBool,
) -> Result<Option<PathBuf>> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("{} を作成できません", parent.display()))?;
    }
    let part = dest.with_extension("bin.part");
    let result = (|| {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder().tls_config(tls).build().into();
        let response = agent
            .get(url)
            .call()
            .with_context(|| format!("ダウンロードを開始できません: {url}"))?;
        let total = response.body().content_length();
        let mut reader = response.into_body().into_reader();
        let mut out = BufWriter::new(
            File::create(&part).with_context(|| format!("{} を作成できません", part.display()))?,
        );
        let mut hasher = Sha1::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut done: u64 = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let n = reader
                .read(&mut buf)
                .context("ダウンロード中に接続が切れました")?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])
                .context("ファイルに書き込めません（空き容量を確認してください）")?;
            done += n as u64;
            progress(done, total);
        }
        out.flush()?;
        let actual: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if actual != sha1 {
            bail!("ダウンロードしたファイルが壊れています（SHA-1 不一致: {actual}）。もう一度お試しください。");
        }
        Ok(true)
    })();
    match result {
        Ok(true) => {
            fs::rename(&part, dest)
                .with_context(|| format!("{} を保存できません", dest.display()))?;
            Ok(Some(dest.to_path_buf()))
        }
        Ok(false) => {
            let _ = fs::remove_file(&part);
            Ok(None)
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// Minimal HTTP/1.1 server answering every request with `body`.
    fn serve(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                    line.clear();
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        format!("http://{addr}/model.bin")
    }

    fn sha1_hex(data: &[u8]) -> String {
        Sha1::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("model-dl-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn downloads_and_verifies() {
        let body: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let url = serve(body.clone());
        let dir = tmp("ok");
        let dest = dir.join("ggml-test.bin");
        let mut last = (0, None);
        let got = download_from(
            &url,
            &sha1_hex(&body),
            &dest,
            &mut |d, t| last = (d, t),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(got, Some(dest.clone()));
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert_eq!(last, (1_000_000, Some(1_000_000)));
        assert!(!dest.with_extension("bin.part").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_checksum_mismatch_and_cleans_up() {
        let url = serve(b"not the model".to_vec());
        let dir = tmp("bad");
        let dest = dir.join("ggml-test.bin");
        let err = download_from(
            &url,
            &"0".repeat(40),
            &dest,
            &mut |_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(err.to_string().contains("SHA-1"), "{err}");
        assert!(!dest.exists() && !dest.with_extension("bin.part").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancel_leaves_nothing() {
        let url = serve(vec![0u8; 1_000_000]);
        let dir = tmp("cancel");
        let dest = dir.join("ggml-test.bin");
        let got = download_from(&url, "x", &dest, &mut |_, _| {}, &AtomicBool::new(true)).unwrap();
        assert_eq!(got, None);
        assert!(!dest.exists() && !dest.with_extension("bin.part").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn model_table_is_consistent() {
        for m in MODELS {
            assert_eq!(m.sha1.len(), 40);
            assert!(m.file.starts_with("ggml-") && m.file.ends_with(".bin"));
            assert_eq!(find(m.id).unwrap().file, m.file);
        }
    }
}
