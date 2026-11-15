//! The mastering maximizer: a lookahead brickwall limiter that watches true peaks.
//!
//! Each sample asks for the gain that would keep it (and the peaks between it and its
//! neighbours) under the ceiling. A sliding minimum holds that request across the lookahead,
//! the release lets it recover, and a box filter as long as the lookahead turns every drop
//! into a smooth ramp that is complete before the peak arrives. Because the hold is a little
//! longer than the ramp, the ramp can never land late. A final clamp keeps the sample-peak
//! promise exactly. Latency is fixed, so the host never has to re-compensate.

use super::filters::one_pole;
use super::loudness::{TruePeak, TP_DELAY};

pub struct Maximizer {
    look: usize,
    hold: usize,
    /// Audio delay line (both channels), `look + TP_DELAY` long.
    delay: [Vec<f32>; 2],
    dw: usize,
    /// Monotonic queue for the sliding minimum: (time, value).
    q: Vec<(u64, f32)>,
    qh: usize,
    qt: usize,
    t: u64,
    box_buf: Vec<f32>,
    box_w: usize,
    box_sum: f64,
    box_refresh: usize,
    released: f32,
    tp: [TruePeak; 2],
    /// Short sample delay so the sample-peak check sees the same instant as the true peak.
    raw: [[f32; TP_DELAY + 1]; 2],
    raw_w: usize,
    fs: f32,
    rel_ms: f32,
    rel_k: f32,
    /// How long it has been limiting, 0..1; slows the release on sustained material.
    sustain: f32,
    sustain_k: f32,
    pub gain: f32,
}

impl Maximizer {
    pub fn new(fs: f32) -> Self {
        let look = ((fs * 0.004) as usize).max(16);
        let hold = look + 3;
        let qcap = hold + 4;
        Self {
            look,
            hold,
            delay: [vec![0.0; look + TP_DELAY], vec![0.0; look + TP_DELAY]],
            dw: 0,
            q: vec![(0, 1.0); qcap],
            qh: 0,
            qt: 0,
            t: 0,
            box_buf: vec![1.0; look],
            box_w: 0,
            box_sum: look as f64,
            box_refresh: 0,
            released: 1.0,
            tp: [TruePeak::new(), TruePeak::new()],
            raw: [[0.0; TP_DELAY + 1]; 2],
            raw_w: 0,
            fs,
            rel_ms: -1.0,
            rel_k: 0.0,
            sustain: 0.0,
            sustain_k: one_pole(fs, 0.6),
            gain: 1.0,
        }
    }

    pub fn latency(&self) -> usize {
        self.look + TP_DELAY
    }

    pub fn reset(&mut self) {
        for d in &mut self.delay {
            d.fill(0.0);
        }
        self.qh = 0;
        self.qt = 0;
        self.box_buf.fill(1.0);
        self.box_sum = self.look as f64;
        self.released = 1.0;
        self.sustain = 0.0;
        for t in &mut self.tp {
            t.reset();
        }
        self.raw = [[0.0; TP_DELAY + 1]; 2];
        self.gain = 1.0;
    }

    fn push_min(&mut self, v: f32) {
        let cap = self.q.len();
        // Drop everything at the back that is no lower than the new value.
        while self.qt != self.qh {
            let last = (self.qt + cap - 1) % cap;
            if self.q[last].1 >= v {
                self.qt = last;
            } else {
                break;
            }
        }
        self.q[self.qt] = (self.t, v);
        self.qt = (self.qt + 1) % cap;
        // Drop what has left the window at the front.
        while self.q[self.qh].0 + (self.hold as u64) <= self.t {
            self.qh = (self.qh + 1) % cap;
        }
    }

    /// `ceiling` is linear; `release_ms` is the base release; `character` (0..1) lets the
    /// release slow down on dense material, which trades pumping for density. `true_peak`
    /// switches the inter-sample detection on. `enabled` = false passes the delayed audio.
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32, ceiling: f32, release_ms: f32, character: f32, true_peak: bool, enabled: bool) -> (f32, f32) {
        // What this sample needs, measured TP_DELAY samples behind the input.
        let rw = self.raw_w;
        self.raw[0][rw] = l;
        self.raw[1][rw] = r;
        self.raw_w = (rw + 1) % (TP_DELAY + 1);
        let dl = self.raw[0][self.raw_w];
        let dr = self.raw[1][self.raw_w];
        let mut peak = dl.abs().max(dr.abs());
        let tl = self.tp[0].tick(l);
        let tr = self.tp[1].tick(r);
        if true_peak {
            peak = peak.max(tl).max(tr);
        }
        let want = if enabled && peak > ceiling { ceiling / peak } else { 1.0 };

        self.t += 1;
        self.push_min(want);
        let held = self.q[self.qh].1;

        if self.rel_ms != release_ms {
            self.rel_ms = release_ms;
            self.rel_k = one_pole(self.fs, release_ms.max(1.0) * 0.001);
        }
        let limiting = if held < 0.999 { 1.0 } else { 0.0 };
        self.sustain += (limiting - self.sustain) * self.sustain_k;
        if held <= self.released {
            self.released = held;
        } else {
            let k = self.rel_k / (1.0 + 3.0 * character * self.sustain);
            self.released += (held - self.released) * k;
        }

        let old = self.box_buf[self.box_w];
        self.box_buf[self.box_w] = self.released;
        self.box_w = (self.box_w + 1) % self.look;
        self.box_sum += self.released as f64 - old as f64;
        self.box_refresh += 1;
        if self.box_refresh >= 4096 {
            // Wash out rounding drift now and then.
            self.box_refresh = 0;
            self.box_sum = self.box_buf.iter().map(|v| *v as f64).sum();
        }
        let g = ((self.box_sum / self.look as f64) as f32).min(1.0);
        self.gain = g;

        let n = self.delay[0].len();
        let out_l = self.delay[0][self.dw];
        let out_r = self.delay[1][self.dw];
        self.delay[0][self.dw] = l;
        self.delay[1][self.dw] = r;
        self.dw = (self.dw + 1) % n;
        if !enabled {
            self.gain = 1.0;
            return (out_l, out_r);
        }
        ((out_l * g).clamp(-ceiling, ceiling), (out_r * g).clamp(-ceiling, ceiling))
    }

    /// Gain reduction right now, dB (positive).
    pub fn reduction_db(&self) -> f32 {
        -20.0 * self.gain.max(1e-6).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn drums(fs: f32, secs: f32) -> Vec<f32> {
        // Loud tone with sharp hits on top.
        (0..(fs * secs) as usize)
            .map(|i| {
                let t = i as f32 / fs;
                let beat = (t * 4.0).fract();
                let hit = (-beat * 30.0).exp() * (TAU * 60.0 * t).sin() * 1.8;
                0.5 * (TAU * 220.0 * t).sin() + 0.3 * (TAU * 3300.0 * t + 0.4).sin() + hit
            })
            .collect()
    }

    #[test]
    fn holds_the_true_peak_ceiling() {
        let fs = 48_000.0;
        let ceiling = 10f32.powf(-1.0 / 20.0);
        let mut m = Maximizer::new(fs);
        let x = drums(fs, 4.0);
        let mut meter = [TruePeak::new(), TruePeak::new()];
        let mut worst = 0.0f32;
        for (i, v) in x.iter().enumerate() {
            let (l, r) = m.tick(*v, -*v * 0.9, ceiling, 80.0, 0.5, true, true);
            assert!(l.abs() <= ceiling + 1e-6 && r.abs() <= ceiling + 1e-6);
            if i > 4800 {
                worst = worst.max(meter[0].tick(l)).max(meter[1].tick(r));
            } else {
                meter[0].tick(l);
                meter[1].tick(r);
            }
        }
        // True peak within a small fraction of a dB of the ceiling.
        let over = 20.0 * (worst / ceiling).log10();
        assert!(over < 0.3, "true peak {over:.2} dB over");
    }

    #[test]
    fn transparent_below_the_ceiling() {
        let fs = 48_000.0;
        let mut m = Maximizer::new(fs);
        let lat = m.latency();
        let x: Vec<f32> = (0..9600).map(|i| 0.3 * (TAU * 440.0 * i as f32 / fs).sin()).collect();
        let mut out = Vec::new();
        for v in &x {
            out.push(m.tick(*v, *v, 0.9, 80.0, 0.5, true, true).0);
        }
        for i in lat..x.len() {
            assert!((out[i] - x[i - lat]).abs() < 1e-6);
        }
    }

    #[test]
    fn ramps_before_the_peak() {
        // A single loud click on silence: the output never clips hard (no gain step
        // at the click itself), and the peak comes out at the ceiling.
        let fs = 48_000.0;
        let mut m = Maximizer::new(fs);
        let mut x = vec![0.1f32; 4800];
        x[2400] = 2.0;
        let out: Vec<f32> = x.iter().map(|v| m.tick(*v, *v, 0.5, 50.0, 0.0, false, true).0).collect();
        let lat = m.latency();
        let at = out[2400 + lat];
        assert!((at - 0.5).abs() < 0.02, "{at}");
        // The gain had already fallen a little before: the sample before the click is below 0.1.
        assert!(out[2400 + lat - 2] < 0.099);
    }
}
