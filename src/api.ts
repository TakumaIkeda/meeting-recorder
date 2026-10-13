import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface DeviceInfo {
  name: string;
  sample_rate: number;
  channels: number;
  sample_format: string;
}

export interface DefaultDevices {
  host: string;
  mic: DeviceInfo | null;
  system: DeviceInfo | null;
}

export interface Paths {
  output_root: string;
  config: string;
  models: string;
}

export interface StartedRecording {
  dir: string;
  mic: DeviceInfo;
  system: DeviceInfo;
}

export type JobStatus =
  | { state: "queued" }
  | { state: "transcribing"; progress: number }
  | { state: "done" }
  | { state: "failed"; error: string };

export interface MeetingInfo {
  dir: string;
  /** RFC 3339 with offset. */
  started_at: string;
  duration_secs: number | null;
  status: JobStatus;
  /** Increases with every status change; keep the copy with the highest value. */
  seq: number;
}

export interface TranscriptLine {
  start_ms: number;
  end_ms: number;
  speaker: "me" | "them";
  text: string;
}

export interface MeetingDetail {
  info: MeetingInfo;
  lines: TranscriptLine[];
}

/** Payload of `recording-progress` (about every 100 ms while recording). */
export interface RecordingProgress {
  elapsed_ms: number;
  /** Peak level since the previous event, 0.0 - 1.0. */
  mic_level: number;
  system_level: number;
  errors: string[];
}

export const getPaths = () => invoke<Paths>("get_paths");
export const getDevices = () => invoke<DefaultDevices>("get_devices");
export const startRecording = () => invoke<StartedRecording>("start_recording");
export const stopRecording = () => invoke<MeetingInfo>("stop_recording");
export const retryTranscription = (dir: string) => invoke<MeetingInfo>("retry_transcription", { dir });
export const listMeetings = () => invoke<MeetingInfo[]>("list_meetings");
export const getMeeting = (dir: string) => invoke<MeetingDetail>("get_meeting", { dir });
export const openFolder = (dir: string) => invoke<void>("open_folder", { dir });

export const onRecordingProgress = (cb: (p: RecordingProgress) => void): Promise<UnlistenFn> =>
  listen<RecordingProgress>("recording-progress", (e) => cb(e.payload));

export const onTranscriptionStatus = (cb: (m: MeetingInfo) => void): Promise<UnlistenFn> =>
  listen<MeetingInfo>("transcription-status", (e) => cb(e.payload));
