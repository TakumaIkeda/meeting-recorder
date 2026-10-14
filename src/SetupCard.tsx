import { useEffect, useState } from "react";
import {
  cancelModelDownload,
  getSetup,
  onModelDownload,
  setUseGpu,
  startGpuDownload,
  startModelDownload,
  type DownloadState,
  type SetupStatus,
} from "./api";

const GPU_DOWNLOAD_ID = "gpu-cuda";

const mb = (bytes: number) => Math.round(bytes / (1024 * 1024));
const sizeLabel = (mib: number) => (mib >= 1024 ? `${(mib / 1024).toFixed(1)} GB` : `${mib} MB`);

function Progress({ download, fallbackMib }: { download: DownloadState; fallbackMib: number }) {
  const total = download.total ?? fallbackMib * 1024 * 1024;
  const pct = total > 0 ? Math.min(100, (download.downloaded / total) * 100) : 0;
  const unpacking = download.id === GPU_DOWNLOAD_ID && download.total != null && download.downloaded >= download.total;
  return (
    <div className="download">
      <div className="download-bar">
        <div className="download-fill" style={{ width: `${pct}%` }} />
      </div>
      <div className="download-row">
        <span className="mono muted small">
          {unpacking ? "展開中…" : `${mb(download.downloaded)} / ${mb(total)} MB`}
        </span>
        {!unpacking && (
          <button type="button" className="btn-small" onClick={() => cancelModelDownload()}>
            キャンセル
          </button>
        )}
      </div>
    </div>
  );
}

/**
 * Shown on the main screen while something is missing for transcription (whisper-cli, a model),
 * or while the optional GPU build can still be downloaded on a PC with an NVIDIA GPU.
 */
export default function SetupCard({ onStatus }: { onStatus?: (s: SetupStatus) => void }) {
  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [download, setDownload] = useState<DownloadState | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = () =>
    getSetup()
      .then((s) => {
        setSetup(s);
        setDownload(s.download);
        setSelected((cur) => cur ?? s.models[0]?.id ?? null);
        onStatus?.(s);
      })
      .catch((e) => setError(String(e)));

  useEffect(() => {
    refresh();
    const unlisten = onModelDownload((d) => {
      setDownload(d);
      if (d.state === "done") refresh();
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  if (!setup) return error ? <div className="notice notice-error">{error}</div> : null;

  const offerGpu = setup.nvidia && !setup.gpu_installed && setup.use_gpu;
  if (setup.model_found && setup.cli_found && !offerGpu) return null;

  const running = download?.state === "running";
  const modelDownload = download && download.id !== GPU_DOWNLOAD_ID ? download : null;
  const gpuDownload = download?.id === GPU_DOWNLOAD_ID ? download : null;
  const spec = setup.models.find((m) => m.id === (modelDownload?.state === "running" ? modelDownload.id : selected));

  const run = async (f: () => Promise<void>) => {
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <section aria-label="文字起こしの準備" className="card setup">
      <div className="setup-head">
        <h2>文字起こしの準備</h2>
        <div className="muted small">録音はこのままでも行えます。</div>
      </div>

      {!setup.cli_found && (
        <div className="notice notice-error setup-notice">
          whisper-cli が見つかりません: {setup.cli_path}
          {"\n"}`npm run fetch-whisper` を実行してからビルドし直すか、{setup.config_path} の "whisper_cli" にパスを指定してください。
        </div>
      )}

      {!setup.model_found && (
        <div className="setup-section">
          <h3>文字起こしモデル（必須）</h3>
          <div className="setup-text">
            初回のみダウンロードします。保存先 <span className="mono">{setup.models_dir}</span>
            {setup.model_path && (
              <div className="muted small">
                config.json で指定されたモデルが見つかりません: <span className="mono">{setup.model_path}</span>
              </div>
            )}
          </div>
          <div className="model-options" role="radiogroup">
            {setup.models.map((m) => (
              <label key={m.id} className={`model-option${selected === m.id ? " selected" : ""}`}>
                <input
                  type="radio"
                  name="model"
                  value={m.id}
                  checked={selected === m.id}
                  disabled={running}
                  onChange={() => setSelected(m.id)}
                />
                <span className="model-label">{m.label}</span>
                <span className="model-size mono">{sizeLabel(m.size_mib)}</span>
                <span className="model-note">{m.note}</span>
              </label>
            ))}
          </div>
          {modelDownload?.state === "running" ? (
            <Progress download={modelDownload} fallbackMib={spec?.size_mib ?? 0} />
          ) : (
            <div className="download-row">
              <span className="muted small">{modelDownload?.state === "cancelled" && "キャンセルしました。"}</span>
              <button
                type="button"
                className="btn-outline"
                onClick={() => selected && run(() => startModelDownload(selected))}
                disabled={!selected || running}
              >
                モデルをダウンロード{spec ? `（${sizeLabel(spec.size_mib)}）` : ""}
              </button>
            </div>
          )}
          {modelDownload?.state === "failed" && (
            <div className="notice notice-error setup-notice">{modelDownload.error}</div>
          )}
        </div>
      )}

      {offerGpu && (
        <div className="setup-section">
          <h3>GPU 高速化（NVIDIA・任意）</h3>
          <div className="setup-text">
            {`NVIDIA の GPU が見つかりました。GPU 版の whisper.cpp ${setup.gpu_pack.version} をダウンロードすると、文字起こしが大幅に速くなります。保存後の容量は約 1.2 GB です。`}
            <div className="muted small">GPU 版で失敗した場合は自動で CPU 版に切り替えて処理します。</div>
          </div>
          {gpuDownload?.state === "running" ? (
            <Progress download={gpuDownload} fallbackMib={setup.gpu_pack.size_mib} />
          ) : (
            <div className="download-row">
              <span className="muted small">{gpuDownload?.state === "cancelled" && "キャンセルしました。"}</span>
              <div className="button-row">
                <button
                  type="button"
                  className="btn-small"
                  onClick={() => run(async () => { await setUseGpu(false); await refresh(); })}
                  disabled={running}
                >
                  使わない
                </button>
                <button type="button" className="btn-outline" onClick={() => run(startGpuDownload)} disabled={running}>
                  GPU 版をダウンロード（{sizeLabel(setup.gpu_pack.size_mib)}）
                </button>
              </div>
            </div>
          )}
          {gpuDownload?.state === "failed" && <div className="notice notice-error setup-notice">{gpuDownload.error}</div>}
        </div>
      )}

      {error && <div className="notice notice-error setup-notice">{error}</div>}
    </section>
  );
}
