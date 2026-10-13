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

export interface StartedRecording {
  dir: string;
  mic: DeviceInfo;
  system: DeviceInfo;
}

export interface StoppedRecording {
  dir: string;
  duration_secs: number;
}

/** Payload of the `recording-progress` event (sent ~10 times per second while recording). */
export interface RecordingProgress {
  elapsed_ms: number;
  /** Peak level since the previous event, 0.0 - 1.0. */
  mic_level: number;
  system_level: number;
  errors: string[];
}

export const getDevices = () => invoke<DefaultDevices>("get_devices");
export const startRecording = () => invoke<StartedRecording>("start_recording");
export const stopRecording = () => invoke<StoppedRecording>("stop_recording");

export const onProgress = (cb: (p: RecordingProgress) => void): Promise<UnlistenFn> =>
  listen<RecordingProgress>("recording-progress", (e) => cb(e.payload));
