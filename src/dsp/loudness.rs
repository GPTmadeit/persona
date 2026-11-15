//! Loudness and true-peak metering after ITU-R BS.1770-4 / EBU R128: K-weighting, momentary
//! (400 ms), short-term (3 s), gated integrated loudness, loudness range, and a 4x oversampled
//! true-peak detector. Everything is allocated up front, so the meters run on the audio thread.

use super::filters::Biquad;
use std::f64::consts::PI;

/// The two K-weighting stages (head shelf and RLB high-pass) for one channel.
#[derive(Clone, Copy)]
pub struct KWeight {
    shelf: Biquad,
    hp: Biquad,
}

impl KWeight {
    pub fn new(fs: f32) -> Self {
        let fs = fs as f64;
        // Stage 1: the high shelf that models the head.
        let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
        let k = (PI * f0 / fs).tan();
        let vh = 10f64.powf(g / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad::from_coefs(
            ((vh + vb * k / q + k * k) / a0) as f32,
            (2.0 * (k * k - vh) / a0) as f32,
            ((vh - vb * k / q + k * k) / a0) as f32,
            (2.0 * (k * k - 1.0) / a0) as f32,
            ((1.0 - k / q + k * k) / a0) as f32,
        );
        // Stage 2: the RLB high-pass.
        let (f0, q) = (38.13547087602444, 0.5003270373238773);
        let k = (PI * f0 / fs).tan();
        let a0 = 1.0 + k / q + k * k;
        let hp = Biquad::from_coefs(1.0, -2.0, 1.0, (2.0 * (k * k - 1.0) / a0) as f32, ((1.0 - k / q + k * k) / a0) as f32);
        Self { shelf, hp }
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        self.hp.tick(self.shelf.tick(x))
    }

    pub fn reset(&mut self) {
        self.shelf.reset();
        self.hp.reset();
    }
}

/// Mean-square energy to LUFS (the -0.691 offset puts a 1 kHz full-scale sine at -3.01 LUFS
/// per channel, as the standard says).
#[inline]
pub fn lufs(energy: f64) -> f32 {
    (-0.691 + 10.0 * energy.max(1e-20).log10()) as f32
}

#[inline]
fn energy_of(lufs: f32) -> f64 {
    10f64.powf((lufs as f64 + 0.691) / 10.0)
}

/// Histogram resolution for the gated measurements: 0.1 LU from -70 to +10 LUFS.
const BINS: usize = 800;
const LOW: f32 = -70.0;

fn bin_of(l: f32) -> Option<usize> {
    if l < LOW {
        None
    } else {
        Some((((l - LOW) * 10.0) as usize).min(BINS - 1))
    }
}

fn lufs_of_bin(i: usize) -> f32 {
    LOW + (i as f32 + 0.5) * 0.1
}

/// Stereo loudness meter. Feed it samples; every 100 ms it updates its readings.
pub struct LoudnessMeter {
    k: [KWeight; 2],
    block_len: usize,
    acc: f64,
    count: usize,
    /// The last 30 blocks of 100 ms, as mean-square energy.
    blocks: [f64; 30],
    head: usize,
    filled: usize,
    /// Gating blocks (400 ms, every 100 ms) for the integrated value: count and energy per bin.
    int_count: Box<[u32; BINS]>,
    int_energy: Box<[f64; BINS]>,
    /// Short-term values every 100 ms, for the loudness range.
    st_count: Box<[u32; BINS]>,
    pub momentary: f32,
    pub short_term: f32,
    pub integrated: f32,
    pub range: f32,
    /// Seconds of audio the integrated value has seen.
    pub seconds: f32,
}

impl LoudnessMeter {
    pub fn new(fs: f32) -> Self {
        Self {
            k: [KWeight::new(fs); 2],
            block_len: ((fs * 0.1) as usize).max(1),
            acc: 0.0,
            count: 0,
            blocks: [0.0; 30],
            head: 0,
            filled: 0,
            int_count: Box::new([0; BINS]),
            int_energy: Box::new([0.0; BINS]),
            st_count: Box::new([0; BINS]),
            momentary: -100.0,
            short_term: -100.0,
            integrated: -100.0,
            range: 0.0,
            seconds: 0.0,
        }
    }

    /// Clears everything, including the integrated value.
    pub fn reset(&mut self) {
        for k in &mut self.k {
            k.reset();
        }
        self.acc = 0.0;
        self.count = 0;
        self.blocks = [0.0; 30];
        self.filled = 0;
        self.reset_integrated();
        self.momentary = -100.0;
        self.short_term = -100.0;
    }

    /// Starts the integrated value and the loudness range over.
    pub fn reset_integrated(&mut self) {
        self.int_count.fill(0);
        self.int_energy.fill(0.0);
        self.st_count.fill(0);
        self.integrated = -100.0;
        self.range = 0.0;
        self.seconds = 0.0;
    }

    /// Returns true when a 100 ms block just completed (the readings changed).
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32) -> bool {
        let a = self.k[0].tick(l) as f64;
        let b = self.k[1].tick(r) as f64;
        self.acc += a * a + b * b;
        self.count += 1;
        if self.count < self.block_len {
            return false;
        }
        self.blocks[self.head] = self.acc / self.count as f64;
        self.head = (self.head + 1) % 30;
        self.filled = (self.filled + 1).min(30);
        self.acc = 0.0;
        self.count = 0;
        self.update();
        true
    }

    fn mean_of_last(&self, n: usize) -> f64 {
        let n = n.min(self.filled).max(1);
        (0..n).map(|i| self.blocks[(self.head + 30 - 1 - i) % 30]).sum::<f64>() / n as f64
    }

    fn update(&mut self) {
        self.momentary = lufs(self.mean_of_last(4));
        self.short_term = lufs(self.mean_of_last(30));
        if self.filled >= 4 {
            self.seconds += 0.1;
            if let Some(i) = bin_of(self.momentary) {
                self.int_count[i] += 1;
                self.int_energy[i] += self.mean_of_last(4);
            }
        }
        if self.filled >= 30 {
            if let Some(i) = bin_of(self.short_term) {
                self.st_count[i] += 1;
            }
        }
        self.integrated = gated(&self.int_count, &self.int_energy);
        self.range = range(&self.st_count);
    }
}

/// Integrated loudness: absolute gate at -70 LUFS, then a relative gate 10 LU below.
fn gated(count: &[u32; BINS], energy: &[f64; BINS]) -> f32 {
    let (n, e) = count.iter().zip(energy.iter()).fold((0u64, 0.0f64), |(n, e), (c, x)| (n + *c as u64, e + x));
    if n == 0 {
        return -100.0;
    }
    let rel = lufs(e / n as f64) - 10.0;
    let start = bin_of(rel).unwrap_or(0);
    let (n, e) = count[start..].iter().zip(energy[start..].iter()).fold((0u64, 0.0f64), |(n, e), (c, x)| (n + *c as u64, e + x));
    if n == 0 {
        -100.0
    } else {
        lufs(e / n as f64)
    }
}

/// Loudness range (EBU Tech 3342): the spread between the 10th and 95th percentiles of the
/// short-term loudness, after a -70 absolute and a -20 LU relative gate.
fn range(count: &[u32; BINS]) -> f32 {
    let (n, e) = count.iter().enumerate().fold((0u64, 0.0f64), |(n, e), (i, c)| (n + *c as u64, e + *c as f64 * energy_of(lufs_of_bin(i))));
    if n < 10 {
        return 0.0;
    }
    let rel = lufs(e / n as f64) - 20.0;
    let start = bin_of(rel).unwrap_or(0);
    let total: u64 = count[start..].iter().map(|c| *c as u64).sum();
    if total == 0 {
        return 0.0;
    }
    let pct = |p: f64| {
        let want = (total as f64 * p).ceil() as u64;
        let mut seen = 0;
        for (i, c) in count[start..].iter().enumerate() {
            seen += *c as u64;
            if seen >= want.max(1) {
                return lufs_of_bin(start + i);
            }
        }
        lufs_of_bin(BINS - 1)
    };
    (pct(0.95) - pct(0.10)).max(0.0)
}

/// Taps per phase of the true-peak interpolator.
const TP_TAPS: usize = 12;
const TP_PHASES: usize = 4;
/// Samples the interpolated stream lags the input by.
pub const TP_DELAY: usize = TP_TAPS / 2;

/// Polyphase coefficients for 4x interpolation: a Kaiser-windowed sinc, cut off at the
/// original Nyquist.
fn tp_kernel() -> [[f32; TP_TAPS]; TP_PHASES] {
    let n = TP_TAPS * TP_PHASES;
    let beta = 6.0f64;
    let bessel = |x: f64| {
        let mut sum = 1.0;
        let mut term = 1.0;
        for k in 1..30 {
            term *= (x / 2.0) / k as f64;
            sum += term * term;
        }
        sum
    };
    let centre = (n as f64 - 1.0) / 2.0;
    let mut h = [[0.0f32; TP_TAPS]; TP_PHASES];
    for p in 0..TP_PHASES {
        let mut sum = 0.0;
        for t in 0..TP_TAPS {
            let i = t * TP_PHASES + p;
            let x = (i as f64 - centre) / TP_PHASES as f64;
            let sinc = if x.abs() < 1e-9 { 1.0 } else { (PI * x).sin() / (PI * x) };
            let r = (i as f64 - centre) / centre;
            let w = bessel(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel(beta);
            h[p][t] = (sinc * w) as f32;
            sum += sinc * w;
        }
        // Each phase passes DC at unity.
        for t in 0..TP_TAPS {
            h[p][t] /= sum as f32;
        }
    }
    h
}

/// Estimates the peak between samples by 4x oversampling (BS.1770 Annex 2).
#[derive(Clone)]
pub struct TruePeak {
    h: [[f32; TP_TAPS]; TP_PHASES],
    line: [f32; TP_TAPS * 2],
    pos: usize,
}

impl Default for TruePeak {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeak {
    pub fn new() -> Self {
        Self { h: tp_kernel(), line: [0.0; TP_TAPS * 2], pos: 0 }
    }

    pub fn reset(&mut self) {
        self.line = [0.0; TP_TAPS * 2];
    }

    /// The largest absolute value among the four interpolated points, about `TP_DELAY`
    /// samples behind `x`.
    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        // A doubled line, so the window is always contiguous.
        self.line[self.pos] = x;
        self.line[self.pos + TP_TAPS] = x;
        self.pos = (self.pos + 1) % TP_TAPS;
        let w = &self.line[self.pos..self.pos + TP_TAPS];
        let mut peak = 0.0f32;
        for ph in &self.h {
            let mut acc = 0.0;
            for (c, v) in ph.iter().zip(w) {
                acc += c * v;
            }
            peak = peak.max(acc.abs());
        }
        peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn sine(fs: f32, hz: f32, amp: f32, secs: f32) -> Vec<f32> {
        (0..(fs * secs) as usize).map(|i| amp * (TAU * hz * i as f32 / fs).sin()).collect()
    }

    #[test]
    fn full_scale_sine_reads_minus_three() {
        for fs in [44_100.0, 48_000.0, 96_000.0] {
            let mut m = LoudnessMeter::new(fs);
            // The same 1 kHz sine at 0 dBFS in both channels: -3.01 LUFS per channel, +3 for two.
            for x in sine(fs, 1000.0, 1.0, 5.0) {
                m.tick(x, x);
            }
            assert!((m.integrated - 0.0).abs() < 0.15, "{fs}: {}", m.integrated);
            assert!((m.momentary - 0.0).abs() < 0.15);
            assert!((m.short_term - 0.0).abs() < 0.15);
        }
    }

    #[test]
    fn both_channels_at_minus_23() {
        // EBU Tech 3341 case 1: a 1 kHz sine at -23 dBFS in both channels reads -23 LUFS.
        let fs = 48_000.0;
        let mut m = LoudnessMeter::new(fs);
        let amp = 10f32.powf(-23.0 / 20.0);
        for x in sine(fs, 1000.0, amp, 6.0) {
            m.tick(x, x);
        }
        assert!((m.integrated - -23.0).abs() < 0.15, "{}", m.integrated);
    }

    #[test]
    fn gating_ignores_silence() {
        let fs = 48_000.0;
        let mut m = LoudnessMeter::new(fs);
        let amp = 10f32.powf(-20.0 / 20.0);
        let tone = sine(fs, 1000.0, amp, 5.0);
        for x in &tone {
            m.tick(*x, *x);
        }
        for _ in 0..(fs * 10.0) as usize {
            m.tick(0.0, 0.0);
        }
        // The silence is gated out (absolute gate), so integrated stays at the tone's level.
        assert!((m.integrated - -20.0).abs() < 0.3, "{}", m.integrated);
    }

    #[test]
    fn range_of_two_levels() {
        let fs = 48_000.0;
        let mut m = LoudnessMeter::new(fs);
        for (db, secs) in [(-20.0, 20.0), (-30.0, 20.0)] {
            let amp = 10f32.powf(db / 20.0);
            for x in sine(fs, 1000.0, amp, secs) {
                m.tick(x, x);
            }
        }
        // EBU Tech 3342 case 1 is 10 LU, give or take a unit.
        assert!((m.range - 10.0).abs() < 1.0, "{}", m.range);
    }

    #[test]
    fn true_peak_sees_between_samples() {
        // A sine at fs/4 with a 45 degree phase: every sample is at 0.707 but the wave peaks at 1.
        let fs = 48_000.0;
        let mut tp = TruePeak::new();
        let mut peak = 0.0f32;
        let mut sample_peak = 0.0f32;
        for i in 0..4800 {
            let x = (TAU * 12_000.0 * i as f32 / fs + TAU / 8.0).sin();
            sample_peak = sample_peak.max(x.abs());
            peak = peak.max(tp.tick(x));
        }
        assert!(sample_peak < 0.72);
        assert!(peak > 0.95 && peak < 1.06, "{peak}");
    }

    #[test]
    fn true_peak_of_a_low_tone_is_its_peak() {
        let fs = 48_000.0;
        let mut tp = TruePeak::new();
        let mut peak = 0.0f32;
        for i in 0..4800 {
            peak = peak.max(tp.tick(0.5 * (TAU * 997.0 * i as f32 / fs).sin()));
        }
        assert!((peak - 0.5).abs() < 0.01, "{peak}");
    }
}
