import { useCallback, useEffect, useRef, useState } from "react";
import {
  getDevices,
  getPaths,
  listMeetings,
  onRecordingProgress,
  onTranscriptionStatus,
  retryTranscription,
  startRecording,
  stopRecording,
  type DeviceInfo,
  type MeetingInfo,
} from "./api";
import { durationLabel, hms, levelToUnit, meetingTitle, shortDateTime } from "./format";

type RecState = "idle" | "starting" | "recording" | "stopping";

const BAR_COUNT = 12;

function LevelBars({ level }: { level: number }) {
  const unit = levelToUnit(level);
  return (
    <div className="bars" aria-hidden="true">
      {Array.from({ length: BAR_COUNT }, (_, i) => (
        <div key={i} className={`bar${unit > i / BAR_COUNT ? " on" : ""}`} style={{ height: 4 + i }} />
      ))}
    </div>
  );
}

function DeviceRow({ label, device, fallback, level }: {
  label: string;
  device: DeviceInfo | null;
  fallback: string;
  level: number;
}) {
  return (
    <div className="device-row">
      <div className="device-label">{label}</div>
      <div className="device-name" title={device ? `${device.sample_rate} Hz / ${device.channels} ch / ${device.sample_format}` : undefined}>
        {device ? device.name : fallback}
      </div>
      <LevelBars level={level} />
    </div>
  );
}

function StatusChip({ meeting, onRetry }: { meeting: MeetingInfo; onRetry: () => void }) {
  const s = meeting.status;
  switch (s.state) {
    case "done":
      return <span className="chip chip-done">完了</span>;
    case "queued":
      return <span className="chip chip-processing">文字起こし待ち</span>;
    case "transcribing":
      return <span className="chip chip-processing">文字起こし中 {s.progress}%</span>;
    case "failed":
      return (
        <>
          <span className="chip chip-failed">失敗</span>
          <button type="button" className="btn-small" onClick={onRetry}>
            再実行
          </button>
        </>
      );
  }
}

function MeetingRow({ meeting, first, onOpen, onRetry }: {
  meeting: MeetingInfo;
  first: boolean;
  onOpen: () => void;
  onRetry: () => void;
}) {
  const title = meetingTitle(meeting.started_at);
  const s = meeting.status;
  return (
    <div className="meeting-row" style={{ borderTop: first ? "none" : undefined }}>
      <div className="meeting-date">{shortDateTime(meeting.started_at)}</div>
      <div className="meeting-title">
        {s.state === "done" ? (
          <a href="#" onClick={(e) => { e.preventDefault(); onOpen(); }}>{title}</a>
        ) : (
          <span>{title}</span>
        )}
      </div>
      <div className="meeting-dur">{durationLabel(meeting.duration_secs)}</div>
      <div className="meeting-status">
        <StatusChip meeting={meeting} onRetry={onRetry} />
      </div>
      {s.state === "failed" && <div className="meeting-error">{s.error}</div>}
    </div>
  );
}

export default function MainView({ onOpen }: { onOpen: (dir: string) => void }) {
  const [rec, setRec] = useState<RecState>("idle");
  const [mic, setMic] = useState<DeviceInfo | null>(null);
  const [system, setSystem] = useState<DeviceInfo | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [levels, setLevels] = useState({ mic: 0, system: 0 });
  const [outputRoot, setOutputRoot] = useState("meetings/");
  const [meetings, setMeetings] = useState<MeetingInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  // Meter values decay gradually instead of flickering between events.
  const smooth = useRef({ mic: 0, system: 0 });

  const upsertMeeting = useCallback((m: MeetingInfo) => {
    setMeetings((list) => {
      // Events and command results can arrive out of order; never go back to an older status.
      const current = list.find((x) => x.dir === m.dir);
      if (current && current.seq > m.seq) return list;
      const rest = list.filter((x) => x.dir !== m.dir);
      return [m, ...rest].sort((a, b) => b.started_at.localeCompare(a.started_at));
    });
  }, []);

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
    getPaths().then((p) => setOutputRoot(p.output_root)).catch(() => {});
    listMeetings().then(setMeetings).catch(() => {});
  }, [refreshDevices]);

  useEffect(() => {
    const unlistenProgress = onRecordingProgress((p) => {
      setElapsedMs(p.elapsed_ms);
      const decay = 0.8;
      smooth.current = {
        mic: Math.max(p.mic_level, smooth.current.mic * decay),
        system: Math.max(p.system_level, smooth.current.system * decay),
      };
      setLevels(smooth.current);
      if (p.errors.length > 0) setWarnings((w) => [...w, ...p.errors].slice(-5));
    });
    const unlistenStatus = onTranscriptionStatus(upsertMeeting);
    return () => {
      unlistenProgress.then((f) => f());
      unlistenStatus.then((f) => f());
    };
  }, [upsertMeeting]);

  const start = async () => {
    setError(null);
    setWarnings([]);
    setElapsedMs(0);
    setRec("starting");
    try {
      const r = await startRecording();
      setMic(r.mic);
      setSystem(r.system);
      setRec("recording");
    } catch (e) {
      setError(String(e));
      setRec("idle");
    }
  };

  const stop = async () => {
    setRec("stopping");
    try {
      upsertMeeting(await stopRecording());
    } catch (e) {
      setError(String(e));
    } finally {
      smooth.current = { mic: 0, system: 0 };
      setLevels(smooth.current);
      setElapsedMs(0);
      setRec("idle");
      refreshDevices();
    }
  };

  const retry = async (dir: string) => {
    try {
      upsertMeeting(await retryTranscription(dir));
    } catch (e) {
      setError(String(e));
    }
  };

  const recording = rec === "recording" || rec === "stopping";

  return (
    <div className="page">
      <header className="header">
        <div className="header-inner">
          <div className="brand">
            <div className="brand-name">会議ログ</div>
            <div className="brand-note">文字起こしはローカル処理</div>
          </div>
          <div className="mono muted header-path" title={outputRoot}>
            保存先 {outputRoot}
          </div>
        </div>
      </header>

      <main className="main">
        <section aria-label="録音" className="card recorder">
          <div className="recorder-clock">
            {recording ? (
              <div className="rec-state rec-state-on">
                <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><circle cx="5" cy="5" r="5" fill="#B42318" /></svg>
                {rec === "stopping" ? "保存中…" : "録音中"}
              </div>
            ) : (
              <div className="rec-state">
                <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><circle cx="5" cy="5" r="4" fill="none" stroke="#555B66" strokeWidth="2" /></svg>
                {rec === "starting" ? "開始中…" : "待機中"}
              </div>
            )}
            <div className="elapsed">{hms(elapsedMs)}</div>
          </div>

          <div className="recorder-devices">
            <DeviceRow label="マイク" device={mic} fallback="既定の入力デバイスが見つかりません" level={levels.mic} />
            <DeviceRow label="システム音声" device={system} fallback="既定の出力デバイスが見つかりません" level={levels.system} />
          </div>

          <div className="recorder-action">
            {recording ? (
              <button type="button" className="btn-rec btn-stop" onClick={stop} disabled={rec === "stopping"}>
                <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><rect width="12" height="12" rx="2" fill="#FFFFFF" /></svg>
                停止して処理
              </button>
            ) : (
              <button type="button" className="btn-rec btn-start" onClick={start} disabled={rec === "starting"}>
                <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true"><circle cx="7" cy="7" r="6" fill="#F97066" /></svg>
                録音開始
              </button>
            )}
            <div className="muted small">停止後、文字起こしを自動で実行</div>
          </div>
        </section>

        {error && <div className="notice notice-error">{error}</div>}
        {warnings.length > 0 && (
          <div className="notice notice-warning">
            {warnings.map((w, i) => <div key={i}>{w}</div>)}
          </div>
        )}

        {meetings.length > 0 && (
          <section aria-label="今回の録音" className="meetings">
            <div className="section-head">
              <h2>今回の録音</h2>
              <div className="muted small">新しい順</div>
            </div>
            <div className="card meeting-list">
              {meetings.map((m, i) => (
                <MeetingRow
                  key={m.dir}
                  meeting={m}
                  first={i === 0}
                  onOpen={() => onOpen(m.dir)}
                  onRetry={() => retry(m.dir)}
                />
              ))}
            </div>
          </section>
        )}
      </main>
    </div>
  );
}
