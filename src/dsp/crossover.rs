//! Linkwitz-Riley crossovers. Four bands from three LR4 splits, with all-pass compensation so
//! every band carries the same phase: untouched, the bands sum back to a flat (all-pass)
//! response, which is what lets a multiband stage sit in a master without colouring it.

use super::filters::{Biquad, Kind};

const Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// One LR4 split (two Butterworth stages each way).
#[derive(Clone, Copy, Default)]
pub struct Lr4 {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
}

impl Lr4 {
    pub fn set(&mut self, fs: f32, hz: f32) {
        for b in &mut self.lp {
            b.set(Kind::LowPass, fs, hz, Q, 0.0);
        }
        for b in &mut self.hp {
            b.set(Kind::HighPass, fs, hz, Q, 0.0);
        }
    }

    #[inline]
    pub fn split(&mut self, x: f32) -> (f32, f32) {
        let lo = {
            let v = self.lp[0].tick(x);
            self.lp[1].tick(v)
        };
        let hi = {
            let v = self.hp[0].tick(x);
            self.hp[1].tick(v)
        };
        (lo, hi)
    }

    /// The low half only (a 24 dB/octave low-pass).
    #[inline]
    pub fn low(&mut self, x: f32) -> f32 {
        {
            let v = self.lp[0].tick(x);
            self.lp[1].tick(v)
        }
    }

    /// The high half only (a 24 dB/octave high-pass).
    #[inline]
    pub fn high(&mut self, x: f32) -> f32 {
        {
            let v = self.hp[0].tick(x);
            self.hp[1].tick(v)
        }
    }

    pub fn reset(&mut self) {
        for b in self.lp.iter_mut().chain(self.hp.iter_mut()) {
            b.reset();
        }
    }
}

/// Four bands for one channel, split at `f[0] < f[1] < f[2]`.
#[derive(Clone, Copy, Default)]
pub struct Split4 {
    mid: Lr4,
    low: Lr4,
    high: Lr4,
    /// The phase of the f[2] split, given to the low pair.
    ap_hi: Biquad,
    /// The phase of the f[0] split, given to the high pair.
    ap_lo: Biquad,
}

impl Split4 {
    pub fn set(&mut self, fs: f32, f: [f32; 3]) {
        self.low.set(fs, f[0]);
        self.mid.set(fs, f[1]);
        self.high.set(fs, f[2]);
        self.ap_lo.set(Kind::AllPass, fs, f[0], Q, 0.0);
        self.ap_hi.set(Kind::AllPass, fs, f[2], Q, 0.0);
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> [f32; 4] {
        let (lo, hi) = self.mid.split(x);
        let (b0, b1) = self.low.split(self.ap_hi.tick(lo));
        let (b2, b3) = self.high.split(self.ap_lo.tick(hi));
        [b0, b1, b2, b3]
    }

    pub fn reset(&mut self) {
        self.mid.reset();
        self.low.reset();
        self.high.reset();
        self.ap_lo.reset();
        self.ap_hi.reset();
    }
}

/// Keeps crossover points in order and apart (at least half an octave).
pub fn ordered(f: [f32; 3]) -> [f32; 3] {
    let a = f[0].clamp(30.0, 2000.0);
    let b = f[1].max(a * 1.42).clamp(60.0, 8000.0);
    let c = f[2].max(b * 1.42).clamp(120.0, 18000.0);
    [a, b, c]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    #[test]
    fn bands_sum_flat() {
        let fs = 48_000.0;
        for hz in [40.0, 120.0, 400.0, 1000.0, 3000.0, 9000.0, 16000.0] {
            let mut s = Split4::default();
            s.set(fs, [120.0, 1000.0, 6000.0]);
            let (mut e_in, mut e_out) = (0.0f64, 0.0f64);
            for i in 0..48_000 {
                let x = (TAU * hz * i as f32 / fs).sin();
                let y: f32 = s.tick(x).iter().sum();
                if i > 24_000 {
                    e_in += (x * x) as f64;
                    e_out += (y * y) as f64;
                }
            }
            let db = 10.0 * (e_out / e_in).log10();
            assert!(db.abs() < 0.05, "{hz} Hz: {db:.3} dB");
        }
    }

    #[test]
    fn bands_hold_their_range() {
        let fs = 48_000.0;
        let mut s = Split4::default();
        s.set(fs, [120.0, 1000.0, 6000.0]);
        let mut e = [0.0f64; 4];
        for i in 0..48_000 {
            let x = (TAU * 2400.0 * i as f32 / fs).sin();
            let b = s.tick(x);
            if i > 24_000 {
                for k in 0..4 {
                    e[k] += (b[k] * b[k]) as f64;
                }
            }
        }
        // 2.4 kHz belongs to band 2 (1 k - 6 k).
        assert!(e[2] > 10.0 * (e[0] + e[1] + e[3]), "{e:?}");
    }
}
