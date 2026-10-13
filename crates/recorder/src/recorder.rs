//! Simultaneous mic + system-audio recording into `<root>/<YYYY-MM-DD_HHMM>/{mic,system}.wav`.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamInstant};
use serde::Serialize;

use crate::devices::{device_name, open_source, DeviceInfo, SourceKind};
use crate::dsp::{downmix_into, f32_to_i16, peak};
use crate::timeline::{SampleSink, Timeline};

/// Output format expected by whisper.cpp.
pub const OUTPUT_SAMPLE_RATE: u32 = 16_000;

/// Peak level (0.0 - 1.0) since the last read, shared lock-free with the audio callback.
#[derive(Default)]
pub struct LevelMeter(AtomicU32);

impl LevelMeter {
    fn update(&self, peak: f32) {
        let _ = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                (peak > f32::from_bits(cur)).then_some(peak.to_bits())
            });
    }

    /// Return the peak seen since the previous call and reset it.
    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Ordering::Relaxed))
    }
}

/// Live values for the UI. Cheap to clone and safe to poll from any thread.
#[derive(Clone)]
pub struct Meters {
    pub mic: Arc<LevelMeter>,
    pub system: Arc<LevelMeter>,
    started: Instant,
    errors: Arc<Mutex<Vec<String>>>,
}

impl Meters {
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Stream errors reported by the audio backend since the last call.
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.errors.lock().unwrap())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackSummary {
    pub kind: SourceKind,
    pub path: PathBuf,
    pub device: DeviceInfo,
    /// Seconds of silence inserted where the device delivered no data.
    pub inserted_silence_secs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingSummary {
    pub dir: PathBuf,
    pub started_at: DateTime<Local>,
    pub duration_secs: f64,
    pub mic: TrackSummary,
    pub system: TrackSummary,
}

enum Msg {
    Block {
        capture: StreamInstant,
        samples: Vec<f32>,
    },
    Stop(StreamInstant),
}

struct Source {
    kind: SourceKind,
    device: DeviceInfo,
    stream: cpal::Stream,
    tx: Sender<Msg>,
    start_tx: Option<Sender<StreamInstant>>,
    writer: JoinHandle<Result<f64>>,
}

/// An active recording. Dropping it without calling [`Recorder::stop`] discards nothing that was
/// already written, but leaves the WAV headers as of the last periodic flush.
pub struct Recorder {
    dir: PathBuf,
    started_at: DateTime<Local>,
    mic: Source,
    system: Source,
    meters: Meters,
}

impl Recorder {
    /// Start recording the default mic and the default output (loopback) into a new
    /// `<output_root>/<YYYY-MM-DD_HHMM>` directory.
    pub fn start(output_root: impl AsRef<Path>) -> Result<Self> {
        let host = cpal::default_host();
        let started_at = Local::now();
        let dir = create_session_dir(output_root.as_ref(), &started_at)?;
        let errors = Arc::new(Mutex::new(Vec::new()));
        let mic_meter = Arc::new(LevelMeter::default());
        let system_meter = Arc::new(LevelMeter::default());

        let fail = |e: anyhow::Error, sources: Vec<Source>| {
            for source in sources {
                source.abort();
            }
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        };
        let mut mic = match Source::open(
            &host,
            SourceKind::Mic,
            &dir,
            mic_meter.clone(),
            errors.clone(),
        ) {
            Ok(s) => s,
            Err(e) => return fail(e, vec![]),
        };
        let mut system = match Source::open(
            &host,
            SourceKind::System,
            &dir,
            system_meter.clone(),
            errors.clone(),
        ) {
            Ok(s) => s,
            Err(e) => return fail(e, vec![mic]),
        };
        // Start both streams back to back, then define t0 on each stream's clock. Anything
        // captured before t0 is trimmed, so both files begin at the same instant.
        let played = mic
            .stream
            .play()
            .context("マイクの録音を開始できません")
            .and_then(|_| {
                system
                    .stream
                    .play()
                    .context("システム音声の録音を開始できません")
            });
        if let Err(e) = played {
            return fail(e, vec![mic, system]);
        }
        let started = Instant::now();
        mic.begin();
        system.begin();

        let recorder = Self {
            dir,
            started_at,
            mic,
            system,
            meters: Meters {
                mic: mic_meter,
                system: system_meter,
                started,
                errors,
            },
        };
        recorder.write_meta(None)?;
        Ok(recorder)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn meters(&self) -> Meters {
        self.meters.clone()
    }

    pub fn mic_device(&self) -> &DeviceInfo {
        &self.mic.device
    }

    pub fn system_device(&self) -> &DeviceInfo {
        &self.system.device
    }

    /// Stop both streams, pad both tracks to the same end instant and finalize the files.
    pub fn stop(self) -> Result<RecordingSummary> {
        let Recorder {
            dir,
            started_at,
            mic,
            system,
            meters,
        } = self;
        let devices = (mic.device.clone(), system.device.clone());
        let duration_secs = meters.elapsed().as_secs_f64();
        let mic = mic.finish(&dir);
        let system = system.finish(&dir);
        let summary = RecordingSummary {
            dir,
            started_at,
            duration_secs,
            mic: mic?,
            system: system?,
        };
        write_meta_file(&summary.dir, &started_at, &devices, Some(&summary))?;
        Ok(summary)
    }

    fn write_meta(&self, summary: Option<&RecordingSummary>) -> Result<()> {
        let devices = (self.mic.device.clone(), self.system.device.clone());
        write_meta_file(&self.dir, &self.started_at, &devices, summary)
    }
}

impl Source {
    fn open(
        host: &cpal::Host,
        kind: SourceKind,
        dir: &Path,
        meter: Arc<LevelMeter>,
        errors: Arc<Mutex<Vec<String>>>,
    ) -> Result<Self> {
        let (device, supported) = open_source(host, kind)?;
        let name = device_name(&device);
        let config: cpal::StreamConfig = supported.config();
        let info = DeviceInfo {
            name: name.clone(),
            sample_rate: config.sample_rate,
            channels: config.channels,
            sample_format: format!("{:?}", supported.sample_format()),
        };

        let (tx, rx) = mpsc::channel();
        let (start_tx, start_rx) = mpsc::channel();
        let path = dir.join(kind.file_name());
        let wav = WavSink::create(&path)?;
        let in_rate = config.sample_rate;
        let writer = thread::Builder::new()
            .name(format!("{kind:?}-writer"))
            .spawn(move || write_track(in_rate, wav, start_rx, rx))
            .context("書き込みスレッドを起動できません")?;

        let label = match kind {
            SourceKind::Mic => "マイク",
            SourceKind::System => "システム音声",
        };
        let err_label = format!("{label}「{name}」");
        let on_error = move |e: cpal::Error| {
            errors.lock().unwrap().push(format!("{err_label}: {e}"));
        };
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config, tx.clone(), meter, on_error),
            SampleFormat::F64 => build::<f64>(&device, config, tx.clone(), meter, on_error),
            SampleFormat::I16 => build::<i16>(&device, config, tx.clone(), meter, on_error),
            SampleFormat::I32 => build::<i32>(&device, config, tx.clone(), meter, on_error),
            SampleFormat::I24 => build::<cpal::I24>(&device, config, tx.clone(), meter, on_error),
            SampleFormat::U16 => build::<u16>(&device, config, tx.clone(), meter, on_error),
            SampleFormat::U8 => build::<u8>(&device, config, tx.clone(), meter, on_error),
            SampleFormat::I8 => build::<i8>(&device, config, tx.clone(), meter, on_error),
            other => Err(anyhow!("未対応のサンプル形式です: {other:?}")),
        }
        .with_context(|| format!("{label}「{name}」の録音ストリームを作成できません"))?;

        Ok(Self {
            kind,
            device: info,
            stream,
            tx,
            start_tx: Some(start_tx),
            writer,
        })
    }

    /// Fix t0 on this stream's clock and let the writer start consuming blocks.
    fn begin(&mut self) {
        if let Some(start_tx) = self.start_tx.take() {
            let _ = start_tx.send(self.stream.now());
        }
    }

    /// Tear down a source that never started; waits for the writer so its file is closed.
    fn abort(self) {
        let Source {
            stream,
            tx,
            start_tx,
            writer,
            ..
        } = self;
        drop(stream);
        drop(start_tx);
        drop(tx);
        let _ = writer.join();
    }

    fn finish(self, dir: &Path) -> Result<TrackSummary> {
        let Source {
            kind,
            device,
            stream,
            tx,
            writer,
            ..
        } = self;
        let end = stream.now();
        let _ = stream.pause();
        // Dropping the stream stops callbacks, so `Stop` is guaranteed to be the last message.
        drop(stream);
        let _ = tx.send(Msg::Stop(end));
        drop(tx);
        let inserted = writer
            .join()
            .map_err(|_| anyhow!("書き込みスレッドが異常終了しました"))?
            .with_context(|| format!("{} の書き込みに失敗しました", kind.file_name()))?;
        Ok(TrackSummary {
            kind,
            path: dir.join(kind.file_name()),
            device,
            inserted_silence_secs: inserted,
        })
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    tx: Sender<Msg>,
    meter: Arc<LevelMeter>,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let mut converted: Vec<f32> = Vec::new();
    let stream = device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            converted.clear();
            converted.extend(
                data.iter()
                    .map(|&s| <f32 as FromSample<T>>::from_sample_(s)),
            );
            let mut mono = Vec::with_capacity(converted.len() / channels.max(1));
            downmix_into(&converted, channels, &mut mono);
            meter.update(peak(&mono));
            let _ = tx.send(Msg::Block {
                capture: info.timestamp().capture,
                samples: mono,
            });
        },
        on_error,
        None,
    )?;
    Ok(stream)
}

/// Signed seconds from `t0` to `t` on the same stream clock.
fn offset_secs(t: StreamInstant, t0: StreamInstant) -> f64 {
    match t.checked_duration_since(t0) {
        Some(d) => d.as_secs_f64(),
        None => -t0.duration_since(t).as_secs_f64(),
    }
}

/// Writer thread: align, resample and write one track. Returns seconds of inserted silence.
fn write_track(
    in_rate: u32,
    wav: WavSink,
    start_rx: Receiver<StreamInstant>,
    rx: Receiver<Msg>,
) -> Result<f64> {
    let mut timeline = Timeline::new(in_rate, OUTPUT_SAMPLE_RATE, wav);
    // If start never arrives the recorder failed to start; just finalize an empty file.
    let Ok(t0) = start_rx.recv() else {
        timeline.finish(0.0)?.finalize()?;
        return Ok(0.0);
    };
    let mut end = None;
    for msg in rx {
        match msg {
            Msg::Block { capture, samples } => {
                timeline.push(offset_secs(capture, t0), &samples)?;
            }
            Msg::Stop(t) => {
                end = Some(offset_secs(t, t0));
                break;
            }
        }
    }
    let end = end.unwrap_or_else(|| timeline.placed_secs());
    let inserted = timeline.inserted_frames as f64 / in_rate as f64;
    timeline.finish(end)?.finalize()?;
    Ok(inserted)
}

/// 16 kHz / mono / 16-bit PCM WAV writer that refreshes the header about once a second so the
/// file stays playable even if the app is killed mid-recording.
struct WavSink {
    writer: hound::WavWriter<BufWriter<File>>,
    since_flush: usize,
}

impl WavSink {
    fn create(path: &Path) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: OUTPUT_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, spec)
            .with_context(|| format!("{} を作成できません", path.display()))?;
        Ok(Self {
            writer,
            since_flush: 0,
        })
    }

    fn finalize(self) -> Result<()> {
        self.writer.finalize()?;
        Ok(())
    }
}

impl SampleSink for WavSink {
    fn write(&mut self, samples: &[f32]) -> Result<()> {
        for &s in samples {
            self.writer.write_sample(f32_to_i16(s))?;
        }
        self.since_flush += samples.len();
        if self.since_flush >= OUTPUT_SAMPLE_RATE as usize {
            self.writer.flush()?;
            self.since_flush = 0;
        }
        Ok(())
    }
}

/// Create `<root>/<YYYY-MM-DD_HHMM>`, adding `_2`, `_3`, ... if it already exists.
fn create_session_dir(root: &Path, at: &DateTime<Local>) -> Result<PathBuf> {
    fs::create_dir_all(root).with_context(|| format!("{} を作成できません", root.display()))?;
    let stamp = at.format("%Y-%m-%d_%H%M").to_string();
    for n in 1.. {
        let name = if n == 1 {
            stamp.clone()
        } else {
            format!("{stamp}_{n}")
        };
        let dir = root.join(name);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("{} を作成できません", dir.display())),
        }
    }
    unreachable!()
}

/// `meta.json`: start time and devices, needed later to merge the two tracks by timestamp.
fn write_meta_file(
    dir: &Path,
    started_at: &DateTime<Local>,
    devices: &(DeviceInfo, DeviceInfo),
    summary: Option<&RecordingSummary>,
) -> Result<()> {
    let meta = serde_json::json!({
        "started_at": started_at.to_rfc3339(),
        "sample_rate": OUTPUT_SAMPLE_RATE,
        "channels": 1,
        "bits_per_sample": 16,
        "duration_secs": summary.map(|s| s.duration_secs),
        "tracks": {
            "mic": {
                "file": SourceKind::Mic.file_name(),
                "device": devices.0,
                "inserted_silence_secs": summary.map(|s| s.mic.inserted_silence_secs),
            },
            "system": {
                "file": SourceKind::System.file_name(),
                "device": devices.1,
                "inserted_silence_secs": summary.map(|s| s.system.inserted_silence_secs),
            },
        },
    });
    let path = dir.join("meta.json");
    fs::write(&path, serde_json::to_vec_pretty(&meta)?)
        .with_context(|| format!("{} を書き込めません", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_dir_gets_suffix_when_taken() {
        let root = std::env::temp_dir().join(format!("recorder-test-{}", std::process::id()));
        let at = Local::now();
        let a = create_session_dir(&root, &at).unwrap();
        let b = create_session_dir(&root, &at).unwrap();
        assert_ne!(a, b);
        assert!(b.file_name().unwrap().to_str().unwrap().ends_with("_2"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn level_meter_keeps_max_and_resets() {
        let m = LevelMeter::default();
        m.update(0.2);
        m.update(0.7);
        m.update(0.1);
        assert_eq!(m.take(), 0.7);
        assert_eq!(m.take(), 0.0);
    }

    #[test]
    fn wav_sink_writes_whisper_format() {
        let path = std::env::temp_dir().join(format!("recorder-wav-{}.wav", std::process::id()));
        let mut t = Timeline::new(48_000, OUTPUT_SAMPLE_RATE, WavSink::create(&path).unwrap());
        t.push(0.0, &vec![0.25; 48_000]).unwrap();
        t.finish(2.0).unwrap().finalize().unwrap();
        let reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(
            (spec.sample_rate, spec.channels, spec.bits_per_sample),
            (16_000, 1, 16)
        );
        assert_eq!(reader.duration(), 32_000);
        fs::remove_file(&path).unwrap();
    }
}
