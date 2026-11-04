//! Biquads (RBJ cookbook) in transposed direct form II, and the small helpers built on them.

use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    LowPass,
    HighPass,
    BandPass,
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
    /// The settings the coefficients were made for, so unchanged ones cost nothing.
    last: (Kind, f32, f32, f32),
}

impl Default for Biquad {
    fn default() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0, last: (Kind::Peak, 0.0, 0.0, 0.0) }
    }
}

impl Biquad {
    pub fn new(kind: Kind, fs: f32, hz: f32, q: f32, gain_db: f32) -> Self {
        let mut b = Self::default();
        b.set(kind, fs, hz, q, gain_db);
        b
    }

    pub fn set(&mut self, kind: Kind, fs: f32, hz: f32, q: f32, gain_db: f32) {
        if self.last == (kind, hz, q, gain_db) {
            return;
        }
        self.last = (kind, hz, q, gain_db);
        let hz = hz.clamp(10.0, fs * 0.49);
        let w = TAU * hz / fs;
        let (sin, cos) = w.sin_cos();
        let q = q.max(0.05);
        let alpha = sin / (2.0 * q);
        let a = 10f32.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            Kind::LowPass => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            Kind::HighPass => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            Kind::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            Kind::Peak => (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a),
            Kind::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - s),
                    (a + 1.0) + (a - 1.0) * cos + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - s,
                )
            }
            Kind::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - s),
                    (a + 1.0) - (a - 1.0) * cos + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - s,
                )
            }
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Magnitude response in dB at `hz`, for drawing curves.
    pub fn response_db(&self, fs: f32, hz: f32) -> f32 {
        let w = TAU * hz / fs;
        let (c1, s1) = (w.cos(), -w.sin());
        let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = self.b1 * s1 + self.b2 * s2;
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = self.a1 * s1 + self.a2 * s2;
        let mag2 = (nr * nr + ni * ni) / (dr * dr + di * di).max(1e-20);
        10.0 * mag2.max(1e-20).log10()
    }
}

/// One-pole smoother coefficient for a time constant in seconds.
pub fn one_pole(fs: f32, seconds: f32) -> f32 {
    if seconds <= 0.0 {
        1.0
    } else {
        1.0 - (-1.0 / (fs * seconds)).exp()
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn gain_to_db(g: f32) -> f32 {
    20.0 * g.max(1e-9).log10()
}

/// A simple DC blocker (used after asymmetric saturation).
#[derive(Clone, Copy, Default)]
pub struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + 0.995 * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gain at `hz` from RMS in and out: a sampled peak misses the top of a high sine.
    fn gain_at(f: &mut Biquad, fs: f32, hz: f32) -> f32 {
        f.reset();
        let n = 48_000;
        let (mut e_in, mut e_out) = (0.0f64, 0.0f64);
        for i in 0..n {
            let x = (TAU * hz * i as f32 / fs).sin();
            let y = f.tick(x);
            if i > n / 2 {
                e_in += (x * x) as f64;
                e_out += (y * y) as f64;
            }
        }
        10.0 * (e_out / e_in).log10() as f32
    }

    #[test]
    fn filters_do_what_they_say() {
        let fs = 48_000.0;
        let mut hp = Biquad::new(Kind::HighPass, fs, 100.0, 0.707, 0.0);
        assert!(gain_at(&mut hp, fs, 30.0) < -18.0);
        assert!(gain_at(&mut hp, fs, 1000.0).abs() < 0.3);
        let mut pk = Biquad::new(Kind::Peak, fs, 3000.0, 1.0, 6.0);
        assert!((gain_at(&mut pk, fs, 3000.0) - 6.0).abs() < 0.3);
        assert!((pk.response_db(fs, 3000.0) - 6.0).abs() < 0.1);
        let mut hs = Biquad::new(Kind::HighShelf, fs, 8000.0, 0.707, 4.0);
        assert!((hs.response_db(fs, 22_000.0) - 4.0).abs() < 0.5);
        assert!((gain_at(&mut hs, fs, 16000.0) - hs.response_db(fs, 16000.0)).abs() < 0.2);
        assert!(gain_at(&mut hs, fs, 200.0).abs() < 0.2);
    }
}
