// Downloads the pinned whisper.cpp Windows CPU build into src-tauri/whisper/, which Tauri bundles
// as a resource (see tauri.conf.json > bundle > resources). Runs automatically before
// `tauri dev` / `tauri build`; does nothing if the pinned version is already there.
//
// The CPU build picks the best ggml-cpu-*.dll variant at runtime, so it works on any x64 PC.
// On Windows the Visual C++ runtime DLLs it needs are copied next to it (app-local deployment),
// so the app also runs on PCs without the VC++ redistributable installed.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const VERSION = "v1.9.2";
const URL = `https://github.com/ggml-org/whisper.cpp/releases/download/${VERSION}/whisper-bin-x64.zip`;
const SHA256 = "49dcc16de826f20bd53d44f947a1ae49dfa81f86cad67a64d80820cb192d674a";
const VC_RUNTIME = ["msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll", "vcomp140.dll"];

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const dest = join(root, "src-tauri", "whisper");
const stamp = join(dest, "VERSION");
const force = process.argv.includes("--force");

if (!force && existsSync(join(dest, "whisper-cli.exe")) && existsSync(stamp) && readFileSync(stamp, "utf8").trim() === VERSION) {
  process.exit(0);
}

console.log(`whisper.cpp ${VERSION} を取得しています: ${URL}`);
const res = await fetch(URL);
if (!res.ok) throw new Error(`ダウンロードに失敗しました: HTTP ${res.status}`);
const zip = Buffer.from(await res.arrayBuffer());
const hash = createHash("sha256").update(zip).digest("hex");
if (hash !== SHA256) throw new Error(`SHA256 が一致しません: ${hash}（期待値 ${SHA256}）`);

const work = mkdtempSync(join(tmpdir(), "whisper-"));
try {
  const zipPath = join(work, "whisper.zip");
  writeFileSync(zipPath, zip);
  // Windows 10+ ships bsdtar, which reads zip files; elsewhere use unzip.
  if (process.platform === "win32") execFileSync("tar", ["-xf", zipPath, "-C", work]);
  else execFileSync("unzip", ["-q", zipPath, "-d", work]);

  const src = join(work, "Release");
  rmSync(dest, { recursive: true, force: true });
  mkdirSync(dest, { recursive: true });
  const wanted = (f) => f === "whisper-cli.exe" || f === "whisper.dll" || (f.startsWith("ggml") && f.endsWith(".dll"));
  const files = readdirSync(src).filter(wanted);
  for (const f of files) copyFileSync(join(src, f), join(dest, f));

  if (process.platform === "win32") {
    const system32 = join(process.env.SystemRoot ?? "C:\\Windows", "System32");
    const missing = [];
    for (const f of VC_RUNTIME) {
      const p = join(system32, f);
      if (existsSync(p)) copyFileSync(p, join(dest, f));
      else missing.push(f);
    }
    if (missing.length > 0) {
      console.warn(`注意: ${missing.join(", ")} が見つからないため同梱しませんでした。` +
        "実行する PC に Visual C++ 再頒布可能パッケージ (x64) が必要です。");
    }
  }
  writeFileSync(stamp, VERSION + "\n");
  console.log(`src-tauri/whisper/ に ${files.length} ファイルを配置しました`);
} finally {
  rmSync(work, { recursive: true, force: true });
}
