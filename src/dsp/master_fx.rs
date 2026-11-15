//! The smaller mastering processors: the bus ("glue") compressor, a band compressor for the
//! multiband stage, the harmonic exciter, and dither.

use super::filters::{db_to_gain, gain_to_db, one_pole, Biquad, Kind};
use super::saturation::Oversampler;

/// Static gain computer with a soft knee: gain change in dB (<= 0) for `level_db`.
#[inline]
pub fn gain_computer(level_db: f32, thresh_db: f32, ratio: f32, knee_db: f32) -> f32 {
    let over = level_db - thresh_db;
    let slope = 1.0 / ratio.max(1.0) - 1.0;
    if over <= -knee_db / 2.0 {
        0.0
    } else if over < knee_db / 2.0 {
        slope * (over + knee_db / 2.0).powi(2) / (2.0 * knee_db.max(1e-3))
    } else {
        slope * over
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GlueSettings {
    pub thresh_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    /// 0 = fixed release; 1 = a release that follows the programme (fast after a transient,
    /// slow on sustained compression).
    pub auto_release: bool,
    pub makeup_db: f32,
    /// Side-chain high-pass, Hz: keeps the kick from pumping the whole mix.
    pub sc_hp: f32,
    /// Parallel blend, 0..1.
    pub mix: f32,
}

/// A stereo-linked VCA-style bus compressor, the kind that "glues" a mix: RMS-ish detection,
/// soft knee, side-chain high-pass.
pub struct Glue {
    fs: f32,
    sc: [Biquad; 2],
    det: f32,
    det_k: f32,
    gr: f32,
    slow: f32,
    times: (f32, f32),
    coefs: (f32, f32),
}

impl Glue {
    pub fn new(fs: f32) -> Self {
        Self { fs, sc: [Biquad::default(); 2], det: 0.0, det_k: one_pole(fs, 0.005), gr: 0.0, slow: 0.0, times: (-1.0, -1.0), coefs: (1.0, 1.0) }
    }

    pub fn reset(&mut self) {
        for b in &mut self.sc {
            b.reset();
        }
        self.det = 0.0;
        self.gr = 0.0;
        self.slow = 0.0;
    }

    /// Returns the gain to apply to both channels (wet), and keeps the GR for metering.
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32, s: &GlueSettings) -> (f32, f32) {
        let fs = self.fs;
        for b in &mut self.sc {
            b.set(Kind::HighPass, fs, s.sc_hp, 0.707, 0.0);
        }
        let a = self.sc[0].tick(l);
        let b = self.sc[1].tick(r);
        let p = (a * a).max(b * b);
        self.det += (p - self.det) * self.det_k;
        let level = 10.0 * self.det.max(1e-12).log10() + 3.0;
        let want = gain_computer(level, s.thresh_db, s.ratio, 6.0);
        if self.times != (s.attack_ms, s.release_ms) {
            self.times = (s.attack_ms, s.release_ms);
            self.coefs = (one_pole(fs, s.attack_ms * 0.001), one_pole(fs, s.release_ms * 0.001));
        }
        if want < self.gr {
            self.gr += (want - self.gr) * self.coefs.0;
        } else {
            let k = if s.auto_release {
                // Long compression gets a long release; short bursts recover quickly.
                let rel = 0.05 + 0.75 * (-self.slow / 6.0).clamp(0.0, 1.0);
                one_pole(fs, rel)
            } else {
                self.coefs.1
            };
            self.gr += (want - self.gr) * k;
        }
        self.slow += (self.gr - self.slow) * one_pole(fs, 0.8);
        // Parallel blend: 1 - mix of the untouched signal under the compressed one.
        let g = 1.0 + (db_to_gain(self.gr + s.makeup_db) - 1.0) * s.mix;
        (l * g, r * g)
    }

    pub fn reduction_db(&self) -> f32 {
        -self.gr
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BandSettings {
    pub thresh_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub gain_db: f32,
}

/// One band of the multiband compressor, stereo-linked, peak-envelope detection.
#[derive(Clone, Copy)]
pub struct BandComp {
    fs: f32,
    env: f32,
    env_k: f32,
    gr: f32,
    times: (f32, f32),
    coefs: (f32, f32),
}

impl BandComp {
    pub fn new(fs: f32) -> Self {
        Self { fs, env: 0.0, env_k: one_pole(fs, 0.003), gr: 0.0, times: (-1.0, -1.0), coefs: (1.0, 1.0) }
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.gr = 0.0;
    }

    /// Returns the linear gain for this sample of the band.
    #[inline]
    pub fn gain(&mut self, l: f32, r: f32, s: &BandSettings, active: bool) -> f32 {
        let p = l.abs().max(r.abs());
        let k = if p > self.env { 1.0 } else { self.env_k };
        self.env += (p - self.env) * k;
        let want = if active { gain_computer(gain_to_db(self.env), s.thresh_db, s.ratio, 4.0) } else { 0.0 };
        if self.times != (s.attack_ms, s.release_ms) {
            self.times = (s.attack_ms, s.release_ms);
            self.coefs = (one_pole(self.fs, s.attack_ms * 0.001), one_pole(self.fs, s.release_ms * 0.001));
        }
        let k = if want < self.gr { self.coefs.0 } else { self.coefs.1 };
        self.gr += (want - self.gr) * k;
        db_to_gain(self.gr + if active { s.gain_db } else { 0.0 })
    }

    pub fn reduction_db(&self) -> f32 {
        -self.gr
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExciteMode {
    /// Even harmonics: thick and warm.
    Tube,
    /// Soft odd harmonics with a little even: glue and weight.
    Tape,
    /// A harder odd curve: edge and bite, best in the top bands.
    Crisp,
}

/// Fixed delay of the exciter, base-rate samples: the 2x oversampler's two half-bands.
pub const EXCITE_DELAY: usize = 15;

/// Adds harmonics to one band of one channel. The dry signal is delayed to match the
/// oversampled path, so a band always comes out with the same delay whatever the amount.
pub struct Exciter {
    os: Oversampler,
    dry: [f32; EXCITE_DELAY],
    w: usize,
    dc_x: f32,
    dc_y: f32,
}

impl Default for Exciter {
    fn default() -> Self {
        Self::new()
    }
}

impl Exciter {
    pub fn new() -> Self {
        Self { os: Oversampler::new(), dry: [0.0; EXCITE_DELAY], w: 0, dc_x: 0.0, dc_y: 0.0 }
    }

    pub fn reset(&mut self) {
        self.os.reset();
        self.dry = [0.0; EXCITE_DELAY];
        self.dc_x = 0.0;
        self.dc_y = 0.0;
    }

    #[inline]
    fn shape(mode: ExciteMode, d: f32, x: f32) -> f32 {
        // Each curve has unity slope at zero, so quiet material keeps its level.
        match mode {
            ExciteMode::Tube => {
                let y = x + 0.35 * d * x * x;
                (d * y).tanh() / d
            }
            ExciteMode::Tape => (d * (x + 0.04)).tanh() / d - (d * 0.04).tanh() / d,
            ExciteMode::Crisp => (d * x * 1.3).atan() / (d * 1.3),
        }
    }

    /// `amount` 0..1.
    #[inline]
    pub fn tick(&mut self, x: f32, mode: ExciteMode, amount: f32) -> f32 {
        let dry = self.dry[self.w];
        self.dry[self.w] = x;
        self.w = (self.w + 1) % EXCITE_DELAY;
        if amount <= 0.0005 {
            // Keep the oversampler's history current so a fade-in doesn't click.
            let up = self.os.upsample(x);
            self.os.downsample(up);
            return dry;
        }
        let d = 1.0 + amount * 7.0;
        let up = self.os.upsample(x);
        let wet = self.os.downsample([Self::shape(mode, d, up[0]), Self::shape(mode, d, up[1])]);
        // DC from the asymmetric curves.
        let y = wet - self.dc_x + 0.9995 * self.dc_y;
        self.dc_x = wet;
        self.dc_y = y;
        dry + (y - dry) * amount.min(1.0)
    }
}

/// Rounds a peak off below `t` instead of letting it through: linear up to 70% of the
/// threshold, then a tanh shoulder that lands on the threshold and never passes it.
#[inline]
pub fn soft_clip(x: f32, t: f32) -> f32 {
    const KNEE: f32 = 0.7;
    let a = x.abs();
    if a <= KNEE * t {
        x
    } else {
        (t * (KNEE + (1.0 - KNEE) * ((a / t - KNEE) / (1.0 - KNEE)).tanh())).copysign(x)
    }
}

/// The maximizer's clipper: shaves the tops of drum hits so the limiter has less to do, which
/// is how loud masters keep their punch. Runs 2x oversampled; both channels always come out
/// `EXCITE_DELAY` samples late, clipping or not.
pub struct SoftClip {
    os: [Oversampler; 2],
    dry: [[f32; EXCITE_DELAY]; 2],
    w: usize,
    running: bool,
    /// Largest reduction since `take_reduction`, linear gain (1 = none).
    low: f32,
}

impl Default for SoftClip {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftClip {
    pub fn new() -> Self {
        Self { os: [Oversampler::new(), Oversampler::new()], dry: [[0.0; EXCITE_DELAY]; 2], w: 0, running: false, low: 1.0 }
    }

    pub fn reset(&mut self) {
        for o in &mut self.os {
            o.reset();
        }
        self.dry = [[0.0; EXCITE_DELAY]; 2];
        self.running = false;
        self.low = 1.0;
    }

    /// `threshold` is linear; `on` = false only delays.
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32, threshold: f32, on: bool) -> (f32, f32) {
        let dry = (self.dry[0][self.w], self.dry[1][self.w]);
        self.dry[0][self.w] = l;
        self.dry[1][self.w] = r;
        self.w = (self.w + 1) % EXCITE_DELAY;
        if !on {
            self.running = false;
            return dry;
        }
        if !self.running {
            // Coming in: the filters start from the last 15 samples, not from stale history.
            self.running = true;
            for (ch, o) in self.os.iter_mut().enumerate() {
                o.reset();
                for k in 0..EXCITE_DELAY {
                    let v = self.dry[ch][(self.w + k) % EXCITE_DELAY];
                    let up = o.upsample(v);
                    o.downsample(up);
                }
            }
            // The newest sample was just primed too, so this one comes from the dry line.
            return dry;
        }
        let mut out = [0.0f32; 2];
        for (ch, x) in [l, r].into_iter().enumerate() {
            let up = self.os[ch].upsample(x);
            let y = [soft_clip(up[0], threshold), soft_clip(up[1], threshold)];
            for (a, b) in up.iter().zip(y) {
                if a.abs() > 1e-6 {
                    self.low = self.low.min(b.abs() / a.abs());
                }
            }
            out[ch] = self.os[ch].downsample(y);
        }
        (out[0], out[1])
    }

    /// The deepest cut since the last call, dB (positive), for the meter.
    pub fn take_reduction_db(&mut self) -> f32 {
        let db = -gain_to_db(self.low);
        self.low = 1.0;
        db
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DitherBits {
    Off,
    Bits16,
    Bits24,
}

/// TPDF dither with optional first-order noise shaping, per channel.
pub struct Dither {
    seed: u32,
    err: [f32; 2],
}

impl Default for Dither {
    fn default() -> Self {
        Self { seed: 0x1234_5678, err: [0.0; 2] }
    }
}

impl Dither {
    #[inline]
    fn rand(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed as f32 / u32::MAX as f32
    }

    #[inline]
    pub fn tick(&mut self, ch: usize, x: f32, bits: DitherBits, shaped: bool) -> f32 {
        let q = match bits {
            DitherBits::Off => return x,
            DitherBits::Bits16 => 1.0 / 32768.0,
            DitherBits::Bits24 => 1.0 / 8_388_608.0,
        };
        let tpdf = (self.rand() - self.rand()) * q;
        let v = x - if shaped { self.err[ch] } else { 0.0 };
        let y = ((v + tpdf) / q).round() * q;
        self.err[ch] = y - v;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    #[test]
    fn glue_compresses_loud_and_leaves_quiet() {
        let fs = 48_000.0;
        let s = GlueSettings { thresh_db: -20.0, ratio: 4.0, attack_ms: 10.0, release_ms: 100.0, auto_release: false, makeup_db: 0.0, sc_hp: 20.0, mix: 1.0 };
        let mut g = Glue::new(fs);
        for i in 0..48_000 {
            let x = 0.5 * (TAU * 500.0 * i as f32 / fs).sin();
            g.tick(x, x, &s);
        }
        // A 0.5 sine reads -6 dB on the detector (RMS + 3, a sine's peak): 14 dB over at 4:1
        // is 10.5 dB of reduction.
        assert!((g.reduction_db() - 10.5).abs() < 1.0, "{}", g.reduction_db());
        let mut g = Glue::new(fs);
        for i in 0..48_000 {
            let x = 0.01 * (TAU * 500.0 * i as f32 / fs).sin();
            g.tick(x, x, &s);
        }
        assert!(g.reduction_db() < 0.05);
    }

    #[test]
    fn exciter_keeps_quiet_level_and_delay() {
        let fs = 48_000.0;
        let mut e = Exciter::new();
        let x: Vec<f32> = (0..4800).map(|i| 0.01 * (TAU * 300.0 * i as f32 / fs).sin()).collect();
        let y: Vec<f32> = x.iter().map(|v| e.tick(*v, ExciteMode::Tube, 0.6)).collect();
        let err = (2000..4800).map(|i| (y[i] - x[i - EXCITE_DELAY]).abs()).fold(0.0f32, f32::max);
        assert!(err < 0.0015, "{err}");
    }

    #[test]
    fn soft_clip_never_passes_its_threshold() {
        for i in 0..2000 {
            let x = (i as f32 - 1000.0) / 200.0;
            let y = soft_clip(x, 0.8);
            assert!(y.abs() <= 0.8 + 1e-6);
            if x.abs() <= 0.56 {
                assert_eq!(x, y);
            }
        }
        // Continuous across the knee.
        assert!((soft_clip(0.5601, 0.8) - soft_clip(0.5599, 0.8)).abs() < 1e-3);
    }

    #[test]
    fn clipper_delay_is_the_same_on_and_off() {
        let fs = 48_000.0;
        let x: Vec<f32> = (0..4800).map(|i| 0.2 * (TAU * 700.0 * i as f32 / fs).sin()).collect();
        for on in [false, true] {
            let mut c = SoftClip::new();
            let y: Vec<f32> = x.iter().map(|v| c.tick(*v, *v, 1.0, on).0).collect();
            // Far below the threshold the clipper is a delay (through the oversampler when on).
            let err = (200..4800).map(|i| (y[i] - x[i - EXCITE_DELAY]).abs()).fold(0.0f32, f32::max);
            assert!(err < 2e-3, "on={on}: {err}");
        }
        // Loud: the tops come off and the meter says so.
        let mut c = SoftClip::new();
        let mut peak = 0.0f32;
        for i in 0..4800 {
            let v = 1.5 * (TAU * 700.0 * i as f32 / fs).sin();
            peak = peak.max(c.tick(v, v, 0.7, true).0.abs());
        }
        assert!(peak < 0.76, "{peak}");
        assert!(c.take_reduction_db() > 5.0);
    }

    #[test]
    fn dither_is_quantised() {
        let mut d = Dither::default();
        for i in 0..1000 {
            let x = 0.3 * (i as f32 * 0.01).sin();
            let y = d.tick(0, x, DitherBits::Bits16, true);
            let steps = y * 32768.0;
            assert!((steps - steps.round()).abs() < 1e-3);
            assert!((y - x).abs() < 4.0 / 32768.0);
        }
    }
}
