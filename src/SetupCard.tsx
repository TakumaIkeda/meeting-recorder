import { useEffect, useState } from "react";
import {
  cancelModelDownload,
  getSetup,
  onModelDownload,
  startModelDownload,
  type DownloadState,
  type SetupStatus,
} from "./api";

const mb = (bytes: number) => Math.round(bytes / (1024 * 1024));

/** Shown on the main screen until whisper-cli and a model are available. */
export default function SetupCard() {
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
  if (setup.model_found && setup.cli_found) return null;

  const running = download?.state === "running";
  const spec = setup.models.find((m) => m.id === (running ? download.id : selected));
  const total = download?.total ?? (spec ? spec.size_mib * 1024 * 1024 : 0);
  const pct = running && total > 0 ? Math.min(100, (download.downloaded / total) * 100) : 0;

  const start = async () => {
    if (!selected) return;
    setError(null);
    try {
      await startModelDownload(selected);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <section aria-label="文字起こしの準備" className="card setup">
      <div className="setup-head">
        <h2>文字起こしの準備</h2>
        <div className="muted small">録音はこのままでも行えます。文字起こしには以下が必要です。</div>
      </div>

      {!setup.cli_found && (
        <div className="notice notice-error setup-notice">
          whisper-cli が見つかりません: {setup.cli_path}
          {"\n"}`npm run fetch-whisper` を実行してからビルドし直すか、{setup.config_path} の "whisper_cli" にパスを指定してください。
        </div>
      )}

      {!setup.model_found && (
        <>
          <div className="setup-text">
            文字起こしモデルをダウンロードします（初回のみ）。保存先 <span className="mono">{setup.models_dir}</span>
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
                <span className="model-size mono">{m.size_mib >= 1024 ? `${(m.size_mib / 1024).toFixed(1)} GB` : `${m.size_mib} MB`}</span>
                <span className="model-note">{m.note}</span>
              </label>
            ))}
          </div>

          {running ? (
            <div className="download">
              <div className="download-bar">
                <div className="download-fill" style={{ width: `${pct}%` }} />
              </div>
              <div className="download-row">
                <span className="mono muted small">
                  {mb(download.downloaded)} / {mb(total)} MB
                </span>
                <button type="button" className="btn-small" onClick={() => cancelModelDownload()}>
                  キャンセル
                </button>
              </div>
            </div>
          ) : (
            <div className="download-row">
              <span className="muted small">
                {download?.state === "cancelled" && "キャンセルしました。"}
              </span>
              <button type="button" className="btn-outline" onClick={start} disabled={!selected}>
                モデルをダウンロード{spec ? `（${spec.size_mib >= 1024 ? `${(spec.size_mib / 1024).toFixed(1)} GB` : `${spec.size_mib} MB`}）` : ""}
              </button>
            </div>
          )}
          {download?.state === "failed" && <div className="notice notice-error setup-notice">{download.error}</div>}
        </>
      )}
      {error && <div className="notice notice-error setup-notice">{error}</div>}
    </section>
  );
}
