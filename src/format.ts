const pad = (n: number) => String(n).padStart(2, "0");

/** 3723000 → "01:02:03" */
export function hms(ms: number): string {
  const total = Math.floor(ms / 1000);
  return `${pad(Math.floor(total / 3600))}:${pad(Math.floor(total / 60) % 60)}:${pad(total % 60)}`;
}

/** Duration as shown in lists: "42分", "35秒", or "—" while unknown. */
export function durationLabel(secs: number | null): string {
  if (secs == null) return "—";
  if (secs < 60) return `${Math.round(secs)}秒`;
  return `${Math.round(secs / 60)}分`;
}

/** "10/13 10:30" */
export function shortDateTime(iso: string): string {
  const d = new Date(iso);
  return `${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const WEEKDAYS = ["日", "月", "火", "水", "木", "金", "土"];

/** "2026-10-13 (火) 10:30–11:12 · 42分" */
export function meetingSpan(iso: string, secs: number | null): string {
  const d = new Date(iso);
  const day = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} (${WEEKDAYS[d.getDay()]})`;
  const start = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (secs == null) return `${day} ${start}`;
  const e = new Date(d.getTime() + secs * 1000);
  return `${day} ${start}–${pad(e.getHours())}:${pad(e.getMinutes())} · ${durationLabel(secs)}`;
}

/** Placeholder title until summaries provide one: "10月13日 10:30 の会議" */
export function meetingTitle(iso: string): string {
  const d = new Date(iso);
  return `${d.getMonth() + 1}月${d.getDate()}日 ${pad(d.getHours())}:${pad(d.getMinutes())} の会議`;
}

/** Map a 0..1 peak to 0..1 on a -60..0 dBFS scale. */
export function levelToUnit(peak: number): number {
  if (peak <= 0) return 0;
  return Math.max(0, Math.min(1, (20 * Math.log10(peak) + 60) / 60));
}
