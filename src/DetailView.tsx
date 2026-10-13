import { useEffect, useState } from "react";
import { getMeeting, openFolder, type MeetingDetail, type TranscriptLine } from "./api";
import { hms, meetingSpan, meetingTitle } from "./format";

const SPEAKERS = {
  me: { who: "自分", color: "#15171C" },
  them: { who: "相手", color: "#2342A8" },
} as const;

const lineId = (l: TranscriptLine) => `t-${hms(l.start_ms).replace(/:/g, "")}`;

function transcriptText(lines: TranscriptLine[]): string {
  return lines.map((l) => `[${hms(l.start_ms)}] ${SPEAKERS[l.speaker].who}: ${l.text}`).join("\n");
}

function SummaryBlock({ title }: { title: string }) {
  return (
    <div className="summary-block">
      <h2>{title}</h2>
      <div className="summary-empty">まだありません</div>
    </div>
  );
}

export default function DetailView({ dir, onBack }: { dir: string; onBack: () => void }) {
  const [detail, setDetail] = useState<MeetingDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    getMeeting(dir).then(setDetail).catch((e) => setError(String(e)));
  }, [dir]);

  const copy = async () => {
    if (!detail) return;
    try {
      await navigator.clipboard.writeText(transcriptText(detail.lines));
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch (e) {
      setError(String(e));
    }
  };

  const open = () => openFolder(dir).catch((e) => setError(String(e)));

  return (
    <div className="page">
      <header className="header">
        <div className="header-inner">
          <a href="#" className="back" onClick={(e) => { e.preventDefault(); onBack(); }}>
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M10 3L5 8l5 5" /></svg>
            会議一覧
          </a>
          <div className="header-actions">
            <button type="button" className="btn-outline" onClick={copy} disabled={!detail}>
              {copied ? "コピーしました" : "全文をコピー"}
            </button>
            <button type="button" className="btn-outline" onClick={open}>
              フォルダを開く
            </button>
          </div>
        </div>
      </header>

      <main className="main detail">
        {error && <div className="notice notice-error">{error}</div>}
        {detail && (
          <>
            <div className="detail-title">
              <h1>{meetingTitle(detail.info.started_at)}</h1>
              <div className="mono muted">{meetingSpan(detail.info.started_at, detail.info.duration_secs)}</div>
            </div>

            <div className="detail-columns">
              <section aria-label="要約" className="card summary">
                <SummaryBlock title="自分のTODO" />
                <SummaryBlock title="決定事項" />
                <SummaryBlock title="持ち越し" />
                <div className="summary-note">要約は次のバージョンで追加予定です。</div>
              </section>

              <section aria-label="全文" className="card transcript">
                <div className="transcript-head">
                  <h2>全文</h2>
                  <div className="legend">
                    <span><svg width="8" height="8" viewBox="0 0 8 8" aria-hidden="true"><circle cx="4" cy="4" r="4" fill="#15171C" /></svg>自分（マイク）</span>
                    <span><svg width="8" height="8" viewBox="0 0 8 8" aria-hidden="true"><circle cx="4" cy="4" r="4" fill="#2342A8" /></svg>相手（システム音声）</span>
                  </div>
                </div>
                {detail.lines.length === 0 && <div className="transcript-empty">発言は検出されませんでした。</div>}
                {detail.lines.map((l, i) => (
                  <div key={i} id={lineId(l)} className="line" style={{ background: i % 2 === 1 ? "#FAFAFB" : "#FFFFFF" }}>
                    <div className="line-ts">{hms(l.start_ms)}</div>
                    <div className="line-who" style={{ color: SPEAKERS[l.speaker].color }}>{SPEAKERS[l.speaker].who}</div>
                    <div className="line-text">{l.text}</div>
                  </div>
                ))}
              </section>
            </div>
          </>
        )}
      </main>
    </div>
  );
}
