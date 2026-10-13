import { useCallback, useEffect, useRef, useState } from "react";
import {
  getDevices,
  onProgress,
  startRecording,
  stopRecording,
  type DeviceInfo,
} from "./api";

type Status = "idle" | "starting" | "recording" | "stopping";

function formatElapsed(ms: number): string {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return [h, m, s].map((n) => String(n).padStart(2, "0")).join(":");
}

/** Map a 0..1 peak to 0..100 % on a -60..0 dBFS scale. */
function levelPercent(peak: number): number {
  if (peak <= 0) return 0;
  const db = 20 * Math.log10(peak);
  return Math.max(0, Math.min(100, ((db + 60) / 60) * 100));
}

function describe(d: DeviceInfo | null | undefined): string {
  if (!d) return "見つかりません";
  return d.name;
}

function LevelRow({
  label,
  device,
  level,
}: {
  label: string;
  device: DeviceInfo | null | undefined;
  level: number;
}) {
  const pct = levelPercent(level);
  return (
    <div className="device">
      <div className="device-head">
        <span className="device-label">{label}</span>
        <span className="device-name" title={describe(device)}>
          {describe(device)}
        </span>
      </div>
      {device && (
        <div className="device-format">
          {device.sample_rate} Hz / {device.channels} ch / {device.sample_format}
        </div>
      )}
      <div className="meter" aria-label={`${label}の入力レベル`}>
        <div
          className={`meter-fill${pct > 95 ? " clip" : ""}`}
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}

export default function App() {
  const [status, setStatus] = useState<Status>("idle");
  const [mic, setMic] = useState<DeviceInfo | null>(null);
  const [system, setSystem] = useState<DeviceInfo | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [levels, setLevels] = useState({ mic: 0, system: 0 });
  const [savedDir, setSavedDir] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  // Smoothed meter values so the bars fall back gradually instead of flickering.
  const smooth = useRef({ mic: 0, system: 0 });

  const refreshDevices = useCallback(async () => {
    try {
      const d = await getDevices();
      setMic(d.mic);
      setSystem(d.system);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refreshDevices();
  }, [refreshDevices]);

  useEffect(() => {
    const unlisten = onProgress((p) => {
      setElapsedMs(p.elapsed_ms);
      const decay = 0.8;
      smooth.current = {
        mic: Math.max(p.mic_level, smooth.current.mic * decay),
        system: Math.max(p.system_level, smooth.current.system * decay),
      };
      setLevels(smooth.current);
      if (p.errors.length > 0) {
        setWarnings((w) => [...w, ...p.errors].slice(-5));
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const start = async () => {
    setError(null);
    setWarnings([]);
    setSavedDir(null);
    setElapsedMs(0);
    setStatus("starting");
    try {
      const r = await startRecording();
      setMic(r.mic);
      setSystem(r.system);
      setStatus("recording");
    } catch (e) {
      setError(String(e));
      setStatus("idle");
    }
  };

  const stop = async () => {
    setStatus("stopping");
    try {
      const r = await stopRecording();
      setSavedDir(r.dir);
    } catch (e) {
      setError(String(e));
    } finally {
      smooth.current = { mic: 0, system: 0 };
      setLevels(smooth.current);
      setStatus("idle");
      refreshDevices();
    }
  };

  const recording = status === "recording" || status === "stopping";
  const statusLabel =
    status === "recording" ? "録音中" : status === "starting" ? "開始中…" : status === "stopping" ? "保存中…" : "待機中";

  return (
    <main className="app">
      <section className={`status ${recording ? "is-recording" : ""}`}>
        <span className="status-dot" />
        <span className="status-label">{statusLabel}</span>
        <span className="elapsed">{formatElapsed(elapsedMs)}</span>
      </section>

      <button
        className={`record-button ${recording ? "stop" : "start"}`}
        onClick={recording ? stop : start}
        disabled={status === "starting" || status === "stopping"}
      >
        {recording ? "停止" : "録音開始"}
      </button>

      <section className="devices">
        <LevelRow label="マイク" device={mic} level={levels.mic} />
        <LevelRow label="システム音声" device={system} level={levels.system} />
      </section>

      {savedDir && (
        <section className="saved">
          <div className="saved-label">保存先</div>
          <code className="saved-path">{savedDir}</code>
        </section>
      )}

      {error && <section className="error">{error}</section>}
      {warnings.length > 0 && (
        <section className="warning">
          {warnings.map((w, i) => (
            <div key={i}>{w}</div>
          ))}
        </section>
      )}
    </main>
  );
}
