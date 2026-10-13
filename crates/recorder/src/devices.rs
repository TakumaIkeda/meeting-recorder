//! Device discovery: the default microphone and the default output used for loopback.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

/// Which audio source a track records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// Default input device (microphone).
    Mic,
    /// Default output device captured via WASAPI loopback.
    System,
}

impl SourceKind {
    pub fn file_name(self) -> &'static str {
        match self {
            SourceKind::Mic => "mic.wav",
            SourceKind::System => "system.wav",
        }
    }
}

/// A device together with the native format we will capture it in.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DefaultDevices {
    pub host: String,
    pub mic: Option<DeviceInfo>,
    pub system: Option<DeviceInfo>,
}

/// One entry of [`list_all_devices`].
#[derive(Debug, Clone, Serialize)]
pub struct DeviceEntry {
    pub name: String,
    pub is_default_input: bool,
    pub is_default_output: bool,
    pub input: Option<DeviceInfo>,
    pub output: Option<DeviceInfo>,
}

pub(crate) fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| device.to_string())
}

fn info(device: &cpal::Device, config: &cpal::SupportedStreamConfig) -> DeviceInfo {
    DeviceInfo {
        name: device_name(device),
        sample_rate: config.sample_rate(),
        channels: config.channels(),
        sample_format: format!("{:?}", config.sample_format()),
    }
}

/// Resolve the device and its native config for a source.
///
/// For [`SourceKind::System`] this is the default *output* device and its output (mix) format;
/// on WASAPI, opening an input stream on an output device enables loopback capture.
pub(crate) fn open_source(
    host: &cpal::Host,
    kind: SourceKind,
) -> Result<(cpal::Device, cpal::SupportedStreamConfig)> {
    match kind {
        SourceKind::Mic => {
            let device = host
                .default_input_device()
                .ok_or_else(|| anyhow!("既定の入力デバイス（マイク）が見つかりません"))?;
            let config = device.default_input_config().with_context(|| {
                format!(
                    "マイク「{}」の既定フォーマットを取得できません",
                    device_name(&device)
                )
            })?;
            Ok((device, config))
        }
        SourceKind::System => {
            if !cfg!(windows) {
                return Err(anyhow!(
                    "システム音声（ループバック）の録音は Windows (WASAPI) でのみ対応しています"
                ));
            }
            let device = host
                .default_output_device()
                .ok_or_else(|| anyhow!("既定の出力デバイスが見つかりません"))?;
            let config = device.default_output_config().with_context(|| {
                format!(
                    "出力デバイス「{}」の既定フォーマットを取得できません",
                    device_name(&device)
                )
            })?;
            Ok((device, config))
        }
    }
}

/// Default mic / output devices with their native formats. Missing devices are `None`.
pub fn default_devices() -> DefaultDevices {
    let host = cpal::default_host();
    let probe = |kind| open_source(&host, kind).ok().map(|(d, c)| info(&d, &c));
    DefaultDevices {
        host: host.id().name().to_string(),
        mic: probe(SourceKind::Mic),
        system: probe(SourceKind::System),
    }
}

/// Every device of the default host, for diagnostics.
pub fn list_all_devices() -> Result<Vec<DeviceEntry>> {
    let host = cpal::default_host();
    let default_in = host.default_input_device();
    let default_out = host.default_output_device();
    let mut entries = Vec::new();
    for device in host.devices().context("デバイス一覧を取得できません")? {
        entries.push(DeviceEntry {
            name: device_name(&device),
            is_default_input: default_in.as_ref() == Some(&device),
            is_default_output: default_out.as_ref() == Some(&device),
            input: device
                .default_input_config()
                .ok()
                .map(|c| info(&device, &c)),
            output: device
                .default_output_config()
                .ok()
                .map(|c| info(&device, &c)),
        });
    }
    Ok(entries)
}

/// Result of [`probe`]: what a short capture from a source actually delivered.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub device: DeviceInfo,
    pub callbacks: u64,
    pub frames: u64,
    pub peak: f32,
    pub errors: Vec<String>,
}

/// Capture from a source for `duration` and report how much data arrived.
///
/// Useful to check that WASAPI loopback works on a machine: play some audio while probing
/// [`SourceKind::System`]. Note that loopback delivers no data at all while nothing is playing.
pub fn probe(kind: SourceKind, duration: std::time::Duration) -> Result<ProbeResult> {
    use cpal::traits::StreamTrait;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Stats {
        callbacks: u64,
        frames: u64,
        peak: f32,
        errors: Vec<String>,
    }

    let host = cpal::default_host();
    let (device, supported) = open_source(&host, kind)?;
    let config = supported.config();
    let channels = config.channels as usize;
    let stats = Arc::new(Mutex::new(Stats::default()));
    let (s_data, s_err) = (stats.clone(), stats.clone());
    let on_error = move |e: cpal::Error| s_err.lock().unwrap().errors.push(e.to_string());

    // Probing only needs f32; ask the backend for it if the native format differs.
    if supported.sample_format() != cpal::SampleFormat::F32 {
        return Err(anyhow!(
            "probe は f32 形式のデバイスのみ対応です（このデバイスは {:?}）",
            supported.sample_format()
        ));
    }
    let stream = device.build_input_stream::<f32, _, _>(
        config,
        move |data: &[f32], _| {
            let mut s = s_data.lock().unwrap();
            s.callbacks += 1;
            s.frames += (data.len() / channels.max(1)) as u64;
            s.peak = data.iter().fold(s.peak, |p, x| p.max(x.abs()));
        },
        on_error,
        None,
    )?;
    stream.play()?;
    std::thread::sleep(duration);
    drop(stream);

    let s = std::mem::take(&mut *stats.lock().unwrap());
    Ok(ProbeResult {
        device: info(&device, &supported),
        callbacks: s.callbacks,
        frames: s.frames,
        peak: s.peak,
        errors: s.errors,
    })
}
