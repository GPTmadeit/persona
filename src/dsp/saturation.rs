//! Saturation characters. The nonlinear stages run at twice the sample rate behind a half-band
//! filter, so the harmonics they make above Nyquist don't fold back down as fizz.

use super::filters::{Biquad, DcBlock, Kind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SatMode {
    Tape,
    Tube,
    Fuzz,
    Phone,
    LoFi,
}

const TAPS: usize = 31;

/// 31-tap Blackman-windowed half-band, unity at DC.
fn halfband() -> [f32; TAPS] {
    let c = TAPS / 2;
    let mut h = [0.0f32; TAPS];
    let mut sum = 0.0;
    for (i, t) in h.iter_mut().enumerate() {
        let k = i as f64 - c as f64;
        let ideal = if k == 0.0 {
            0.5
        } else if (k as i64) % 2 == 0 {
            0.0
        } else {
            (0.5 * std::f64::consts::PI * k).sin() / (std::f64::consts::PI * k)
        };
        let w = 0.42 + 0.5 * (std::f64::consts::PI * k / c as f64).cos() + 0.08 * (2.0 * std::f64::consts::PI * k / c as f64).cos();
        *t = (ideal * w) as f32;
        sum += ideal * w;
    }
    for t in &mut h {
        *t /= sum as f32;
    }
    h
}

struct Oversampler {
    h: [f32; TAPS],
    up: [f32; TAPS],
    down: [f32; TAPS],
    up_pos: usize,
    down_pos: usize,
}

impl Oversampler {
    fn new() -> Self {
        Self { h: halfband(), up: [0.0; TAPS], down: [0.0; TAPS], up_pos: 0, down_pos: 0 }
    }

    fn reset(&mut self) {
        self.up = [0.0; TAPS];
        self.down = [0.0; TAPS];
    }

    #[inline]
    fn fir(h: &[f32; TAPS], line: &[f32; TAPS], pos: usize) -> f32 {
        let mut acc = 0.0;
        for (i, &c) in h.iter().enumerate() {
            acc += c * line[(pos + i) % TAPS];
        }
        acc
    }

    /// One input sample to two at the doubled rate (zero-stuffed, gain 2, filtered).
    #[inline]
    fn upsample(&mut self, x: f32) -> [f32; 2] {
        let mut out = [0.0; 2];
        for (j, v) in [x * 2.0, 0.0].into_iter().enumerate() {
            self.up_pos = (self.up_pos + TAPS - 1) % TAPS;
            self.up[self.up_pos] = v;
            out[j] = Self::fir(&self.h, &self.up, self.up_pos);
        }
        out
    }

    /// Two doubled-rate samples back to one.
    #[inline]
    fn downsample(&mut self, y: [f32; 2]) -> f32 {
        let mut out = 0.0;
        for (j, v) in y.into_iter().enumerate() {
            self.down_pos = (self.down_pos + TAPS - 1) % TAPS;
            self.down[self.down_pos] = v;
            if j == 1 {
                out = Self::fir(&self.h, &self.down, self.down_pos);
            }
        }
        out
    }
}

pub struct Saturator {
    fs: f32,
    os: Oversampler,
    dc: DcBlock,
    pre: [Biquad; 2],
    post: [Biquad; 2],
    mode: Option<SatMode>,
    hold: f32,
    hold_count: f32,
}

impl Saturator {
    pub fn new(fs: f32) -> Self {
        Self {
            fs,
            os: Oversampler::new(),
            dc: DcBlock::default(),
            pre: [Biquad::default(); 2],
            post: [Biquad::default(); 2],
            mode: None,
            hold: 0.0,
            hold_count: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.os.reset();
        self.dc = DcBlock::default();
        for b in self.pre.iter_mut().chain(self.post.iter_mut()) {
            b.reset();
        }
    }

    fn set_mode(&mut self, m: SatMode) {
        if self.mode == Some(m) {
            return;
        }
        self.mode = Some(m);
        let fs = self.fs;
        let (pre, post) = match m {
            // Tape: a gentle pre-emphasis lift into the saturation, taken back out after.
            SatMode::Tape => (
                [Biquad::new(Kind::HighShelf, fs, 3000.0, 0.7, 3.0), Biquad::new(Kind::Peak, fs, 120.0, 0.8, 1.0)],
                [Biquad::new(Kind::HighShelf, fs, 3000.0, 0.7, -3.5), Biquad::new(Kind::LowPass, fs, 16000.0, 0.7, 0.0)],
            ),
            SatMode::Tube => (
                [Biquad::new(Kind::Peak, fs, 1000.0, 0.6, 0.0), Biquad::new(Kind::Peak, fs, 1000.0, 0.6, 0.0)],
                [Biquad::new(Kind::LowPass, fs, 14000.0, 0.7, 0.0), Biquad::new(Kind::Peak, fs, 1000.0, 0.6, 0.0)],
            ),
            SatMode::Fuzz => (
                [Biquad::new(Kind::HighPass, fs, 150.0, 0.7, 0.0), Biquad::new(Kind::Peak, fs, 1800.0, 0.8, 3.0)],
                [Biquad::new(Kind::LowPass, fs, 7500.0, 0.7, 0.0), Biquad::new(Kind::Peak, fs, 1000.0, 0.6, 0.0)],
            ),
            // Phone: the 300 Hz - 3.4 kHz band a telephone line carries.
            SatMode::Phone => (
                [Biquad::new(Kind::HighPass, fs, 380.0, 0.9, 0.0), Biquad::new(Kind::LowPass, fs, 3400.0, 0.9, 0.0)],
                [Biquad::new(Kind::HighPass, fs, 380.0, 0.9, 0.0), Biquad::new(Kind::Peak, fs, 1800.0, 1.2, 4.0)],
            ),
            SatMode::LoFi => (
                [Biquad::new(Kind::HighPass, fs, 120.0, 0.7, 0.0), Biquad::new(Kind::LowPass, fs, 6000.0, 0.7, 0.0)],
                [Biquad::new(Kind::LowPass, fs, 5500.0, 0.7, 0.0), Biquad::new(Kind::Peak, fs, 1000.0, 0.6, 0.0)],
            ),
        };
        self.pre = pre;
        self.post = post;
    }

    #[inline]
    fn shape(mode: SatMode, x: f32) -> f32 {
        match mode {
            SatMode::Tape => {
                // Soft, slightly asymmetric: tape's mostly odd harmonics with a little even.
                let b = 0.08;
                (x + b).tanh() - b.tanh()
            }
            SatMode::Tube => {
                // Asymmetric triode-ish curve: the even harmonics are what read as "warm".
                let y = x + 0.25 * x * x;
                if y >= 0.0 {
                    y.tanh()
                } else {
                    (y * 0.85).tanh() / 0.85
                }
            }
            SatMode::Fuzz => (x * 2.5).atan() * (2.0 / std::f32::consts::PI),
            SatMode::Phone | SatMode::LoFi => (x * 1.4).tanh(),
        }
    }

    /// The static curve at this drive, level-normalised like `tick`; for drawing.
    pub fn curve(mode: SatMode, drive: f32, x: f32) -> f32 {
        let g = 10f32.powf(drive.clamp(0.0, 1.0) * 1.4);
        let reference = 0.25;
        Self::shape(mode, g * x) * reference / Self::shape(mode, g * reference).abs().max(1e-6)
    }

    /// `drive` and `mix` are 0..1.
    #[inline]
    pub fn tick(&mut self, x: f32, mode: SatMode, drive: f32, mix: f32) -> f32 {
        self.set_mode(mode);
        let mut v = x;
        for b in &mut self.pre {
            v = b.tick(v);
        }
        let g = 10f32.powf(drive.clamp(0.0, 1.0) * 1.4);
        // Normalise so a typical vocal peak (-12 dBFS) comes out at about the level it went in.
        let reference = 0.25;
        let norm = reference / Self::shape(mode, g * reference).abs().max(1e-6);
        let wet = if mode == SatMode::LoFi {
            // Sample-rate and bit-depth reduction, then the drive.
            let factor = 1.0 + drive * 5.0;
            self.hold_count += 1.0;
            if self.hold_count >= factor {
                self.hold_count -= factor;
                let bits = 12.0 - drive * 5.0;
                let q = 2f32.powf(bits);
                self.hold = (v * q).round() / q;
            }
            Self::shape(mode, g * self.hold) * norm
        } else {
            let up = self.os.upsample(v);
            let y = [Self::shape(mode, g * up[0]), Self::shape(mode, g * up[1])];
            self.os.downsample(y) * norm
        };
        let mut w = self.dc.tick(wet);
        for b in &mut self.post {
            w = b.tick(w);
        }
        x + (w - x) * mix.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_is_finite_and_level_matched() {
        let fs = 48_000.0;
        for mode in [SatMode::Tape, SatMode::Tube, SatMode::Fuzz, SatMode::Phone, SatMode::LoFi] {
            let mut s = Saturator::new(fs);
            let (mut pi, mut po) = (0.0f32, 0.0f32);
            for i in 0..48_000 {
                let x = 0.25 * (std::f32::consts::TAU * 800.0 * i as f32 / fs).sin();
                let y = s.tick(x, mode, 0.6, 1.0);
                assert!(y.is_finite());
                if i > 24_000 {
                    pi = pi.max(x.abs());
                    po = po.max(y.abs());
                }
            }
            let db = 20.0 * (po / pi).log10();
            assert!(db.abs() < 9.0, "{mode:?} changed the level by {db:.1} dB");
        }
    }
}
