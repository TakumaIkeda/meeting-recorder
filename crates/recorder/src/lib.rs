//! GUI-independent recording module.
//!
//! Records the default microphone and the default output device (WASAPI loopback) at the same
//! time and writes `mic.wav` / `system.wav` as 16 kHz, mono, 16-bit PCM (ready for whisper.cpp).
//! Both files share the same start instant, so sample `n` in each corresponds to the same time.

pub mod devices;
pub mod dsp;
pub mod recorder;
pub mod timeline;

pub use devices::{
    default_devices, list_all_devices, DefaultDevices, DeviceEntry, DeviceInfo, SourceKind,
};
pub use recorder::{Meters, Recorder, RecordingSummary, TrackSummary, OUTPUT_SAMPLE_RATE};
