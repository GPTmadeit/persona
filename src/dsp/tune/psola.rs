//! Time-domain PSOLA pitch shifting. Grains of two periods are lifted from the input at
//! pitch-synchronous marks and overlap-added at a new spacing; the grain content is never
//! stretched, so the formants survive and the result sounds like the same person singing a
//! different note. Ported from HelixTune's PsolaShifter (MIT), with the formant resampling it
//! used for throat modelling left out.

fn next_pow2(v: usize) -> usize {
    v.next_power_of_two()
}

pub struct Psola {
    fs: f32,
    latency: usize,
    max_period: usize,
    capacity_period: usize,
    input: Vec<f32>,
    in_mask: i64,
    write_pos: i64,
    out_accum: Vec<f32>,
    win_accum: Vec<f32>,
    out_mask: i64,
    analysis_pos: f64,
    synth_pos: f64,
    primed: bool,
    last_period: f32,
    voiced_gain: f32,
    window: Vec<f32>,
    scores: Vec<f64>,
    ref_scratch: Vec<f32>,
    cand_scratch: Vec<f32>,
    energy_prefix: Vec<f64>,
}

impl Psola {
    /// Buffers are sized for `lowest_hz`, so later range changes never reallocate.
    pub fn new(fs: f32, lowest_hz: f32, max_block: usize) -> Self {
        let capacity_period = (fs / lowest_hz.max(20.0)).ceil() as usize;
        let ring = next_pow2(8 * capacity_period + 4 * max_block + 16);
        let window_size = 2048;
        let window = (0..=window_size)
            .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / window_size as f64).cos()) as f32)
            .collect();
        let scores_len = 2 * (capacity_period / 4 + 2) + 1;
        let mut s = Self {
            fs,
            latency: 3 * capacity_period + 8,
            max_period: capacity_period,
            capacity_period,
            input: vec![0.0; ring],
            in_mask: ring as i64 - 1,
            write_pos: 0,
            out_accum: vec![0.0; ring],
            win_accum: vec![0.0; ring],
            out_mask: ring as i64 - 1,
            analysis_pos: 0.0,
            synth_pos: 0.0,
            primed: false,
            last_period: 200.0,
            voiced_gain: 0.0,
            window,
            scores: vec![0.0; scores_len],
            ref_scratch: vec![0.0; 512],
            cand_scratch: vec![0.0; 512 + scores_len],
            energy_prefix: vec![0.0; 512 + scores_len + 1],
        };
        s.reset();
        s
    }

    /// Narrows the working range: recomputes grain size and latency only, no allocation.
    pub fn set_min_frequency(&mut self, hz: f32) {
        let wanted = (self.fs / hz.max(20.0)).ceil() as usize;
        self.max_period = wanted.clamp(16, self.capacity_period);
        // A grain centred on a synthesis mark reads input ahead of it; three periods covers it.
        self.latency = 3 * self.max_period + 8;
        self.reset();
    }

    pub fn latency(&self) -> usize {
        self.latency
    }

    pub fn reset(&mut self) {
        self.input.fill(0.0);
        self.out_accum.fill(0.0);
        self.win_accum.fill(0.0);
        self.write_pos = 0;
        self.analysis_pos = 0.0;
        self.synth_pos = 0.0;
        self.primed = false;
        self.last_period = self.max_period as f32 * 0.4;
        self.voiced_gain = 0.0;
    }

    #[inline]
    fn wrap_in(&self, p: i64) -> usize {
        (p & self.in_mask) as usize
    }

    #[inline]
    fn wrap_out(&self, p: i64) -> usize {
        (p & self.out_mask) as usize
    }

    /// Catmull-Rom read: linear interpolation is audible as a dull, gritty top once grains are
    /// read every period.
    fn read(&self, pos: f64) -> f32 {
        let lo = (self.write_pos - self.input.len() as i64 + 4) as f64;
        let hi = (self.write_pos - 3) as f64;
        let p = pos.clamp(lo, hi);
        let i = p.floor() as i64;
        let f = (p - i as f64) as f32;
        let xm1 = self.input[self.wrap_in(i - 1)];
        let x0 = self.input[self.wrap_in(i)];
        let x1 = self.input[self.wrap_in(i + 1)];
        let x2 = self.input[self.wrap_in(i + 2)];
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * f + c2) * f + c1) * f + x0
    }

    fn copy_from_ring(&self, start: i64, dest: &mut [f32]) {
        for (k, d) in dest.iter_mut().enumerate() {
            *d = self.input[self.wrap_in(start + k as i64)];
        }
    }

    /// "Previous mark + one period" accumulates phase error; a normalised cross-correlation
    /// against the previous grain snaps each mark back onto the same point of the glottal
    /// cycle. The result is a measured sub-sample *lag*, never an integer position: rounding
    /// the mark would add a creeping delay, which is a pitch shift.
    fn refine_mark(&mut self, predicted: f64, period: f32) -> f64 {
        let corr_len = (period as usize).clamp(32, 512);
        let search = ((period * 0.25) as usize).max(1);
        let half = corr_len / 2;
        let width = 2 * half;
        let ref_base = self.analysis_pos.round() as i64;
        let cand_base = predicted.round() as i64;
        if cand_base + search as i64 + half as i64 + 2 >= self.write_pos {
            return predicted;
        }
        let count = 2 * search + 1;
        let span = width + count - 1;
        if self.scores.len() < count || self.ref_scratch.len() < width || self.cand_scratch.len() < span {
            return predicted;
        }
        let mut refs = std::mem::take(&mut self.ref_scratch);
        let mut cands = std::mem::take(&mut self.cand_scratch);
        self.copy_from_ring(ref_base - half as i64, &mut refs[..width]);
        self.copy_from_ring(cand_base - search as i64 - half as i64, &mut cands[..span]);
        self.energy_prefix[0] = 0.0;
        for k in 0..span {
            let x = cands[k] as f64;
            self.energy_prefix[k + 1] = self.energy_prefix[k] + x * x;
        }
        let (mut best_score, mut best_i) = (f64::NEG_INFINITY, 0usize);
        for i in 0..count {
            let mut dot = 0.0f64;
            for k in 0..width {
                dot += refs[k] as f64 * cands[i + k] as f64;
            }
            let energy = (self.energy_prefix[i + width] - self.energy_prefix[i]).max(0.0);
            let score = dot / (energy + 1e-12).sqrt();
            self.scores[i] = score;
            if score > best_score {
                best_score = score;
                best_i = i;
            }
        }
        self.ref_scratch = refs;
        self.cand_scratch = cands;
        let mut sub = 0.0;
        if best_i > 0 && best_i < count - 1 {
            let (s0, s1, s2) = (self.scores[best_i - 1], self.scores[best_i], self.scores[best_i + 1]);
            let d = s0 - 2.0 * s1 + s2;
            if d.abs() > 1e-15 {
                sub = (0.5 * (s0 - s2) / d).clamp(-1.0, 1.0);
            }
        }
        let lag = ((cand_base + best_i as i64 - search as i64) as f64 + sub) - ref_base as f64;
        self.analysis_pos + lag
    }

    fn emit_grain(&mut self, analysis: f64, synth: f64, period: f32) {
        let l = (period.round() as i64).max(2);
        let base = synth.round() as i64;
        let frac = (synth - base as f64) as f32;
        let inv_span = 1.0 / (2 * l) as f32;
        let w_scale = (self.window.len() - 1) as f32;
        for k in -l..=l {
            let kp = k as f32 - frac;
            let u = (kp + l as f32) * inv_span;
            if u <= 0.0 || u >= 1.0 {
                continue;
            }
            let wi = u * w_scale;
            let i = wi as usize;
            let wf = wi - i as f32;
            let w = self.window[i] + wf * (self.window[i + 1] - self.window[i]);
            let s = self.read(analysis + kp as f64);
            let o = self.wrap_out(base + k);
            self.out_accum[o] += w * s;
            self.win_accum[o] += w;
        }
    }

    /// Shifts `io` in place by `ratio` (output f0 / input f0) given the current input
    /// `period` in samples. Unvoiced material crossfades to the dry input (delayed to match).
    pub fn process(&mut self, io: &mut [f32], ratio: f32, period: f32, voiced: bool) {
        let ratio = ratio.clamp(0.25, 4.0);
        if voiced && period > 1.0 {
            self.last_period = period.clamp(16.0, self.max_period as f32);
        }
        let period = self.last_period;
        let n = io.len();
        for (i, &x) in io.iter().enumerate() {
            let w = self.wrap_in(self.write_pos + i as i64);
            self.input[w] = x;
        }
        self.write_pos += n as i64;
        let out_start = self.write_pos - n as i64 - self.latency as i64;
        if out_start < 0 {
            io.fill(0.0);
            return;
        }
        if !self.primed {
            self.analysis_pos = out_start as f64;
            self.synth_pos = out_start as f64;
            self.primed = true;
        }
        // A stall can strand the marks; snap back rather than grind through catch-up grains.
        if (self.synth_pos - out_start as f64).abs() > (8 * self.max_period) as f64 {
            self.analysis_pos = out_start as f64;
            self.synth_pos = out_start as f64;
        }
        if (self.analysis_pos - self.synth_pos).abs() > (4 * self.max_period) as f64 {
            self.analysis_pos = self.synth_pos;
        }
        let l = (period.round() as i64).max(2);
        let emit_until = (out_start + n as i64 + l) as f64;
        let step = period as f64 / ratio as f64;
        let mut guard = 0;
        while self.synth_pos < emit_until && guard < 4096 {
            guard += 1;
            // Track the analysis mark nearest this synthesis mark: denser synthesis marks reuse
            // one (pitch up), sparser ones skip some (pitch down). That asymmetry is the shift.
            while (self.analysis_pos + period as f64 - self.synth_pos).abs() < (self.analysis_pos - self.synth_pos).abs() {
                let next = self.refine_mark(self.analysis_pos + period as f64, period);
                if next <= self.analysis_pos {
                    break;
                }
                self.analysis_pos = next;
            }
            self.emit_grain(self.analysis_pos, self.synth_pos, period);
            self.synth_pos += step;
        }
        let target = if voiced { 1.0 } else { 0.0 };
        let coeff = 1.0 - (-1.0 / (self.fs * 0.010)).exp();
        for (i, y) in io.iter_mut().enumerate() {
            let p = out_start + i as i64;
            let o = self.wrap_out(p);
            let w = self.win_accum[o];
            let wet = if w > 1e-4 { self.out_accum[o] / w } else { 0.0 };
            let dry = self.input[self.wrap_in(p)];
            self.out_accum[o] = 0.0;
            self.win_accum[o] = 0.0;
            self.voiced_gain += (target - self.voiced_gain) * coeff;
            *y = wet * self.voiced_gain + dry * (1.0 - self.voiced_gain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::tune::detector::{Detector, VoiceRange};

    #[test]
    fn shifts_by_the_asked_interval() {
        let fs = 48_000.0;
        let hz = 220.0;
        for semis in [-3.0f32, 0.0, 2.0, 5.0] {
            let mut ps = Psola::new(fs, 65.0, 512);
            ps.set_min_frequency(90.0);
            let ratio = 2f32.powf(semis / 12.0);
            let n = 48_000;
            let mut x: Vec<f32> = (0..n)
                .map(|i| {
                    let t = i as f32 / fs;
                    (1..6).map(|h| (std::f32::consts::TAU * hz * h as f32 * t).sin() / h as f32).sum::<f32>() * 0.3
                })
                .collect();
            for chunk in x.chunks_mut(256) {
                ps.process(chunk, ratio, fs / hz, true);
            }
            let mut d = Detector::new(fs);
            d.set_range(VoiceRange::Wide);
            let r = d.process(&x[n - d.frame_size()..]);
            let want = hz * ratio;
            let cents = 1200.0 * (r.hz / want).log2();
            assert!(r.voiced && cents.abs() < 8.0, "{semis} st: got {} Hz, want {want} ({cents:.1} cents)", r.hz);
        }
    }
}
