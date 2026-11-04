//! Gate, de-esser and the two compressors. Every processor returns the gain reduction it
//! applied, in dB, for the meters.

use super::filters::{db_to_gain, gain_to_db, one_pole, Biquad, Kind};

/// Downward expander for bedroom noise between phrases: hysteresis so it doesn't chatter,
/// a hold so word endings aren't chopped.
pub struct Gate {
    fs: f32,
    env: f32,
    gain: f32,
    open: bool,
    hold: usize,
    k: [f32; 4],
}

impl Gate {
    pub fn new(fs: f32) -> Self {
        let k = [one_pole(fs, 0.0005), one_pole(fs, 0.05), one_pole(fs, 0.001), one_pole(fs, 0.08)];
        Self { fs, env: 0.0, gain: 1.0, open: false, hold: 0, k }
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.gain = 1.0;
        self.open = false;
        self.hold = 0;
    }

    #[inline]
    pub fn tick(&mut self, x: f32, thresh_db: f32, range_db: f32) -> f32 {
        let a = x.abs();
        let k = if a > self.env { self.k[0] } else { self.k[1] };
        self.env += (a - self.env) * k;
        let level = gain_to_db(self.env);
        if level > thresh_db {
            self.open = true;
            self.hold = (self.fs * 0.04) as usize;
        } else if level < thresh_db - 4.0 {
            if self.hold > 0 {
                self.hold -= 1;
            } else {
                self.open = false;
            }
        }
        let target = if self.open { 1.0 } else { db_to_gain(-range_db) };
        let k = if target > self.gain { self.k[2] } else { self.k[3] };
        self.gain += (target - self.gain) * k;
        x * self.gain
    }

    pub fn reduction_db(&self) -> f32 {
        -gain_to_db(self.gain)
    }
}

/// De-esser: a zero-phase band around the sibilance frequency is detected and subtracted back
/// out by the gain reduction (y = x - (1 - g)·BP(x)), so the cut is exact at its centre, falls
/// away smoothly either side, and leaves the signal untouched when it isn't working. (Taking a
/// high-passed copy out instead leaves most of the "s" behind: that filter shifts the phase.)
pub struct DeEsser {
    fs: f32,
    split: Biquad,
    env: f32,
    gain: f32,
    k: [f32; 4],
}

impl DeEsser {
    pub fn new(fs: f32) -> Self {
        let k = [one_pole(fs, 0.0003), one_pole(fs, 0.04), one_pole(fs, 0.0005), one_pole(fs, 0.03)];
        Self { fs, split: Biquad::default(), env: 0.0, gain: 1.0, k }
    }

    pub fn reset(&mut self) {
        self.split.reset();
        self.env = 0.0;
        self.gain = 1.0;
    }

    pub fn set(&mut self, freq: f32) {
        self.split.set(Kind::BandPass, self.fs, freq, 0.9, 0.0);
    }

    #[inline]
    pub fn tick(&mut self, x: f32, thresh_db: f32, range_db: f32) -> f32 {
        let band = self.split.tick(x);
        let a = band.abs();
        let k = if a > self.env { self.k[0] } else { self.k[1] };
        self.env += (a - self.env) * k;
        // ~5:1 above the threshold, capped at the range.
        let over = gain_to_db(self.env) - thresh_db;
        let gr = (over * 0.8).clamp(0.0, range_db);
        let target = db_to_gain(-gr);
        let k = if target < self.gain { self.k[2] } else { self.k[3] };
        self.gain += (target - self.gain) * k;
        x - (1.0 - self.gain) * band
    }

    pub fn reduction_db(&self) -> f32 {
        -gain_to_db(self.gain)
    }
}

/// The "Punch" compressor, after the FET character: fast, peak-sensing, soft knee, with a
/// touch of harmonic colour that grows with how hard it's working.
pub struct Fet {
    fs: f32,
    gr: f32,
    times: (f32, f32),
    coefs: (f32, f32),
}

#[derive(Clone, Copy, Debug)]
pub struct FetSettings {
    pub thresh_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
    pub color: f32,
}

impl Fet {
    pub fn new(fs: f32) -> Self {
        Self { fs, gr: 0.0, times: (-1.0, -1.0), coefs: (1.0, 1.0) }
    }

    pub fn reset(&mut self) {
        self.gr = 0.0;
    }

    #[inline]
    pub fn tick(&mut self, x: f32, s: &FetSettings) -> f32 {
        let level = gain_to_db(x.abs());
        let knee = 6.0;
        let over = level - s.thresh_db;
        let slope = 1.0 / s.ratio.max(1.0) - 1.0;
        let want = if over <= -knee / 2.0 {
            0.0
        } else if over < knee / 2.0 {
            slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
        } else {
            slope * over
        };
        if self.times != (s.attack_ms, s.release_ms) {
            self.times = (s.attack_ms, s.release_ms);
            self.coefs = (one_pole(self.fs, s.attack_ms * 0.001), one_pole(self.fs, s.release_ms * 0.001));
        }
        let k = if want < self.gr { self.coefs.0 } else { self.coefs.1 };
        self.gr += (want - self.gr) * k;
        let y = x * db_to_gain(self.gr + s.makeup_db);
        if s.color > 0.0 {
            // Colour follows the gain reduction, like a transformer pushed harder.
            let amt = s.color * (0.15 + (-self.gr / 12.0).min(1.0) * 0.85);
            let sat = (y * 1.6).tanh() / 1.6;
            y + (sat - y) * amt
        } else {
            y
        }
    }

    pub fn reduction_db(&self) -> f32 {
        -self.gr
    }
}

/// The "Level" compressor, after the opto character: RMS-sensing, gentle, and a release that
/// slows down the longer it has been working, which is what makes it sound smooth.
pub struct Opto {
    rms: f32,
    gr: f32,
    history: f32,
    k_rms: f32,
    k_hist: f32,
    k_att: f32,
    k_fast: f32,
    k_slow: f32,
}

impl Opto {
    pub fn new(fs: f32) -> Self {
        Self {
            rms: 0.0,
            gr: 0.0,
            history: 0.0,
            k_rms: one_pole(fs, 0.01),
            k_hist: one_pole(fs, 2.0),
            k_att: one_pole(fs, 0.010),
            k_fast: one_pole(fs, 0.06),
            k_slow: one_pole(fs, 1.46),
        }
    }

    pub fn reset(&mut self) {
        self.rms = 0.0;
        self.gr = 0.0;
        self.history = 0.0;
    }

    /// `amount` 0..1 is the "peak reduction" knob.
    #[inline]
    pub fn tick(&mut self, x: f32, amount: f32, makeup_db: f32) -> f32 {
        self.rms += (x * x - self.rms) * self.k_rms;
        let level = 10.0 * self.rms.max(1e-12).log10();
        let thresh = -8.0 - amount * 32.0;
        let knee = 10.0;
        let over = level - thresh;
        let slope = 1.0 / 3.0 - 1.0;
        let want = if amount <= 0.001 || over <= -knee / 2.0 {
            0.0
        } else if over < knee / 2.0 {
            slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
        } else {
            slope * over
        };
        self.history += (-self.gr - self.history) * self.k_hist;
        let slow = (self.history / 8.0).clamp(0.0, 1.0);
        let release = self.k_fast + (self.k_slow - self.k_fast) * slow;
        let k = if want < self.gr { self.k_att } else { release };
        self.gr += (want - self.gr) * k;
        x * db_to_gain(self.gr + makeup_db)
    }

    pub fn reduction_db(&self) -> f32 {
        -self.gr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle<F: FnMut(f32) -> f32>(mut f: F, amp: f32, hz: f32) -> f32 {
        let fs = 48_000.0;
        let mut peak = 0.0f32;
        for i in 0..48_000 {
            let y = f(amp * (std::f32::consts::TAU * hz * i as f32 / fs).sin());
            if i > 36_000 {
                peak = peak.max(y.abs());
            }
        }
        gain_to_db(peak)
    }

    #[test]
    fn fet_compresses_by_the_ratio() {
        let mut c = Fet::new(48_000.0);
        let s = FetSettings { thresh_db: -20.0, ratio: 4.0, attack_ms: 1.0, release_ms: 50.0, makeup_db: 0.0, color: 0.0 };
        // A 0 dBFS sine sits 20 dB over: 4:1 leaves 5 dB of it.
        let out = settle(|x| c.tick(x, &s), 1.0, 200.0);
        assert!((out - -15.0).abs() < 1.5, "{out}");
        assert!((c.reduction_db() - 15.0).abs() < 2.0);
    }

    #[test]
    fn de_esser_only_touches_the_highs() {
        let fs = 48_000.0;
        let mut d = DeEsser::new(fs);
        d.set(6000.0);
        let low = settle(|x| d.tick(x, -30.0, 10.0), 0.5, 300.0);
        assert!((low - gain_to_db(0.5)).abs() < 0.5, "low band changed: {low}");
        d.reset();
        let high = settle(|x| d.tick(x, -30.0, 10.0), 0.5, 6000.0);
        assert!(high < gain_to_db(0.5) - 6.0, "sibilance not reduced: {high}");
    }

    #[test]
    fn gate_closes_on_noise_floor() {
        let mut g = Gate::new(48_000.0);
        let quiet = settle(|x| g.tick(x, -50.0, 20.0), 0.001, 400.0);
        assert!(quiet < gain_to_db(0.001) - 15.0);
        let loud = settle(|x| g.tick(x, -50.0, 20.0), 0.3, 400.0);
        assert!((loud - gain_to_db(0.3)).abs() < 0.5);
    }
}
