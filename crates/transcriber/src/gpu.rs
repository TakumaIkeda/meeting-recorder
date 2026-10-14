//! Optional NVIDIA GPU acceleration: the official whisper.cpp CUDA build, downloaded on demand.
//!
//! It is a superset of the CPU build: ggml loads `ggml-cuda.dll` dynamically and falls back to
//! the CPU backends when no NVIDIA driver is present. It is too large (~1.2 GB unpacked) to put in
//! every installer, so the app offers it only on PCs with an NVIDIA driver.

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::models::{download_from, Checksum};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct GpuPack {
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Download size.
    pub size_mib: u32,
    dir_name: &'static str,
}

pub const CUDA_PACK: GpuPack = GpuPack {
    version: "v1.9.2 (CUDA 12.4)",
    url: "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.2/whisper-cublas-12.4.0-bin-x64.zip",
    sha256: "443110ddaad70d4290ab2e77179e31cf712035bbc4fad56bb4519a90c917b39c",
    size_mib: 640,
    dir_name: "whisper-cuda-v1.9.2",
};

/// Written last, so a half-finished install is never picked up.
const MARKER: &str = "INSTALLED";

/// Files needed to run whisper-cli with CUDA (the archive also carries unrelated tools).
fn wanted(name: &str) -> bool {
    name == "whisper-cli.exe"
        || name == "whisper.dll"
        || (name.starts_with("ggml") && name.ends_with(".dll"))
        || matches!(
            name,
            "cudart64_12.dll" | "cublas64_12.dll" | "cublasLt64_12.dll"
        )
}

/// Whether an NVIDIA driver is installed (it ships `nvcuda.dll`).
pub fn nvidia_driver_present() -> bool {
    if !cfg!(windows) {
        return false;
    }
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    Path::new(&system_root)
        .join("System32")
        .join("nvcuda.dll")
        .is_file()
}

impl GpuPack {
    pub fn install_dir(&self, runtime_dir: &Path) -> PathBuf {
        runtime_dir.join(self.dir_name)
    }

    /// `whisper-cli.exe` of a completed install, if any.
    pub fn installed_cli(&self, runtime_dir: &Path) -> Option<PathBuf> {
        let dir = self.install_dir(runtime_dir);
        dir.join(MARKER)
            .is_file()
            .then(|| dir.join("whisper-cli.exe"))
            .filter(|p| p.is_file())
    }

    /// Download, verify and unpack into `runtime_dir`. The Visual C++ runtime DLLs found in
    /// `vc_runtime_from` (the bundled CPU build's folder) are copied alongside.
    /// Returns `None` if cancelled.
    pub fn install(
        &self,
        runtime_dir: &Path,
        vc_runtime_from: Option<&Path>,
        progress: &mut dyn FnMut(u64, Option<u64>),
        cancel: &AtomicBool,
    ) -> Result<Option<PathBuf>> {
        let zip_path = runtime_dir.join(format!("{}.zip", self.dir_name));
        let Some(zip_path) = download_from(
            self.url,
            Checksum::Sha256(self.sha256),
            &zip_path,
            progress,
            cancel,
        )?
        else {
            return Ok(None);
        };
        let result = unpack(&zip_path, &self.install_dir(runtime_dir), vc_runtime_from);
        let _ = fs::remove_file(&zip_path);
        result?;
        Ok(self.installed_cli(runtime_dir))
    }
}

/// Extract the wanted files of `zip_path` into `dest` via a temporary folder.
pub(crate) fn unpack(zip_path: &Path, dest: &Path, vc_runtime_from: Option<&Path>) -> Result<()> {
    let tmp = dest.with_extension("tmp");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).with_context(|| format!("{} を作成できません", tmp.display()))?;

    let file =
        File::open(zip_path).with_context(|| format!("{} を開けません", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).context("zip を読み込めません")?;
    let mut extracted = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        // Entries look like `Release/whisper-cli.exe`; keep only the file name.
        let Some(name) = entry
            .enclosed_name()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        else {
            continue;
        };
        if entry.is_dir() || !wanted(&name) {
            continue;
        }
        let out_path = tmp.join(&name);
        let mut out = File::create(&out_path)
            .with_context(|| format!("{} を作成できません", out_path.display()))?;
        io::copy(&mut entry, &mut out)
            .context("展開に失敗しました（空き容量を確認してください）")?;
        extracted += 1;
    }
    anyhow::ensure!(
        tmp.join("whisper-cli.exe").is_file(),
        "zip に whisper-cli.exe が含まれていません"
    );

    if let Some(src) = vc_runtime_from {
        for name in [
            "msvcp140.dll",
            "vcruntime140.dll",
            "vcruntime140_1.dll",
            "vcomp140.dll",
        ] {
            if src.join(name).is_file() {
                fs::copy(src.join(name), tmp.join(name))?;
            }
        }
    }
    fs::write(tmp.join(MARKER), format!("{extracted} files\n"))?;
    let _ = fs::remove_dir_all(dest);
    fs::rename(&tmp, dest).with_context(|| format!("{} に配置できません", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(path: &Path, files: &[&str]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for f in files {
            w.start_file(format!("Release/{f}"), opts).unwrap();
            w.write_all(f.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn unpacks_only_runtime_files_and_marks_install() {
        let root = std::env::temp_dir().join(format!("gpu-pack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let zip_path = root.join("pack.zip");
        make_zip(
            &zip_path,
            &[
                "whisper-cli.exe",
                "whisper.dll",
                "ggml-cuda.dll",
                "ggml-cpu-x64.dll",
                "cublasLt64_12.dll",
                "wchess.exe",
                "SDL2.dll",
            ],
        );
        let vc = root.join("bundled");
        fs::create_dir_all(&vc).unwrap();
        fs::write(vc.join("vcruntime140.dll"), b"vc").unwrap();

        let pack = CUDA_PACK;
        assert_eq!(pack.installed_cli(&root), None);
        unpack(&zip_path, &pack.install_dir(&root), Some(&vc)).unwrap();

        let dir = pack.install_dir(&root);
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "INSTALLED",
                "cublasLt64_12.dll",
                "ggml-cpu-x64.dll",
                "ggml-cuda.dll",
                "vcruntime140.dll",
                "whisper-cli.exe",
                "whisper.dll"
            ]
        );
        assert_eq!(pack.installed_cli(&root), Some(dir.join("whisper-cli.exe")));
        assert!(!dir.with_extension("tmp").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn zip_without_cli_is_rejected_and_nothing_is_installed() {
        let root = std::env::temp_dir().join(format!("gpu-pack-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let zip_path = root.join("pack.zip");
        make_zip(&zip_path, &["ggml.dll"]);
        assert!(unpack(&zip_path, &CUDA_PACK.install_dir(&root), None).is_err());
        assert_eq!(CUDA_PACK.installed_cli(&root), None);
        fs::remove_dir_all(&root).unwrap();
    }
}
