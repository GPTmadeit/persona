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
    AllPass,
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
    /// Whether `last` was an analog-matched design (`set_matched`) or a cookbook one (`set`).
    matched: bool,
}

impl Default for Biquad {
    fn default() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0, last: (Kind::Peak, 0.0, 0.0, 0.0), matched: false }
    }
}

impl Biquad {
    pub fn new(kind: Kind, fs: f32, hz: f32, q: f32, gain_db: f32) -> Self {
        let mut b = Self::default();
        b.set(kind, fs, hz, q, gain_db);
        b
    }

    pub fn set(&mut self, kind: Kind, fs: f32, hz: f32, q: f32, gain_db: f32) {
        if self.last == (kind, hz, q, gain_db) && !self.matched {
            return;
        }
        self.last = (kind, hz, q, gain_db);
        self.matched = false;
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
            Kind::AllPass => (1.0 - alpha, -2.0 * cos, 1.0 + alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
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

    /// A filter from normalised coefficients (a0 = 1), for designs outside the cookbook.
    pub fn from_coefs(b0: f32, b1: f32, b2: f32, a1: f32, a2: f32) -> Self {
        Self { b0, b1, b2, a1, a2, z1: 0.0, z2: 0.0, last: (Kind::Peak, f32::NAN, 0.0, 0.0), matched: false }
    }

    /// The same filters as `set`, designed to follow the analog curve all the way up instead of
    /// the cookbook's bilinear transform, which squeezes everything above a few kHz towards
    /// Nyquist: a +6 dB bell at 12 kHz comes out lopsided at 44.1 kHz, and a high shelf loses
    /// its top. Here the poles are the analog poles mapped straight to the z-plane, and the
    /// zeros are solved so the magnitude meets the analog filter's (after Vicanek, "Matched
    /// Second Order Digital Filters", 2016). Cuts are the exact inverse of the matching boost,
    /// and a high shelf is built from the low shelf it mirrors, so boost and cut always cancel.
    /// This is what a mastering EQ needs; `BandPass` and `AllPass` fall back to the cookbook.
    pub fn set_matched(&mut self, kind: Kind, fs: f32, hz: f32, q: f32, gain_db: f32) {
        if matches!(kind, Kind::BandPass | Kind::AllPass) {
            return self.set(kind, fs, hz, q, gain_db);
        }
        if self.last == (kind, hz, q, gain_db) && self.matched {
            return;
        }
        self.last = (kind, hz, q, gain_db);
        self.matched = true;
        let fs = fs as f64;
        let hz = (hz as f64).clamp(10.0, fs * 0.47);
        let q = (q as f64).max(0.05);
        let w0 = std::f64::consts::TAU * hz / fs;
        // Boosts only: `a` is the cookbook's A for the size of the move.
        let a = 10f64.powf((gain_db as f64).abs() / 40.0);
        let ra = a.sqrt();
        // H(s) = (n[0] s^2 + n[1] s + n[2]) / (d[0] s^2 + d[1] s + d[2]), s in units of the centre.
        let (n, d): ([f64; 3], [f64; 3]) = match kind {
            Kind::Peak => ([1.0, a / q, 1.0], [1.0, 1.0 / (a * q), 1.0]),
            Kind::LowShelf | Kind::HighShelf => ([a, a * ra / q, a * a], [a, ra / q, 1.0]),
            Kind::LowPass => ([0.0, 0.0, 1.0], [1.0, 1.0 / q, 1.0]),
            _ => ([1.0, 0.0, 0.0], [1.0, 1.0 / q, 1.0]),
        };
        // Poles: e^(p T) for each analog pole p.
        let disc = d[1] * d[1] - 4.0 * d[0] * d[2];
        let (a1, a2) = if disc < 0.0 {
            let re = -d[1] / (2.0 * d[0]);
            let im = (-disc).sqrt() / (2.0 * d[0]);
            (-2.0 * (re * w0).exp() * (im * w0).cos(), (2.0 * re * w0).exp())
        } else {
            let p1 = (-d[1] + disc.sqrt()) / (2.0 * d[0]);
            let p2 = (-d[1] - disc.sqrt()) / (2.0 * d[0]);
            (-((p1 * w0).exp() + (p2 * w0).exp()), ((p1 + p2) * w0).exp())
        };
        // |H|^2 of a biquad half is linear in these: X0 phi0 + X1 phi1 + X2 phi2.
        let (aa0, aa1, aa2) = ((1.0 + a1 + a2).powi(2), (1.0 - a1 + a2).powi(2), -4.0 * a2);
        let p1 = (w0 / 2.0).sin().powi(2);
        let p0 = 1.0 - p1;
        let p2 = 4.0 * p0 * p1;
        let den_at_centre = aa0 * p0 + aa1 * p1 + aa2 * p2;
        // The analog magnitude squared at x times the centre frequency.
        let analog = |x: f64| ((n[2] - n[0] * x * x).powi(2) + (n[1] * x).powi(2)) / ((d[2] - d[0] * x * x).powi(2) + (d[1] * x).powi(2)).max(1e-30);
        // From the squared-magnitude terms of the numerator to its (minimum-phase) coefficients.
        let zeros = |bb0: f64, bb1: f64, bb2: f64| {
            let (bb0, bb1) = (bb0.max(0.0), bb1.max(0.0));
            let w = 0.5 * (bb0.sqrt() + bb1.sqrt());
            let b0 = 0.5 * (w + (w * w + bb2).max(0.0).sqrt());
            (b0, 0.5 * (bb0.sqrt() - bb1.sqrt()), -bb2 / (4.0 * b0))
        };
        let (mut b, mut den) = match kind {
            Kind::HighPass => {
                // Both zeros stay at DC so the slope holds all the way down; the gain is
                // matched at the corner.
                let b0 = (den_at_centre * analog(1.0)).sqrt() / (4.0 * p1);
                ((b0, -2.0 * b0, b0), (a1, a2))
            }
            Kind::LowPass => {
                let bb0 = aa0;
                let bb1 = ((den_at_centre * analog(1.0) - bb0 * p0) / p1).max(0.0);
                let b0 = 0.5 * (bb0.sqrt() + bb1.sqrt());
                ((b0, bb0.sqrt() - b0, 0.0), (a1, a2))
            }
            Kind::Peak => {
                // Unity at DC; the centre has the analog gain and is the top of the bell.
                let g2 = analog(1.0);
                let r1 = den_at_centre * g2;
                let r2 = (-aa0 + aa1 + 4.0 * (p0 - p1) * aa2) * g2;
                let bb0 = aa0;
                let bb2 = (r1 - r2 * p1 - bb0) / (4.0 * p1 * p1);
                let bb1 = r2 + bb0 + 4.0 * (p1 - p0) * bb2;
                (zeros(bb0, bb1, bb2), (a1, a2))
            }
            _ => {
                // The low-shelf boost: the analog gain at DC, at the corner and at Nyquist.
                let bb0 = aa0 * analog(0.0);
                let bb1 = aa1 * analog(std::f64::consts::PI / w0);
                let bb2 = (den_at_centre * analog(1.0) - bb0 * p0 - bb1 * p1) / p2;
                (zeros(bb0, bb1, bb2), (a1, a2))
            }
        };
        // A cut is the boost turned upside down. A high shelf is the level it reaches times the
        // opposite low shelf, so it inverts when it boosts.
        let invert = match kind {
            Kind::Peak | Kind::LowShelf => gain_db < 0.0,
            Kind::HighShelf => gain_db >= 0.0,
            _ => false,
        };
        if invert && b.0.abs() > 1e-12 {
            let k = 1.0 / b.0;
            let (num, new_den) = ((k, den.0 * k, den.1 * k), (b.1 * k, b.2 * k));
            b = num;
            den = new_den;
        }
        if kind == Kind::HighShelf {
            let top = 10f64.powf(gain_db as f64 / 20.0);
            b = (b.0 * top, b.1 * top, b.2 * top);
        }
        self.b0 = b.0 as f32;
        self.b1 = b.1 as f32;
        self.b2 = b.2 as f32;
        self.a1 = den.0 as f32;
        self.a2 = den.1 as f32;
    }

    /// A new analog-matched filter; see `set_matched`.
    pub fn matched(kind: Kind, fs: f32, hz: f32, q: f32, gain_db: f32) -> Self {
        let mut b = Self::default();
        b.set_matched(kind, fs, hz, q, gain_db);
        b
    }

    /// Takes another filter's coefficients and keeps this one's state: for the second channel
    /// of a pair.
    pub fn copy_coefs(&mut self, from: &Biquad) {
        self.b0 = from.b0;
        self.b1 = from.b1;
        self.b2 = from.b2;
        self.a1 = from.a1;
        self.a2 = from.a2;
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

    /// Magnitude of the analog prototype `set_matched` aims for, dB.
    fn analog_db(kind: Kind, f0: f32, q: f32, gain_db: f32, hz: f32) -> f32 {
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let ra = a.sqrt();
        let q = q as f64;
        let (n, d): ([f64; 3], [f64; 3]) = match kind {
            Kind::Peak => ([1.0, a / q, 1.0], [1.0, 1.0 / (a * q), 1.0]),
            Kind::LowShelf => ([a, a * ra / q, a * a], [a, ra / q, 1.0]),
            Kind::HighShelf => ([a * a, a * ra / q, a], [1.0, ra / q, a]),
            Kind::LowPass => ([0.0, 0.0, 1.0], [1.0, 1.0 / q, 1.0]),
            _ => ([1.0, 0.0, 0.0], [1.0, 1.0 / q, 1.0]),
        };
        let x = (hz / f0) as f64;
        let m = ((n[2] - n[0] * x * x).powi(2) + (n[1] * x).powi(2)) / ((d[2] - d[0] * x * x).powi(2) + (d[1] * x).powi(2));
        (10.0 * m.log10()) as f32
    }

    #[test]
    fn matched_filters_follow_the_analog_curve() {
        let mut worst = (0.0f32, String::new());
        for fs in [44_100.0f32, 48_000.0, 96_000.0] {
            for kind in [Kind::Peak, Kind::LowShelf, Kind::HighShelf] {
                for f0 in [40.0f32, 300.0, 2500.0, 8000.0, 12_000.0, 16_000.0] {
                    for q in [0.3f32, 0.5, 0.8, 2.0, 6.0] {
                        for gain in [-9.0f32, -6.0, -3.0, 3.0, 6.0, 9.0] {
                            if kind != Kind::Peak && !(0.4..=1.0).contains(&q) {
                                continue;
                            }
                            let b = Biquad::matched(kind, fs, f0, q, gain);
                            // Stable: the poles are inside the unit circle.
                            assert!(b.a2.abs() < 1.0 && b.a1.abs() < 1.0 + b.a2, "{kind:?} {fs} {f0} {q} {gain}");
                            // Up to 0.42 fs: 18.5 kHz at 44.1 kHz.
                            for k in 0..160 {
                                let hz = 20.0 * (fs * 0.42 / 20.0).powf(k as f32 / 159.0);
                                let err = (b.response_db(fs, hz) - analog_db(kind, f0, q, gain, hz)).abs();
                                if err > worst.0 {
                                    worst = (err, format!("{kind:?} fs {fs} f0 {f0} q {q} gain {gain} at {hz:.0} Hz"));
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(worst.0 < 0.7, "worst error {:.2} dB: {}", worst.0, worst.1);
    }

    #[test]
    fn matched_boost_and_cut_cancel() {
        let fs = 44_100.0;
        for kind in [Kind::Peak, Kind::LowShelf, Kind::HighShelf] {
            let up = Biquad::matched(kind, fs, 9000.0, 0.8, 5.0);
            let down = Biquad::matched(kind, fs, 9000.0, 0.8, -5.0);
            for hz in [50.0, 900.0, 9000.0, 17_000.0] {
                let sum = up.response_db(fs, hz) + down.response_db(fs, hz);
                assert!(sum.abs() < 0.01, "{kind:?} at {hz}: {sum}");
            }
        }
    }

    #[test]
    fn matched_bell_beats_the_cookbook_up_high() {
        // +6 dB at 12 kHz, Q 1, at 44.1 kHz: the cookbook bell is far off the analog curve at
        // 18 kHz, the matched one is on it.
        let fs = 44_100.0;
        let want = analog_db(Kind::Peak, 12_000.0, 1.0, 6.0, 18_000.0);
        let cookbook = Biquad::new(Kind::Peak, fs, 12_000.0, 1.0, 6.0).response_db(fs, 18_000.0);
        let matched = Biquad::matched(Kind::Peak, fs, 12_000.0, 1.0, 6.0).response_db(fs, 18_000.0);
        assert!((cookbook - want).abs() > 1.0, "cookbook {cookbook} vs analog {want}");
        assert!((matched - want).abs() < 0.4, "matched {matched} vs analog {want}");
        assert!((matched - want).abs() * 3.0 < (cookbook - want).abs());
    }

    #[test]
    fn matched_cuts_hold_their_slope_and_corner() {
        let fs = 48_000.0;
        let hp = Biquad::matched(Kind::HighPass, fs, 30.0, 0.707, 0.0);
        assert!((hp.response_db(fs, 30.0) - -3.0).abs() < 0.2, "{}", hp.response_db(fs, 30.0));
        assert!((hp.response_db(fs, 7.5) - -24.1).abs() < 0.6, "{}", hp.response_db(fs, 7.5));
        assert!(hp.response_db(fs, 1000.0).abs() < 0.05);
        let lp = Biquad::matched(Kind::LowPass, fs, 18_000.0, 0.707, 0.0);
        assert!((lp.response_db(fs, 18_000.0) - -3.0).abs() < 0.3, "{}", lp.response_db(fs, 18_000.0));
        assert!(lp.response_db(fs, 1000.0).abs() < 0.05);
        // Stable and usable as a running filter.
        let mut f = Biquad::matched(Kind::HighShelf, 44_100.0, 16_000.0, 0.7, 9.0);
        assert!((gain_at(&mut f, 44_100.0, 100.0)).abs() < 0.1);
    }
}
