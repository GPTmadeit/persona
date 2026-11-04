//! A brickwall safety limiter with a short lookahead, so the chain can be pushed hard without
//! clipping the DAW's mixer. The lookahead is fixed, which keeps the plugin's latency stable.

use super::filters::one_pole;

pub struct Limiter {
    look: usize,
    delay: [Vec<f32>; 2],
    req: Vec<f32>,
    w: usize,
    gain: f32,
    att: f32,
    rel: f32,
}

impl Limiter {
    pub fn new(fs: f32) -> Self {
        let look = ((fs * 0.0015) as usize).max(8);
        Self {
            look,
            delay: [vec![0.0; look], vec![0.0; look]],
            req: vec![1.0; look],
            w: 0,
            gain: 1.0,
            att: one_pole(fs, look as f32 / fs / 3.0),
            rel: one_pole(fs, 0.08),
        }
    }

    pub fn latency(&self) -> usize {
        self.look
    }

    pub fn reset(&mut self) {
        for d in &mut self.delay {
            d.fill(0.0);
        }
        self.req.fill(1.0);
        self.gain = 1.0;
    }

    /// Returns the delayed, limited pair and the gain applied.
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32, ceiling: f32, enabled: bool) -> (f32, f32, f32) {
        let peak = l.abs().max(r.abs());
        self.req[self.w] = if enabled && peak > ceiling { ceiling / peak } else { 1.0 };
        let out_l = self.delay[0][self.w];
        let out_r = self.delay[1][self.w];
        self.delay[0][self.w] = l;
        self.delay[1][self.w] = r;
        self.w = (self.w + 1) % self.look;
        // Lowest gain anything in the lookahead window will need.
        let want = self.req.iter().copied().fold(1.0f32, f32::min);
        let k = if want < self.gain { self.att } else { self.rel };
        self.gain += (want - self.gain) * k;
        if !enabled {
            return (out_l, out_r, 1.0);
        }
        let g = self.gain.min(1.0);
        // The ceiling is a promise: clamp whatever the smoothing let through.
        ((out_l * g).clamp(-ceiling, ceiling), (out_r * g).clamp(-ceiling, ceiling), g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_exceeds_the_ceiling() {
        let fs = 48_000.0;
        let mut lim = Limiter::new(fs);
        let ceiling = 0.8;
        for i in 0..48_000 {
            let x = 1.6 * (std::f32::consts::TAU * 150.0 * i as f32 / fs).sin();
            let (l, r, _) = lim.tick(x, -x, ceiling, true);
            assert!(l.abs() <= ceiling + 1e-6 && r.abs() <= ceiling + 1e-6);
        }
    }
}
