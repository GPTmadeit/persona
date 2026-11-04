//! Monophonic f0 estimation: YIN (de Cheveigné & Kawahara 2002) with an FFT-accelerated
//! difference function, sub-sample refinement, single-frame outlier rejection and a lattice of
//! octave candidates for the stabiliser to choose between. Ported from HelixTune's
//! PitchDetector (MIT, github.com/GPTmadeit/HelixTune).

use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;

/// Where to look for the voice. The search bounds are the biggest lever on octave errors: a
/// tracker that never looks below 180 Hz cannot halve onto a rumble.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoiceRange {
    Low,
    Mid,
    High,
    Wide,
}

impl VoiceRange {
    pub const ALL: [VoiceRange; 4] = [VoiceRange::Low, VoiceRange::Mid, VoiceRange::High, VoiceRange::Wide];

    /// Search range in Hz.
    pub fn hz(self) -> (f32, f32) {
        match self {
            VoiceRange::Low => (65.0, 600.0),
            VoiceRange::Mid => (90.0, 900.0),
            VoiceRange::High => (180.0, 1600.0),
            VoiceRange::Wide => (65.0, 1600.0),
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Candidate {
    pub period: f32,
    pub hz: f32,
    /// Normalised difference value; lower is more periodic.
    pub cost: f32,
}

pub const MAX_CANDIDATES: usize = 6;

#[derive(Clone, Copy, Default, Debug)]
pub struct Detection {
    pub hz: f32,
    pub midi: f32,
    /// 1 - aperiodicity, 0..1.
    pub confidence: f32,
    pub voiced: bool,
    pub rms: f32,
    pub candidates: [Candidate; MAX_CANDIDATES],
    pub count: usize,
}

struct Config {
    tau_min: usize,
    tau_max: usize,
    window: usize,
    frame: usize,
    n: usize,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
}

pub struct Detector {
    fs: f32,
    configs: Vec<Config>,
    current: usize,
    tracking: f32,
    spec_a: Vec<Complex32>,
    spec_b: Vec<Complex32>,
    scratch: Vec<Complex32>,
    power: Vec<f32>,
    diff: Vec<f32>,
    cmnd: Vec<f32>,
    history: [f32; 2],
    history_fill: usize,
    max_frame: usize,
}

impl Detector {
    /// Builds every range's configuration up front, so switching ranges never allocates.
    pub fn new(fs: f32) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let mut configs = Vec::new();
        let (mut max_n, mut max_tau, mut max_frame, mut max_scratch) = (0, 0, 0, 0);
        for r in VoiceRange::ALL {
            let (lo, hi) = r.hz();
            // The difference sum has to span two periods of the lowest note in range.
            let tau_max = (fs / lo).ceil() as usize + 2;
            let tau_min = ((fs / hi).floor() as usize).saturating_sub(2).max(2);
            let window = tau_max;
            let frame = window + tau_max;
            let n = frame.next_power_of_two();
            let fft = planner.plan_fft_forward(n);
            let ifft = planner.plan_fft_inverse(n);
            max_scratch = max_scratch.max(fft.get_inplace_scratch_len()).max(ifft.get_inplace_scratch_len());
            max_n = max_n.max(n);
            max_tau = max_tau.max(tau_max);
            max_frame = max_frame.max(frame);
            configs.push(Config { tau_min, tau_max, window, frame, n, fft, ifft });
        }
        Self {
            fs,
            configs,
            current: VoiceRange::Mid.index(),
            tracking: 0.5,
            spec_a: vec![Complex32::default(); max_n],
            spec_b: vec![Complex32::default(); max_n],
            scratch: vec![Complex32::default(); max_scratch],
            power: vec![0.0; max_tau + 1],
            diff: vec![0.0; max_tau + 1],
            cmnd: vec![0.0; max_tau + 1],
            history: [0.0; 2],
            history_fill: 0,
            max_frame,
        }
    }

    pub fn set_range(&mut self, r: VoiceRange) {
        self.current = r.index();
    }

    /// 0 accepts only clean periodicity; 1 tolerates breathy, noisy takes.
    pub fn set_tracking(&mut self, t: f32) {
        self.tracking = t.clamp(0.0, 1.0);
    }

    pub fn frame_size(&self) -> usize {
        self.configs[self.current].frame
    }

    pub fn max_frame_size(&self) -> usize {
        self.max_frame
    }

    pub fn reset(&mut self) {
        self.history = [0.0; 2];
        self.history_fill = 0;
    }

    fn difference(&mut self, frame: &[f32]) {
        let c = &self.configs[self.current];
        // r(tau) = sum_{j<W} x[j] x[j+tau] = IFFT(A * conj(B)), A the whole frame, B its first W.
        for (i, s) in self.spec_a[..c.n].iter_mut().enumerate() {
            *s = Complex32::new(if i < c.frame { frame[i] } else { 0.0 }, 0.0);
        }
        for (i, s) in self.spec_b[..c.n].iter_mut().enumerate() {
            *s = Complex32::new(if i < c.window { frame[i] } else { 0.0 }, 0.0);
        }
        c.fft.process_with_scratch(&mut self.spec_a[..c.n], &mut self.scratch);
        c.fft.process_with_scratch(&mut self.spec_b[..c.n], &mut self.scratch);
        for k in 0..c.n {
            self.spec_a[k] *= self.spec_b[k].conj();
        }
        c.ifft.process_with_scratch(&mut self.spec_a[..c.n], &mut self.scratch);

        let mut p0 = 0.0f64;
        for &x in &frame[..c.window] {
            p0 += x as f64 * x as f64;
        }
        self.power[0] = p0 as f32;
        let mut running = p0;
        for tau in 1..=c.tau_max {
            let out = frame[tau - 1] as f64;
            let inn = frame[tau - 1 + c.window] as f64;
            running += inn * inn - out * out;
            self.power[tau] = running.max(0.0) as f32;
        }
        // Calibrate the transform's scale from r(0) == p(0) rather than assume one.
        let raw0 = self.spec_a[0].re;
        let scale = if raw0.abs() > 1e-20 { p0 as f32 / raw0 } else { 0.0 };
        for tau in 0..=c.tau_max {
            let r = self.spec_a[tau].re * scale;
            self.diff[tau] = (self.power[0] + self.power[tau] - 2.0 * r).max(0.0);
        }
        // Cumulative mean normalisation: what stops YIN reporting an octave down.
        self.cmnd[0] = 1.0;
        let mut sum = 0.0f64;
        for tau in 1..=c.tau_max {
            sum += self.diff[tau] as f64;
            self.cmnd[tau] = if sum > 1e-20 { (self.diff[tau] as f64 * tau as f64 / sum) as f32 } else { 1.0 };
        }
    }

    /// The first dip below the threshold (not the global minimum): that ordering is the
    /// bias against octave-down errors. Negative when the threshold was never crossed.
    fn absolute_threshold(&self, threshold: f32) -> isize {
        let c = &self.configs[self.current];
        let mut tau = c.tau_min;
        while tau <= c.tau_max {
            if self.cmnd[tau] < threshold {
                while tau < c.tau_max && self.cmnd[tau + 1] < self.cmnd[tau] {
                    tau += 1;
                }
                return tau as isize;
            }
            tau += 1;
        }
        let mut best = c.tau_min;
        for tau in c.tau_min..=c.tau_max {
            if self.cmnd[tau] < self.cmnd[best] {
                best = tau;
            }
        }
        -(best as isize)
    }

    /// Sub-sample period from a parabola through the raw difference function (the
    /// normalised one is tilted and drags the vertex).
    fn refine(&self, tau: usize) -> f32 {
        let c = &self.configs[self.current];
        if tau <= c.tau_min || tau >= c.tau_max {
            return tau as f32;
        }
        let (a, b, d) = (self.diff[tau - 1], self.diff[tau], self.diff[tau + 1]);
        let denom = a - 2.0 * b + d;
        if denom.abs() < 1e-12 {
            return tau as f32;
        }
        tau as f32 + (0.5 * (a - d) / denom).clamp(-1.0, 1.0)
    }

    /// Every local minimum is a period the frame is consistent with. The lattice is anchored on
    /// YIN's own estimate (right by construction); octave alternatives start at a disadvantage.
    fn collect(&self, tau_yin: usize, out: &mut Detection) {
        let c = &self.configs[self.current];
        out.count = 0;
        let anchor_tau = tau_yin.clamp(c.tau_min, c.tau_max);
        let anchor = self.refine(anchor_tau);
        if anchor > 0.0 {
            out.candidates[0] = Candidate { period: anchor, hz: self.fs / anchor, cost: self.cmnd[anchor_tau].clamp(0.0, 1.0) };
            out.count = 1;
        }
        let mut best: [(usize, f32); MAX_CANDIDATES] = [(0, 0.0); MAX_CANDIDATES];
        let mut found = 0;
        for tau in c.tau_min + 1..c.tau_max {
            let v = self.cmnd[tau];
            if v >= self.cmnd[tau - 1] || v > self.cmnd[tau + 1] || v > 0.75 || tau.abs_diff(anchor_tau) <= 2 {
                continue;
            }
            if found < MAX_CANDIDATES - 1 {
                best[found] = (tau, v);
                found += 1;
            } else {
                let mut worst = 0;
                for i in 1..found {
                    if best[i].1 > best[worst].1 {
                        worst = i;
                    }
                }
                if v < best[worst].1 {
                    best[worst] = (tau, v);
                }
            }
        }
        for &(tau, v) in &best[..found] {
            if out.count >= MAX_CANDIDATES {
                break;
            }
            let t = self.refine(tau);
            if t > 0.0 {
                out.candidates[out.count] = Candidate { period: t, hz: self.fs / t, cost: v.clamp(0.0, 1.0) };
                out.count += 1;
            }
        }
        if out.count == 0 || anchor <= 0.0 {
            return;
        }
        for cand in &mut out.candidates[..out.count] {
            let ratio = cand.period / anchor;
            if ratio > 1.4 {
                cand.cost = (cand.cost + 0.35 * ratio.log2()).clamp(0.0, 2.0);
            } else if ratio < 1.0 / 1.4 {
                cand.cost = (cand.cost + 0.35 * (1.0 / ratio).log2()).clamp(0.0, 2.0);
            }
        }
        out.candidates[..out.count].sort_by(|a, b| a.period.total_cmp(&b.period));
    }

    /// `frame` holds the newest `frame_size()` samples, oldest first.
    pub fn process(&mut self, frame: &[f32]) -> Detection {
        let mut out = Detection::default();
        let fsz = self.frame_size();
        let frame = &frame[frame.len() - fsz..];
        let energy: f64 = frame.iter().map(|&x| x as f64 * x as f64).sum();
        out.rms = (energy / fsz as f64).sqrt() as f32;
        if out.rms < 1e-5 {
            self.reset();
            return out;
        }
        self.difference(frame);
        let threshold = 0.05 + self.tracking * 0.25;
        let raw = self.absolute_threshold(threshold);
        let tau_int = raw.unsigned_abs();
        self.collect(tau_int, &mut out);
        let tau = self.refine(tau_int);
        if tau <= 0.0 {
            return out;
        }
        let hz = self.fs / tau;
        let (lo, hi) = VoiceRange::ALL[self.current].hz();
        out.confidence = (1.0 - self.cmnd[tau_int]).clamp(0.0, 1.0);
        out.voiced = raw > 0 && hz >= lo && hz <= hi && out.confidence > 0.35;
        if !out.voiced {
            self.reset();
            return out;
        }
        out.hz = hz;
        // One frame that disagrees with two agreeing ones is a consonant or an octave slip.
        if self.history_fill >= 2 {
            let prev_agree = (self.history[0] / self.history[1]).log2().abs() < 0.04;
            let disagrees = (hz / self.history[0]).log2().abs() > 0.10;
            if prev_agree && disagrees {
                out.hz = self.history[0];
            }
        }
        self.history[1] = self.history[0];
        self.history[0] = out.hz;
        self.history_fill = (self.history_fill + 1).min(2);
        out.midi = 69.0 + 12.0 * (out.hz / 440.0).log2();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(fs: f32, hz: f32, n: usize) -> Vec<f32> {
        // A vocal-ish pulse: fundamental plus decaying harmonics.
        (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                (1..8).map(|h| (std::f32::consts::TAU * hz * h as f32 * t).sin() / h as f32).sum::<f32>() * 0.3
            })
            .collect()
    }

    #[test]
    fn finds_the_pitch_across_the_voice() {
        let fs = 48_000.0;
        let mut d = Detector::new(fs);
        for (range, hz) in [(VoiceRange::Low, 98.0), (VoiceRange::Mid, 220.0), (VoiceRange::Mid, 330.0), (VoiceRange::High, 523.25)] {
            d.set_range(range);
            d.reset();
            let x = tone(fs, hz, d.frame_size() * 2);
            let r = d.process(&x);
            assert!(r.voiced, "{hz} Hz not voiced");
            let cents = 1200.0 * (r.hz / hz).log2();
            assert!(cents.abs() < 5.0, "{hz} Hz read as {} ({cents} cents)", r.hz);
        }
    }

    #[test]
    fn noise_and_silence_are_unvoiced() {
        let mut d = Detector::new(48_000.0);
        let n = d.frame_size();
        assert!(!d.process(&vec![0.0; n]).voiced);
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        assert!(!d.process(&noise).voiced);
    }
}
