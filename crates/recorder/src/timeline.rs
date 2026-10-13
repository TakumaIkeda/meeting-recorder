//! Places captured blocks on a common timeline and resamples them to the output rate.
//!
//! Every track's sample 0 corresponds to the same instant `t0` (the recording start), so
//! mic.wav and system.wav can be merged later purely by sample index / timestamp.
//!
//! * Blocks captured before `t0` are trimmed.
//! * Gaps (WASAPI loopback delivers no packets while nothing is playing, or the device
//!   glitched) are filled with silence once they exceed [`GAP_TOLERANCE_SECS`].
//! * On stop the track is padded with silence up to the stop instant.

use crate::dsp::Resampler;

/// Gaps shorter than this are treated as timestamp jitter and not filled.
pub const GAP_TOLERANCE_SECS: f64 = 0.05;

pub trait SampleSink {
    fn write(&mut self, samples: &[f32]) -> anyhow::Result<()>;
}

impl SampleSink for Vec<f32> {
    fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
        self.extend_from_slice(samples);
        Ok(())
    }
}

pub struct Timeline<S: SampleSink> {
    in_rate: f64,
    /// Input-rate frames placed on the timeline so far (including inserted silence).
    placed: u64,
    /// Total silence inserted to fill gaps, in input frames.
    pub inserted_frames: u64,
    resampler: Resampler,
    scratch: Vec<f32>,
    sink: S,
}

impl<S: SampleSink> Timeline<S> {
    pub fn new(in_rate: u32, out_rate: u32, sink: S) -> Self {
        Self {
            in_rate: in_rate as f64,
            placed: 0,
            inserted_frames: 0,
            resampler: Resampler::new(in_rate, out_rate),
            scratch: Vec::new(),
            sink,
        }
    }

    /// Add a mono block whose first sample was captured `offset_secs` after `t0`
    /// (negative if captured before `t0`).
    pub fn push(&mut self, offset_secs: f64, samples: &[f32]) -> anyhow::Result<()> {
        let expected = (offset_secs * self.in_rate).round() as i64;
        let diff = expected - self.placed as i64;
        let tolerance = (GAP_TOLERANCE_SECS * self.in_rate) as i64;

        let mut samples = samples;
        if diff > tolerance || (self.placed == 0 && diff > 0) {
            // Missing audio: fill with silence so later blocks keep their real position.
            self.silence(diff as u64)?;
        } else if diff < 0 && expected < 0 {
            // Captured (partly) before t0: drop the part that precedes the start.
            let skip = ((-expected) as usize).min(samples.len());
            samples = &samples[skip..];
        }
        if samples.is_empty() {
            return Ok(());
        }
        self.scratch.clear();
        self.resampler.process(samples, &mut self.scratch);
        self.placed += samples.len() as u64;
        self.sink.write(&self.scratch)
    }

    fn silence(&mut self, frames: u64) -> anyhow::Result<()> {
        self.scratch.clear();
        self.resampler.process_silence(frames, &mut self.scratch);
        self.placed += frames;
        self.inserted_frames += frames;
        self.sink.write(&self.scratch)
    }

    /// Pad up to `end_secs` after `t0`, flush the resampler and return the sink.
    pub fn finish(mut self, end_secs: f64) -> anyhow::Result<S> {
        let end = (end_secs * self.in_rate).round() as i64;
        let missing = end - self.placed as i64;
        if missing > 0 {
            self.silence(missing as u64)?;
        }
        self.scratch.clear();
        self.resampler.flush(&mut self.scratch);
        self.sink.write(&self.scratch)?;
        Ok(self.sink)
    }

    /// Seconds of audio placed on the timeline so far.
    pub fn placed_secs(&self) -> f64 {
        self.placed as f64 / self.in_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ones(n: usize) -> Vec<f32> {
        vec![0.5; n]
    }

    #[test]
    fn contiguous_blocks_are_not_padded() {
        let mut t = Timeline::new(16_000, 16_000, Vec::new());
        for i in 0..10 {
            // Slight jitter in timestamps after the first block must not insert silence.
            let jitter = match i {
                0 => 0.0,
                _ if i % 2 == 0 => 0.002,
                _ => -0.002,
            };
            t.push(i as f64 * 0.01 + jitter, &ones(160)).unwrap();
        }
        assert_eq!(t.inserted_frames, 0);
        let out = t.finish(0.1).unwrap();
        assert_eq!(out.len(), 1600);
    }

    #[test]
    fn late_first_block_is_shifted_to_its_real_position() {
        let mut t = Timeline::new(48_000, 16_000, Vec::new());
        t.push(0.25, &ones(4800)).unwrap();
        let out = t.finish(0.35).unwrap();
        assert_eq!(out.len(), 5600);
        // Silence until 0.25 s (allow for the filter's ringing near the edge).
        assert!(out[..3900].iter().all(|s| s.abs() < 1e-3));
        assert!((out[4400] - 0.5).abs() < 1e-2);
    }

    #[test]
    fn audio_before_start_is_trimmed() {
        let mut t = Timeline::new(16_000, 16_000, Vec::new());
        let mut block = vec![0.0f32; 1600];
        block[1599] = 1.0; // Last sample is captured 0.1 s - 1/16000 after the block start.
        t.push(-0.05, &block).unwrap();
        let out = t.finish(0.1).unwrap();
        assert_eq!(out.len(), 1600);
        let pos = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(pos, 1599 - 800);
    }

    #[test]
    fn silent_loopback_gap_is_filled_and_end_is_padded() {
        let mut t = Timeline::new(48_000, 16_000, Vec::new());
        t.push(0.0, &ones(4800)).unwrap(); // 0.0 - 0.1 s
        t.push(2.0, &ones(4800)).unwrap(); // 2.0 - 2.1 s, nothing played in between
        assert_eq!(t.inserted_frames, 1_900 * 48);
        let out = t.finish(3.0).unwrap();
        assert_eq!(out.len(), 48_000);
        assert!((out[16_000 * 2 + 800] - 0.5).abs() < 1e-2);
        assert!(out[16_000 + 100].abs() < 1e-3);
        assert!(out[47_000].abs() < 1e-3);
    }
}
