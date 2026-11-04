//! The stereo half of the chain: doubler, tempo-synced delay and reverb. All three take the
//! mono vocal and return a stereo wet signal; the chain mixes them.

use super::filters::{one_pole, Biquad, Kind};

#[inline]
fn read_lerp(buf: &[f32], write: usize, delay: f32) -> f32 {
    let n = buf.len();
    let d = delay.clamp(1.0, (n - 2) as f32);
    let pos = write as f32 + n as f32 - d;
    let i = pos.floor();
    let f = pos - i;
    let i = i as usize;
    let a = buf[i % n];
    let b = buf[(i + 1) % n];
    a + (b - a) * f
}

/// Two micro-pitch-shifted copies, a few cents apart and a few ms late, panned left and right:
/// the studio trick behind wide, thick vocals.
pub struct Doubler {
    fs: f32,
    buf: Vec<f32>,
    w: usize,
    phase: [f32; 2],
    lfo: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct DoublerSettings {
    pub detune_cents: f32,
    pub delay_ms: f32,
    pub width: f32,
}

impl Doubler {
    pub fn new(fs: f32) -> Self {
        Self { fs, buf: vec![0.0; (fs * 0.2) as usize], w: 0, phase: [0.0, 0.5], lfo: 0.0 }
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
    }

    #[inline]
    pub fn tick(&mut self, x: f32, s: &DoublerSettings) -> (f32, f32) {
        self.buf[self.w] = x;
        let window = self.fs * 0.045;
        self.lfo = (self.lfo + 0.37 / self.fs) % 1.0;
        let wobble = (self.lfo * std::f32::consts::TAU).sin() * 0.0006 * self.fs;
        let mut voices = [0.0f32; 2];
        for v in 0..2 {
            let cents = if v == 0 { s.detune_cents } else { -s.detune_cents };
            let ratio = 2f32.powf(cents / 1200.0);
            // Two taps sweeping through a window at the rate that bends pitch by `ratio`,
            // crossfaded so the jump back is never heard.
            self.phase[v] = (self.phase[v] + (1.0 - ratio) / window).rem_euclid(1.0);
            let base = s.delay_ms * 0.001 * self.fs * if v == 0 { 1.0 } else { 1.31 } + if v == 0 { wobble } else { -wobble };
            let mut acc = 0.0;
            for t in 0..2 {
                let ph = (self.phase[v] + t as f32 * 0.5) % 1.0;
                let gain = (std::f32::consts::PI * ph).sin().powi(2);
                acc += gain * read_lerp(&self.buf, self.w, base + ph * window);
            }
            voices[v] = acc;
        }
        self.w = (self.w + 1) % self.buf.len();
        let spread = s.width.clamp(0.0, 1.0);
        let (a, b) = (0.5 + 0.5 * spread, 0.5 - 0.5 * spread);
        (voices[0] * a + voices[1] * b, voices[1] * a + voices[0] * b)
    }
}

/// Note lengths in beats for the delay's sync divisions.
pub const DIVISION_BEATS: [f32; 10] = [0.25, 1.0 / 3.0, 0.5, 0.75, 2.0 / 3.0, 1.0, 1.5, 2.0, 3.0, 4.0];

pub struct DelaySettings {
    pub seconds: f32,
    pub feedback: f32,
    pub ping_pong: bool,
    pub hp_hz: f32,
    pub lp_hz: f32,
}

pub struct Delay {
    fs: f32,
    l: Vec<f32>,
    r: Vec<f32>,
    w: usize,
    len: f32,
    filt: [[Biquad; 2]; 2],
}

impl Delay {
    pub fn new(fs: f32) -> Self {
        let n = (fs * 4.2) as usize;
        Self { fs, l: vec![0.0; n], r: vec![0.0; n], w: 0, len: fs * 0.25, filt: [[Biquad::default(); 2]; 2] }
    }

    pub fn reset(&mut self) {
        self.l.fill(0.0);
        self.r.fill(0.0);
        for f in self.filt.iter_mut().flatten() {
            f.reset();
        }
    }

    pub fn set_filters(&mut self, hp: f32, lp: f32) {
        for ch in &mut self.filt {
            ch[0].set(Kind::HighPass, self.fs, hp, 0.707, 0.0);
            ch[1].set(Kind::LowPass, self.fs, lp, 0.707, 0.0);
        }
    }

    #[inline]
    pub fn tick(&mut self, x: f32, s: &DelaySettings) -> (f32, f32) {
        let target = (s.seconds * self.fs).clamp(1.0, self.l.len() as f32 - 4.0);
        // Glide to a new time instead of jumping: a jump is a click.
        self.len += (target - self.len) * 0.0005;
        let dl = read_lerp(&self.l, self.w, self.len);
        let dr = read_lerp(&self.r, self.w, self.len);
        let fl = self.filt[0][0].tick(dl);
        let fl = self.filt[0][1].tick(fl);
        let fr = self.filt[1][0].tick(dr);
        let fr = self.filt[1][1].tick(fr);
        let fb = s.feedback.clamp(0.0, 0.95);
        if s.ping_pong {
            // In on the left, each repeat crosses to the other side.
            self.l[self.w] = x + fr * fb;
            self.r[self.w] = fl * fb;
        } else {
            self.l[self.w] = x + fl * fb;
            self.r[self.w] = x + fr * fb;
        }
        self.w = (self.w + 1) % self.l.len();
        // Every repeat, the first included, comes back through the filters.
        (fl, fr)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReverbType {
    Plate,
    Room,
    Hall,
    Chamber,
}

pub struct ReverbSettings {
    pub kind: ReverbType,
    pub size: f32,
    pub decay_s: f32,
    pub predelay_ms: f32,
    pub damping: f32,
}

struct Allpass {
    buf: Vec<f32>,
    w: usize,
}

impl Allpass {
    fn new(n: usize) -> Self {
        Self { buf: vec![0.0; n.max(1)], w: 0 }
    }
    #[inline]
    fn tick(&mut self, x: f32, g: f32) -> f32 {
        let d = self.buf[self.w];
        let y = -g * x + d;
        self.buf[self.w] = x + g * y;
        self.w = (self.w + 1) % self.buf.len();
        y
    }
}

const LINES: usize = 8;
const BASE_MS: [f32; LINES] = [29.7, 37.1, 41.1, 43.7, 53.3, 59.9, 67.3, 79.1];

/// An 8-line feedback delay network with a Householder mix, input diffusion, per-line damping
/// and a slow modulation that keeps it from ringing metallic.
pub struct Reverb {
    fs: f32,
    pre: Vec<f32>,
    pre_w: usize,
    lowcut: Biquad,
    diffusers: [Allpass; 4],
    lines: Vec<Vec<f32>>,
    w: usize,
    lengths: [f32; LINES],
    gains: [f32; LINES],
    damp_state: [f32; LINES],
    damp_k: f32,
    lfo: f32,
    configured: (ReverbType, f32, f32, f32),
}

impl Reverb {
    pub fn new(fs: f32) -> Self {
        let max_line = (fs * 0.08 * 2.2) as usize + 8;
        let diffusers = [4.7f32, 3.6, 12.7, 9.3].map(|ms| Allpass::new((ms * 0.001 * fs) as usize));
        Self {
            fs,
            pre: vec![0.0; (fs * 0.2) as usize],
            pre_w: 0,
            lowcut: Biquad::default(),
            diffusers,
            lines: (0..LINES).map(|_| vec![0.0; max_line]).collect(),
            w: 0,
            lengths: [1000.0; LINES],
            gains: [0.5; LINES],
            damp_state: [0.0; LINES],
            damp_k: 0.5,
            lfo: 0.0,
            configured: (ReverbType::Plate, -1.0, -1.0, -1.0),
        }
    }

    pub fn reset(&mut self) {
        self.pre.fill(0.0);
        for l in &mut self.lines {
            l.fill(0.0);
        }
        for d in &mut self.diffusers {
            d.buf.fill(0.0);
        }
        self.damp_state = [0.0; LINES];
        self.lowcut.reset();
    }

    pub fn set_lowcut(&mut self, hz: f32) {
        self.lowcut.set(Kind::HighPass, self.fs, hz, 0.707, 0.0);
    }

    fn configure(&mut self, s: &ReverbSettings) {
        let key = (s.kind, s.size, s.decay_s, s.damping);
        if self.configured == key {
            return;
        }
        self.configured = key;
        let shape = match s.kind {
            ReverbType::Room => 0.55,
            ReverbType::Plate => 0.85,
            ReverbType::Chamber => 1.1,
            ReverbType::Hall => 1.6,
        };
        let scale = shape * (0.45 + s.size.clamp(0.0, 1.0) * 0.9);
        let max = self.lines[0].len() as f32 - 4.0;
        for i in 0..LINES {
            self.lengths[i] = (BASE_MS[i] * 0.001 * self.fs * scale).clamp(16.0, max);
            // Each line loses 60 dB over the decay time.
            self.gains[i] = 10f32.powf(-3.0 * self.lengths[i] / (s.decay_s.max(0.1) * self.fs));
        }
        let cutoff = 16_000.0 * (1.0 - s.damping.clamp(0.0, 1.0)) + 2_500.0 * s.damping.clamp(0.0, 1.0);
        self.damp_k = 1.0 - (-std::f32::consts::TAU * cutoff / self.fs).exp();
    }

    #[inline]
    pub fn tick(&mut self, x: f32, s: &ReverbSettings) -> (f32, f32) {
        self.configure(s);
        self.pre[self.pre_w] = x;
        let pre_n = self.pre.len();
        let pd = (s.predelay_ms * 0.001 * self.fs).clamp(0.0, pre_n as f32 - 2.0) as usize;
        let delayed = self.pre[(self.pre_w + pre_n - pd) % pre_n];
        self.pre_w = (self.pre_w + 1) % pre_n;
        let mut v = self.lowcut.tick(delayed);
        let g = if s.kind == ReverbType::Room { 0.6 } else { 0.72 };
        for d in &mut self.diffusers {
            v = d.tick(v, g);
        }
        self.lfo = (self.lfo + 0.23 / self.fs) % 1.0;
        let m = (self.lfo * std::f32::consts::TAU).sin() * 0.0005 * self.fs;
        let mut outs = [0.0f32; LINES];
        for i in 0..LINES {
            let wobble = if i % 3 == 0 { m } else if i % 3 == 1 { -m } else { 0.0 };
            outs[i] = read_lerp(&self.lines[i], self.w, self.lengths[i] + wobble);
        }
        // Householder: every line feeds every other, energy preserved.
        let sum: f32 = outs.iter().sum::<f32>() * (2.0 / LINES as f32);
        let n = self.lines[0].len();
        for i in 0..LINES {
            let fb = (outs[i] - sum) * self.gains[i];
            self.damp_state[i] += (fb - self.damp_state[i]) * self.damp_k;
            let input = if i % 2 == 0 { v } else { -v };
            self.lines[i][self.w] = self.damp_state[i] + input * 0.35;
        }
        self.w = (self.w + 1) % n;
        let l = outs[0] - outs[2] + outs[4] - outs[6] + 0.5 * (outs[1] + outs[5]);
        let r = outs[1] - outs[3] + outs[5] - outs[7] + 0.5 * (outs[2] + outs[6]);
        (l * 0.5, r * 0.5)
    }
}

/// Follows the dry vocal so delay and reverb can sit back while the words are being said.
pub struct DuckFollower {
    env: f32,
    att: f32,
    rel: f32,
}

impl DuckFollower {
    pub fn new(fs: f32) -> Self {
        Self { env: 0.0, att: one_pole(fs, 0.005), rel: one_pole(fs, 0.25) }
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let a = x.abs();
        let k = if a > self.env { self.att } else { self.rel };
        self.env += (a - self.env) * k;
        self.env
    }

    /// Wet gain for a duck amount 0..1: full depth once the vocal is near -20 dBFS.
    #[inline]
    pub fn gain(&self, amount: f32) -> f32 {
        1.0 - amount.clamp(0.0, 1.0) * 0.85 * (self.env / 0.1).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_repeats_on_time() {
        let fs = 48_000.0;
        let mut d = Delay::new(fs);
        d.set_filters(20.0, 20_000.0);
        let s = DelaySettings { seconds: 0.25, feedback: 0.0, ping_pong: false, hp_hz: 20.0, lp_hz: 20_000.0 };
        d.len = 0.25 * fs;
        let mut first = None;
        for i in 0..24_000 {
            let (l, _) = d.tick(if i == 0 { 1.0 } else { 0.0 }, &s);
            if first.is_none() && l.abs() > 0.3 {
                first = Some(i);
            }
        }
        assert_eq!(first, Some(12_000));
    }

    #[test]
    fn reverb_decays_and_stays_finite() {
        let fs = 48_000.0;
        let mut r = Reverb::new(fs);
        r.set_lowcut(100.0);
        let s = ReverbSettings { kind: ReverbType::Hall, size: 0.7, decay_s: 1.0, predelay_ms: 10.0, damping: 0.5 };
        let mut early = 0.0f32;
        let mut late = 0.0f32;
        for i in 0..(fs as usize * 3) {
            let x = if i < 480 { 0.5 } else { 0.0 };
            let (l, rr) = r.tick(x, &s);
            assert!(l.is_finite() && rr.is_finite());
            if (4800..9600).contains(&i) {
                early = early.max(l.abs());
            }
            if i > fs as usize * 2 {
                late = late.max(l.abs());
            }
        }
        assert!(early > 1e-3, "no tail");
        assert!(late < early * 0.01, "tail doesn't decay: early {early}, late {late}");
    }

    #[test]
    fn doubler_is_wide_and_finite() {
        let fs = 48_000.0;
        let mut d = Doubler::new(fs);
        let s = DoublerSettings { detune_cents: 10.0, delay_ms: 15.0, width: 1.0 };
        let (mut diff, mut sum) = (0.0f32, 0.0f32);
        for i in 0..48_000 {
            let x = 0.3 * (std::f32::consts::TAU * 220.0 * i as f32 / fs).sin();
            let (l, r) = d.tick(x, &s);
            assert!(l.is_finite() && r.is_finite());
            if i > 24_000 {
                diff += (l - r).abs();
                sum += (l + r).abs();
            }
        }
        assert!(diff > sum * 0.2, "left and right are nearly identical");
    }
}
