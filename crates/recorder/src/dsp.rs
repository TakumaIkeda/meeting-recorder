//! Signal processing helpers: downmix, peak metering and a streaming resampler.

/// Average interleaved `channels`-channel frames into mono.
pub fn downmix_into(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    let scale = 1.0 / channels as f32;
    out.extend(
        interleaved
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() * scale),
    );
}

/// Absolute peak of a block of samples.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
}

/// Convert a float sample in [-1, 1] to 16-bit PCM with clipping.
pub fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

/// Zero crossings of the windowed sinc on each side (per output sample period).
const ZERO_CROSSINGS: f64 = 32.0;
/// Cutoff relative to the lower of the two Nyquist frequencies.
const CUTOFF: f64 = 0.94;
/// Kernel table resolution (entries per input sample).
const TABLE_RES: f64 = 256.0;

/// Streaming band-limited resampler (windowed sinc, Blackman window) for mono audio.
///
/// Output sample `n` is placed exactly at input position `n * in_rate / out_rate`, so the
/// output timeline never drifts relative to the input no matter how input is chunked.
pub struct Resampler {
    step: f64,
    half_width: f64,
    table: Vec<f32>,
    buf: Vec<f32>,
    /// Absolute input index of `buf[0]`.
    buf_start: u64,
    /// Total input frames received so far.
    total_in: u64,
    next_out: u64,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        assert!(in_rate > 0 && out_rate > 0);
        let step = in_rate as f64 / out_rate as f64;
        // When downsampling the kernel is stretched so its cutoff sits below the output Nyquist.
        let scale = step.max(1.0);
        let half_width = ZERO_CROSSINGS * scale;
        // Cutoff in cycles per input sample.
        let fc = 0.5 * CUTOFF / scale;
        let len = (half_width * TABLE_RES).ceil() as usize + 2;
        let table = (0..len)
            .map(|i| {
                let x = i as f64 / TABLE_RES;
                if x >= half_width {
                    return 0.0;
                }
                let arg = 2.0 * fc * x;
                let sinc = if arg == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * arg)
                };
                // Blackman window over [-half_width, half_width], evaluated at x >= 0.
                let p = 0.5 + x / (2.0 * half_width);
                let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * p).cos()
                    + 0.08 * (4.0 * std::f64::consts::PI * p).cos();
                (2.0 * fc * sinc * w) as f32
            })
            .collect();
        Self {
            step,
            half_width,
            table,
            buf: Vec::new(),
            buf_start: 0,
            total_in: 0,
            next_out: 0,
        }
    }

    fn kernel(&self, distance: f64) -> f32 {
        let pos = distance * TABLE_RES;
        let i = pos as usize;
        if i + 1 >= self.table.len() {
            return 0.0;
        }
        let frac = (pos - i as f64) as f32;
        self.table[i] + (self.table[i + 1] - self.table[i]) * frac
    }

    /// Feed input samples and append every output sample that can be computed so far.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buf.extend_from_slice(input);
        self.total_in += input.len() as u64;
        self.produce(self.total_in, None, out);
    }

    /// Feed `count` zero samples (cheaper than building a buffer of zeros by the caller).
    pub fn process_silence(&mut self, count: u64, out: &mut Vec<f32>) {
        // Feed in bounded blocks so a long gap doesn't allocate a huge buffer.
        const BLOCK: u64 = 4096;
        let zeros = [0.0f32; BLOCK as usize];
        let mut left = count;
        while left > 0 {
            let n = left.min(BLOCK);
            self.process(&zeros[..n as usize], out);
            left -= n;
        }
    }

    /// Emit the remaining tail. Total output equals `round(total_in * out_rate / in_rate)`.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        let target = (self.total_in as f64 / self.step).round() as u64;
        let pad = vec![0.0f32; self.half_width.ceil() as usize + 2];
        self.buf.extend_from_slice(&pad);
        let available = self.total_in + pad.len() as u64;
        self.produce(available, Some(target), out);
    }

    fn produce(&mut self, available: u64, limit: Option<u64>, out: &mut Vec<f32>) {
        loop {
            if limit.is_some_and(|l| self.next_out >= l) {
                break;
            }
            let t = self.next_out as f64 * self.step;
            let last = (t + self.half_width).floor() as i64;
            if last >= available as i64 {
                break;
            }
            let first = (t - self.half_width).ceil() as i64;
            let mut acc = 0.0f32;
            for k in first.max(self.buf_start as i64)..=last {
                let x = self.buf[(k - self.buf_start as i64) as usize];
                acc += x * self.kernel((t - k as f64).abs());
            }
            out.push(acc);
            self.next_out += 1;
        }
        // Drop input that no future output sample can reach.
        let next_t = self.next_out as f64 * self.step;
        let keep_from = ((next_t - self.half_width).ceil().max(0.0)) as u64;
        if keep_from > self.buf_start {
            let drop = ((keep_from - self.buf_start) as usize).min(self.buf.len());
            self.buf.drain(..drop);
            self.buf_start += drop as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32 * 0.5
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn resample_chunked(input: &[f32], in_rate: u32, chunk: usize) -> Vec<f32> {
        let mut r = Resampler::new(in_rate, 16_000);
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            r.process(c, &mut out);
        }
        r.flush(&mut out);
        out
    }

    #[test]
    fn downmix_averages_channels() {
        let mut out = Vec::new();
        downmix_into(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn output_length_matches_duration() {
        for &rate in &[48_000u32, 44_100, 16_000, 8_000, 96_000] {
            let input = sine(rate, 440.0, 1.5);
            let out = resample_chunked(&input, rate, 480);
            assert_eq!(out.len(), 24_000, "rate {rate}");
        }
    }

    #[test]
    fn passband_tone_is_preserved() {
        for &rate in &[48_000u32, 44_100] {
            let out = resample_chunked(&sine(rate, 1000.0, 1.0), rate, 441);
            let expected = sine(16_000, 1000.0, 1.0);
            // Skip filter edges.
            let (a, b) = (&out[1000..15000], &expected[1000..15000]);
            let err = a
                .iter()
                .zip(b)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0f32, f32::max);
            assert!(err < 0.01, "rate {rate}: max error {err}");
        }
    }

    #[test]
    fn tone_above_nyquist_is_rejected() {
        // 12 kHz cannot be represented at 16 kHz and would alias to 4 kHz without filtering.
        let out = resample_chunked(&sine(48_000, 12_000.0, 1.0), 48_000, 512);
        assert!(
            rms(&out[1000..15000]) < 0.001,
            "rms {}",
            rms(&out[1000..15000])
        );
    }

    #[test]
    fn chunking_does_not_change_output() {
        let input = sine(44_100, 700.0, 0.5);
        let a = resample_chunked(&input, 44_100, 1);
        let b = resample_chunked(&input, 44_100, 10_000);
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6));
    }
}
