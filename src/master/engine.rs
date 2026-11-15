//! The mastering chain, in order: input → parametric EQ (stereo, mid or side per band) →
//! Balance EQ → Glue → four bands of compression, harmonics and width → soft clip → maximizer
//! → output → dither. Every stage keeps its delay whether it is working or not, so the latency
//! reported to the host never changes.
//!
//! Plain Rust over plain settings, so the tests and the offline renderer drive it directly.

use std::sync::atomic::Ordering;

use crate::dsp::crossover::{ordered, Split4};
use crate::dsp::filters::{db_to_gain, gain_to_db, one_pole, Biquad, Kind};
use crate::dsp::loudness::{LoudnessMeter, TruePeak};
use crate::dsp::master_fx::{BandComp, BandSettings, Dither, DitherBits, ExciteMode, Exciter, Glue, GlueSettings, SoftClip, EXCITE_DELAY};
use crate::dsp::maximizer::Maximizer;

use super::params::{Channel, EqType, MasterParams, BAL_BANDS, BAL_HZ, EQ_BANDS, MB_BANDS};
use super::shared::*;

/// The crossover points Auto Master measures with, and sets.
pub const AUTO_XOVER: [f32; 3] = [120.0, 1000.0, 6000.0];

#[derive(Clone, Copy, Debug)]
pub struct EqBandSettings {
    pub on: bool,
    pub kind: EqType,
    pub freq: f32,
    pub gain: f32,
    pub q: f32,
    pub channel: Channel,
}

impl EqBandSettings {
    /// The filter this band is, for the chain and for drawing.
    pub fn filter(&self, fs: f32) -> Biquad {
        let mut b = Biquad::default();
        self.apply(&mut b, fs);
        b
    }

    fn apply(&self, b: &mut Biquad, fs: f32) {
        match self.kind {
            EqType::Bell => b.set_matched(Kind::Peak, fs, self.freq, self.q, self.gain),
            EqType::LowShelf => b.set_matched(Kind::LowShelf, fs, self.freq, self.q.clamp(0.4, 1.0), self.gain),
            EqType::HighShelf => b.set_matched(Kind::HighShelf, fs, self.freq, self.q.clamp(0.4, 1.0), self.gain),
            EqType::LowCut => b.set_matched(Kind::HighPass, fs, self.freq, self.q.clamp(0.4, 2.0), 0.0),
            EqType::HighCut => b.set_matched(Kind::LowPass, fs, self.freq, self.q.clamp(0.4, 2.0), 0.0),
        }
    }

    /// Whether the band changes anything.
    pub fn active(&self) -> bool {
        self.on && (matches!(self.kind, EqType::LowCut | EqType::HighCut) || self.gain.abs() > 0.01)
    }
}

/// One Balance band as a filter: shelves at the two ends, broad bells between.
pub fn balance_filter(i: usize, fs: f32, gain_db: f32) -> Biquad {
    let mut b = Biquad::default();
    set_balance(&mut b, i, fs, gain_db);
    b
}

fn set_balance(b: &mut Biquad, i: usize, fs: f32, gain_db: f32) {
    let g = (gain_db * 20.0).round() / 20.0;
    if i == 0 {
        b.set_matched(Kind::LowShelf, fs, 85.0, 0.7, g);
    } else if i == BAL_BANDS - 1 {
        b.set_matched(Kind::HighShelf, fs, 10_500.0, 0.7, g);
    } else {
        b.set_matched(Kind::Peak, fs, BAL_HZ[i], 0.9, g);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BandSet {
    pub comp: BandSettings,
    pub excite: f32,
    pub width: f32,
}

/// Everything the chain needs for one block.
#[derive(Clone, Debug)]
pub struct MasterSettings {
    pub bypass: bool,
    pub input_db: f32,
    pub output_db: f32,
    pub eq_on: bool,
    pub eq: [EqBandSettings; EQ_BANDS],
    pub bal_amount: f32,
    pub bal_db: [f32; BAL_BANDS],
    pub glue_on: bool,
    pub glue: GlueSettings,
    pub xover: [f32; 3],
    pub mb_on: bool,
    pub exc_on: bool,
    pub exc_mode: ExciteMode,
    pub img_on: bool,
    pub mono_bass: bool,
    pub mono_freq: f32,
    pub band: [BandSet; MB_BANDS],
    pub max_on: bool,
    pub max_gain_db: f32,
    pub ceiling_db: f32,
    pub max_release_ms: f32,
    pub max_character: f32,
    pub true_peak: bool,
    pub clip: f32,
    pub dither: DitherBits,
    pub dither_shape: bool,
}

impl MasterSettings {
    /// Reads the current (smoothed) values. `advance` steps the smoothers by that many samples.
    pub fn from_params(p: &MasterParams, advance: u32) -> Self {
        let f = |param: &nih_plug::prelude::FloatParam| {
            if advance > 0 && param.smoothed.is_smoothing() {
                param.smoothed.next_step(advance)
            } else {
                param.value()
            }
        };
        let bal = p.bal();
        Self {
            bypass: p.bypass.value(),
            input_db: f(&p.input),
            output_db: f(&p.output),
            eq_on: p.eq_on.value(),
            eq: std::array::from_fn(|i| {
                let b = &p.eq[i];
                EqBandSettings { on: b.on.value(), kind: b.kind.value(), freq: b.freq.value(), gain: f(&b.gain), q: b.q.value(), channel: b.channel.value() }
            }),
            bal_amount: f(&p.bal_amount),
            bal_db: std::array::from_fn(|i| bal[i].value()),
            glue_on: p.glue_on.value(),
            glue: GlueSettings {
                thresh_db: f(&p.glue_thresh),
                ratio: p.glue_ratio.value(),
                attack_ms: p.glue_attack.value(),
                release_ms: p.glue_release.value(),
                auto_release: p.glue_auto.value(),
                makeup_db: f(&p.glue_makeup),
                sc_hp: p.glue_hp.value(),
                mix: f(&p.glue_mix),
            },
            xover: [p.x_lo.value(), p.x_mid.value(), p.x_hi.value()],
            mb_on: p.mb_on.value(),
            exc_on: p.exc_on.value(),
            exc_mode: p.exc_mode.value().mode(),
            img_on: p.img_on.value(),
            mono_bass: p.mono_bass.value(),
            mono_freq: p.mono_freq.value(),
            band: std::array::from_fn(|i| {
                let b = &p.band[i];
                BandSet {
                    comp: BandSettings { thresh_db: f(&b.thresh), ratio: b.ratio.value(), attack_ms: b.attack.value(), release_ms: b.release.value(), gain_db: f(&b.gain) },
                    excite: f(&b.excite),
                    width: f(&b.width),
                }
            }),
            max_on: p.max_on.value(),
            max_gain_db: f(&p.max_gain),
            ceiling_db: f(&p.ceiling),
            max_release_ms: p.max_release.value(),
            max_character: f(&p.max_character),
            true_peak: p.true_peak.value(),
            clip: f(&p.clip),
            dither: p.dither.value().bits(),
            dither_shape: p.dither_shape.value(),
        }
    }

    /// Sets one parameter by its id and plain value, the way presets and Auto Master name
    /// them. Returns false for an id it doesn't know.
    pub fn set(&mut self, id: &str, v: f32) -> bool {
        let on = v > 0.5;
        // Band parameters end in _1.._n.
        if let Some((base, idx)) = id.rsplit_once('_').and_then(|(b, i)| i.parse::<usize>().ok().map(|i| (b, i))) {
            let k = idx.wrapping_sub(1);
            if base.starts_with("eq_") && k < EQ_BANDS {
                let b = &mut self.eq[k];
                match base {
                    "eq_on" => b.on = on,
                    "eq_type" => b.kind = [EqType::Bell, EqType::LowShelf, EqType::HighShelf, EqType::LowCut, EqType::HighCut][(v as usize).min(4)],
                    "eq_f" => b.freq = v,
                    "eq_g" => b.gain = v,
                    "eq_q" => b.q = v,
                    "eq_ch" => b.channel = [Channel::Stereo, Channel::Mid, Channel::Side][(v as usize).min(2)],
                    _ => return false,
                }
                return true;
            }
            if k < MB_BANDS && !base.starts_with("bal") {
                let b = &mut self.band[k];
                match base {
                    "mb_thr" => b.comp.thresh_db = v,
                    "mb_rat" => b.comp.ratio = v,
                    "mb_att" => b.comp.attack_ms = v,
                    "mb_rel" => b.comp.release_ms = v,
                    "mb_gain" => b.comp.gain_db = v,
                    "exc_amt" => b.excite = v,
                    "img_w" => b.width = v,
                    _ => return false,
                }
                return true;
            }
            if base == "bal" && idx < BAL_BANDS {
                self.bal_db[idx] = v;
                return true;
            }
        }
        match id {
            "bypass" => self.bypass = on,
            "m_in" => self.input_db = v,
            "m_out" => self.output_db = v,
            "genre" | "tgt" => {}
            "eq_mod" => self.eq_on = on,
            "bal_amt" => self.bal_amount = v,
            "glue_on" => self.glue_on = on,
            "glue_thr" => self.glue.thresh_db = v,
            "glue_rat" => self.glue.ratio = v,
            "glue_att" => self.glue.attack_ms = v,
            "glue_rel" => self.glue.release_ms = v,
            "glue_auto" => self.glue.auto_release = on,
            "glue_mk" => self.glue.makeup_db = v,
            "glue_hp" => self.glue.sc_hp = v,
            "glue_mix" => self.glue.mix = v,
            "x_lo" => self.xover[0] = v,
            "x_mid" => self.xover[1] = v,
            "x_hi" => self.xover[2] = v,
            "mb_on" => self.mb_on = on,
            "exc_on" => self.exc_on = on,
            "exc_mode" => self.exc_mode = [ExciteMode::Tube, ExciteMode::Tape, ExciteMode::Crisp][(v as usize).min(2)],
            "img_on" => self.img_on = on,
            "img_mono" => self.mono_bass = on,
            "img_mono_f" => self.mono_freq = v,
            "max_on" => self.max_on = on,
            "max_gain" => self.max_gain_db = v,
            "max_ceil" => self.ceiling_db = v,
            "max_rel" => self.max_release_ms = v,
            "max_char" => self.max_character = v,
            "max_tp" => self.true_peak = on,
            "max_clip" => self.clip = v,
            "dith" => self.dither = [DitherBits::Off, DitherBits::Bits16, DitherBits::Bits24][(v as usize).min(2)],
            "dith_shape" => self.dither_shape = on,
            _ => return false,
        }
        true
    }

    /// Whether the imager changes anything.
    pub fn imaging(&self) -> bool {
        self.img_on && (self.mono_bass || self.band.iter().any(|b| (b.width - 1.0).abs() > 0.005))
    }

    pub fn multiband(&self) -> bool {
        self.mb_on || self.exc_on || self.imaging()
    }
}

const HIST: usize = 180;

fn hist_bin(db: f32) -> usize {
    (((db + 80.0) * 2.0).max(0.0) as usize).min(HIST - 1)
}

fn hist_percentile(h: &[u32; HIST], p: f32) -> f32 {
    let total: u64 = h.iter().map(|c| *c as u64).sum();
    if total == 0 {
        return -100.0;
    }
    let want = (total as f32 * p) as u64;
    let mut seen = 0u64;
    for (i, c) in h.iter().enumerate() {
        seen += *c as u64;
        if seen >= want.max(1) {
            return -80.0 + (i as f32 + 0.5) * 0.5;
        }
    }
    10.0
}

/// Auto Master's ears: measures the mix on the first pass and the master's loudness on the
/// second.
struct Listener {
    mode: u32,
    fs: f32,
    meter: LoudnessMeter,
    tp: [TruePeak; 2],
    tp_max: f32,
    bp: [[Biquad; 2]; BAL_BANDS],
    band_acc: [f64; BAL_BANDS],
    /// What pink noise reads in each band, dB, so it can be taken out.
    pink_ref: [f32; BAL_BANDS],
    split: [Split4; 2],
    band_env: [f32; MB_BANDS],
    band_hist: Box<[[u32; HIST]; MB_BANDS]>,
    glue_sc: [Biquad; 2],
    glue_det: f32,
    glue_hist: Box<[u32; HIST]>,
    low: [[Biquad; 2]; 2],
    energy: [f64; 4],
    env: f32,
    k: [f32; 3],
    active: usize,
    total: usize,
}

impl Listener {
    fn new(fs: f32) -> Self {
        let bp: [[Biquad; 2]; BAL_BANDS] = std::array::from_fn(|i| [Biquad::new(Kind::BandPass, fs, BAL_HZ[i], 1.4, 0.0); 2]);
        // Pink noise has equal energy per octave: integrate each band's response over log f.
        let pink_ref = std::array::from_fn(|i| {
            let n = 600;
            let (lo, hi) = (10f32.ln(), (fs * 0.5).ln());
            let sum: f64 = (0..n).map(|k| 10f64.powf(bp[i][0].response_db(fs, (lo + (hi - lo) * (k as f32 + 0.5) / n as f32).exp()) as f64 / 10.0)).sum();
            (10.0 * (sum * ((hi - lo) / n as f32) as f64).max(1e-12).log10()) as f32
        });
        let mut split = [Split4::default(); 2];
        for s in &mut split {
            s.set(fs, AUTO_XOVER);
        }
        Self {
            mode: REQ_NONE,
            fs,
            meter: LoudnessMeter::new(fs),
            tp: [TruePeak::new(), TruePeak::new()],
            tp_max: 0.0,
            bp,
            band_acc: [0.0; BAL_BANDS],
            pink_ref,
            split,
            band_env: [0.0; MB_BANDS],
            band_hist: Box::new([[0; HIST]; MB_BANDS]),
            glue_sc: [Biquad::new(Kind::HighPass, fs, 90.0, 0.707, 0.0); 2],
            glue_det: 0.0,
            glue_hist: Box::new([0; HIST]),
            low: [[Biquad::new(Kind::LowPass, fs, 120.0, 0.707, 0.0); 2]; 2],
            energy: [0.0; 4],
            env: 0.0,
            k: [one_pole(fs, 0.1), one_pole(fs, 0.003), one_pole(fs, 0.005)],
            active: 0,
            total: 0,
        }
    }

    fn start(&mut self, mode: u32) {
        self.mode = mode;
        self.meter.reset();
        self.tp_max = 0.0;
        self.band_acc = [0.0; BAL_BANDS];
        for h in self.band_hist.iter_mut() {
            h.fill(0);
        }
        self.glue_hist.fill(0);
        self.energy = [0.0; 4];
        self.active = 0;
        self.total = 0;
        self.env = 0.0;
    }

    fn need(&self) -> usize {
        (self.fs * if self.mode == REQ_REFINE { 8.0 } else { 12.0 }) as usize
    }

    #[inline]
    fn tick(&mut self, l: f32, r: f32) {
        self.total += 1;
        let a = l.abs().max(r.abs());
        self.env = if a > self.env { a } else { self.env + (a - self.env) * self.k[0] };
        self.meter.tick(l, r);
        if self.mode == REQ_REFINE {
            if self.env > 0.003 {
                self.active += 1;
            }
            return;
        }
        self.tp_max = self.tp_max.max(self.tp[0].tick(l)).max(self.tp[1].tick(r));
        let bl = self.split[0].tick(l);
        let br = self.split[1].tick(r);
        let (gl, gr) = (self.glue_sc[0].tick(l), self.glue_sc[1].tick(r));
        self.glue_det += ((gl * gl).max(gr * gr) - self.glue_det) * self.k[2];
        let (m, s) = (0.5 * (l + r), 0.5 * (l - r));
        let lm = {
            let v = self.low[0][0].tick(m);
            self.low[0][1].tick(v)
        };
        let ls = {
            let v = self.low[1][0].tick(s);
            self.low[1][1].tick(v)
        };
        let bands: [f32; BAL_BANDS] = std::array::from_fn(|i| {
            let (x, y) = (self.bp[i][0].tick(l), self.bp[i][1].tick(r));
            x * x + y * y
        });
        for b in 0..MB_BANDS {
            let p = bl[b].abs().max(br[b].abs());
            self.band_env[b] += (p - self.band_env[b]) * if p > self.band_env[b] { 1.0 } else { self.k[1] };
        }
        // Only the music counts: silence would drag every figure down.
        if self.env > 0.003 {
            self.active += 1;
            for (acc, e) in self.band_acc.iter_mut().zip(bands) {
                *acc += e as f64;
            }
            for b in 0..MB_BANDS {
                self.band_hist[b][hist_bin(gain_to_db(self.band_env[b]))] += 1;
            }
            self.glue_hist[hist_bin(10.0 * self.glue_det.max(1e-12).log10() + 3.0)] += 1;
            self.energy[0] += (m * m) as f64;
            self.energy[1] += (s * s) as f64;
            self.energy[2] += (lm * lm) as f64;
            self.energy[3] += (ls * ls) as f64;
        }
    }

    /// Publishes progress and, when there is enough, the result.
    fn report(&mut self, sh: &MasterShared) {
        let need = self.need();
        sh.learn_progress.store((self.active as f32 / need as f32).min(1.0), Ordering::Relaxed);
        let enough = self.active >= need;
        let gave_up = self.total >= (self.fs * 90.0) as usize;
        if !enough && !gave_up {
            return;
        }
        let refine = self.mode == REQ_REFINE;
        self.mode = REQ_NONE;
        if !enough && (self.active as f32) < need as f32 * 0.4 {
            sh.learn_state.store(LEARN_FAILED, Ordering::Relaxed);
            return;
        }
        let h = &sh.heard;
        if refine {
            h.out_lufs.store(self.meter.integrated, Ordering::Relaxed);
            sh.learn_state.store(LEARN_REFINED, Ordering::Relaxed);
            return;
        }
        let n = self.active.max(1) as f64;
        h.lufs.store(self.meter.integrated, Ordering::Relaxed);
        h.true_peak_db.store(gain_to_db(self.tp_max), Ordering::Relaxed);
        h.range.store(self.meter.range, Ordering::Relaxed);
        for i in 0..BAL_BANDS {
            h.bands_db[i].store((10.0 * (self.band_acc[i] / n).max(1e-12).log10()) as f32 - self.pink_ref[i], Ordering::Relaxed);
        }
        for b in 0..MB_BANDS {
            h.mb_db[b].store(hist_percentile(&self.band_hist[b], 0.9), Ordering::Relaxed);
        }
        h.glue_db.store(hist_percentile(&self.glue_hist, 0.9), Ordering::Relaxed);
        h.side_ratio.store((self.energy[1] / self.energy[0].max(1e-12)).sqrt() as f32, Ordering::Relaxed);
        h.low_side_ratio.store((self.energy[3] / self.energy[2].max(1e-12)).sqrt() as f32, Ordering::Relaxed);
        sh.learn_state.store(LEARN_HEARD, Ordering::Relaxed);
    }
}

pub struct MasterChain {
    fs: f32,
    latency: usize,
    dry: [Vec<f32>; 2],
    dry_w: usize,
    eq: [[Biquad; 2]; EQ_BANDS],
    eq_live: [bool; EQ_BANDS],
    bal: [[Biquad; 2]; BAL_BANDS],
    bal_live: bool,
    glue: Glue,
    split: [Split4; 2],
    comp: [BandComp; MB_BANDS],
    exciter: [[Exciter; 2]; MB_BANDS],
    exc_amt: [f32; MB_BANDS],
    exc_k: f32,
    mono_hp: Biquad,
    /// The multiband stage's input, delayed like its output, for when it has nothing to do.
    mb_dry: [[f32; EXCITE_DELAY]; 2],
    mb_w: usize,
    clip: SoftClip,
    max: Maximizer,
    dither: Dither,
    meter: LoudnessMeter,
    in_meter: LoudnessMeter,
    proc_meter: LoudnessMeter,
    tp: [TruePeak; 2],
    tp_hold: f32,
    match_target: f32,
    match_gain: f32,
    match_k: f32,
    corr: [f32; 3],
    corr_k: f32,
    listener: Listener,
}

impl MasterChain {
    pub fn new(fs: f32) -> Self {
        let max = Maximizer::new(fs);
        let latency = 2 * EXCITE_DELAY + max.latency();
        Self {
            fs,
            latency,
            dry: [vec![0.0; latency], vec![0.0; latency]],
            dry_w: 0,
            eq: [[Biquad::default(); 2]; EQ_BANDS],
            eq_live: [false; EQ_BANDS],
            bal: [[Biquad::default(); 2]; BAL_BANDS],
            bal_live: false,
            glue: Glue::new(fs),
            split: [Split4::default(); 2],
            comp: [BandComp::new(fs); MB_BANDS],
            exciter: std::array::from_fn(|_| [Exciter::new(), Exciter::new()]),
            exc_amt: [0.0; MB_BANDS],
            exc_k: one_pole(fs, 0.02),
            mono_hp: Biquad::default(),
            mb_dry: [[0.0; EXCITE_DELAY]; 2],
            mb_w: 0,
            clip: SoftClip::new(),
            max,
            dither: Dither::default(),
            meter: LoudnessMeter::new(fs),
            in_meter: LoudnessMeter::new(fs),
            proc_meter: LoudnessMeter::new(fs),
            tp: [TruePeak::new(), TruePeak::new()],
            tp_hold: 0.0,
            match_target: 1.0,
            match_gain: 1.0,
            match_k: one_pole(fs, 0.25),
            corr: [0.0; 3],
            corr_k: one_pole(fs, 0.3),
            listener: Listener::new(fs),
        }
    }

    /// Delay through the chain, in samples. It does not depend on the settings.
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// The last integrated loudness and held true peak (dB) the output meter read.
    pub fn loudness(&self) -> (f32, f32) {
        (self.meter.integrated, gain_to_db(self.tp_hold))
    }

    pub fn reset(&mut self) {
        for d in &mut self.dry {
            d.fill(0.0);
        }
        for b in self.eq.iter_mut().flatten().chain(self.bal.iter_mut().flatten()) {
            b.reset();
        }
        self.glue.reset();
        for s in &mut self.split {
            s.reset();
        }
        for c in &mut self.comp {
            c.reset();
        }
        for e in self.exciter.iter_mut().flatten() {
            e.reset();
        }
        self.mono_hp.reset();
        self.mb_dry = [[0.0; EXCITE_DELAY]; 2];
        self.clip.reset();
        self.max.reset();
        self.match_gain = self.match_target;
    }

    /// Processes one block in place. `shared` receives the meters and drives Auto Master.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], s: &MasterSettings, shared: Option<&MasterShared>) {
        let n = left.len().min(right.len());
        let mut pos = 0;
        while pos < n {
            let end = (pos + 64).min(n);
            self.block(&mut left[pos..end], &mut right[pos..end], s, shared);
            pos = end;
        }
    }

    fn block(&mut self, left: &mut [f32], right: &mut [f32], s: &MasterSettings, shared: Option<&MasterShared>) {
        let n = left.len();
        let fs = self.fs;
        if let Some(sh) = shared {
            if sh.learn_cancel.swap(false, Ordering::Relaxed) {
                self.listener.mode = REQ_NONE;
                sh.learn_state.store(LEARN_IDLE, Ordering::Relaxed);
            }
            let req = sh.learn_request.swap(REQ_NONE, Ordering::Relaxed);
            if req != REQ_NONE {
                self.listener.start(req);
                sh.learn_progress.store(0.0, Ordering::Relaxed);
                sh.learn_state.store(if req == REQ_REFINE { LEARN_REFINING } else { LEARN_LISTENING }, Ordering::Relaxed);
            }
            if sh.reset_meters.swap(false, Ordering::Relaxed) {
                self.meter.reset_integrated();
                self.tp_hold = 0.0;
            }
        }

        // ---- per-block coefficients
        for (i, band) in s.eq.iter().enumerate() {
            let live = s.eq_on && band.active();
            if live {
                if !self.eq_live[i] {
                    self.eq[i][0].reset();
                    self.eq[i][1].reset();
                }
                band.apply(&mut self.eq[i][0], fs);
                let src = self.eq[i][0];
                self.eq[i][1].copy_coefs(&src);
            }
            self.eq_live[i] = live;
        }
        let bal_live = s.bal_amount > 0.001 && s.bal_db.iter().any(|g| g.abs() > 0.01);
        if bal_live {
            for i in 0..BAL_BANDS {
                if !self.bal_live {
                    self.bal[i][0].reset();
                    self.bal[i][1].reset();
                }
                set_balance(&mut self.bal[i][0], i, fs, s.bal_db[i] * s.bal_amount);
                let src = self.bal[i][0];
                self.bal[i][1].copy_coefs(&src);
            }
        }
        self.bal_live = bal_live;
        let x = ordered(s.xover);
        for sp in &mut self.split {
            sp.set(fs, x);
        }
        self.mono_hp.set(Kind::HighPass, fs, s.mono_freq, 0.707, 0.0);
        let multiband = s.multiband();
        let imaging = s.imaging();
        let in_gain = db_to_gain(s.input_db);
        let out_gain = db_to_gain(s.output_db);
        let push = if s.max_on { db_to_gain(s.max_gain_db) } else { 1.0 };
        let ceiling = db_to_gain(s.ceiling_db);
        let clip_on = s.max_on && s.clip > 0.001;
        let clip_at = ceiling * db_to_gain((1.0 - s.clip.clamp(0.0, 1.0)) * 6.0);
        let gain_match = shared.is_some_and(|sh| sh.gain_match.load(Ordering::Relaxed));
        let listening = self.listener.mode;

        let mut spec_in = [0.0f32; 64];
        let mut spec_out = [0.0f32; 64];
        let mut in_peak = [0.0f32; 2];
        let mut out_peak = [0.0f32; 2];
        let mut max_gr = 0.0f32;
        let mut mb_gr = [0.0f32; MB_BANDS];
        let mut glue_gr = 0.0f32;

        for i in 0..n {
            let (dl, dr) = (left[i], right[i]);
            let (mut l, mut r) = (dl * in_gain, dr * in_gain);
            in_peak[0] = in_peak[0].max(l.abs());
            in_peak[1] = in_peak[1].max(r.abs());
            spec_in[i] = 0.5 * (l + r);
            self.in_meter.tick(l, r);

            // ---- EQ and Balance, in mid/side: a stereo band is the same filter on both.
            let (mut m, mut sd) = (0.5 * (l + r), 0.5 * (l - r));
            for (k, band) in s.eq.iter().enumerate() {
                if self.eq_live[k] {
                    if band.channel != Channel::Side {
                        m = self.eq[k][0].tick(m);
                    }
                    if band.channel != Channel::Mid {
                        sd = self.eq[k][1].tick(sd);
                    }
                }
            }
            if listening == REQ_LISTEN {
                self.listener.tick(m + sd, m - sd);
            }
            if bal_live {
                for b in &mut self.bal {
                    m = b[0].tick(m);
                    sd = b[1].tick(sd);
                }
            }
            l = m + sd;
            r = m - sd;

            if s.glue_on {
                (l, r) = self.glue.tick(l, r, &s.glue);
                glue_gr = glue_gr.max(self.glue.reduction_db());
            }

            // ---- four bands: compression, harmonics, width. Always running, so switching a
            // module in never starts from stale filters; with nothing to do the delayed input
            // goes out instead, untouched.
            let dry = (self.mb_dry[0][self.mb_w], self.mb_dry[1][self.mb_w]);
            self.mb_dry[0][self.mb_w] = l;
            self.mb_dry[1][self.mb_w] = r;
            self.mb_w = (self.mb_w + 1) % EXCITE_DELAY;
            let bl = self.split[0].tick(l);
            let br = self.split[1].tick(r);
            let (mut sm, mut ss) = (0.0f32, 0.0f32);
            for b in 0..MB_BANDS {
                let set = &s.band[b];
                let g = self.comp[b].gain(bl[b], br[b], &set.comp, s.mb_on);
                mb_gr[b] = mb_gr[b].max(self.comp[b].reduction_db());
                let want = if s.exc_on { set.excite } else { 0.0 };
                self.exc_amt[b] += (want - self.exc_amt[b]) * self.exc_k;
                let amt = if self.exc_amt[b] < 0.0006 && want == 0.0 { 0.0 } else { self.exc_amt[b] };
                let xl = self.exciter[b][0].tick(bl[b] * g, s.exc_mode, amt);
                let xr = self.exciter[b][1].tick(br[b] * g, s.exc_mode, amt);
                let w = if s.img_on { set.width } else { 1.0 };
                sm += 0.5 * (xl + xr);
                ss += 0.5 * (xl - xr) * w;
            }
            let hp = self.mono_hp.tick(ss);
            if imaging && s.mono_bass {
                ss = hp;
            }
            if multiband {
                l = sm + ss;
                r = sm - ss;
            } else {
                (l, r) = dry;
            }

            // ---- soft clip and maximizer
            let (cl, cr) = self.clip.tick(l * push, r * push, clip_at, clip_on);
            let (ml, mr) = self.max.tick(cl, cr, ceiling, s.max_release_ms, s.max_character, s.true_peak, s.max_on);
            max_gr = max_gr.max(self.max.reduction_db());
            let mut ol = self.dither.tick(0, ml * out_gain, s.dither, s.dither_shape);
            let mut or = self.dither.tick(1, mr * out_gain, s.dither, s.dither_shape);

            if listening == REQ_REFINE {
                self.listener.tick(ol, or);
            }
            if self.proc_meter.tick(ol, or) {
                let (a, b) = (self.in_meter.short_term, self.proc_meter.short_term);
                self.match_target = if a > -60.0 && b > -60.0 { db_to_gain((a - b).clamp(-30.0, 0.0)) } else { 1.0 };
            }

            // ---- bypass: the input, delayed by the chain's latency
            let rd = self.dry_w;
            let (bl_, br_) = (self.dry[0][rd], self.dry[1][rd]);
            self.dry[0][rd] = dl;
            self.dry[1][rd] = dr;
            self.dry_w = (rd + 1) % self.latency;
            if s.bypass {
                ol = bl_;
                or = br_;
            }

            // ---- meters read what is heard, before gain match turns it down
            self.meter.tick(ol, or);
            self.tp_hold = self.tp_hold.max(self.tp[0].tick(ol)).max(self.tp[1].tick(or));
            out_peak[0] = out_peak[0].max(ol.abs());
            out_peak[1] = out_peak[1].max(or.abs());
            spec_out[i] = 0.5 * (ol + or);
            self.corr[0] += (ol * or - self.corr[0]) * self.corr_k;
            self.corr[1] += (ol * ol - self.corr[1]) * self.corr_k;
            self.corr[2] += (or * or - self.corr[2]) * self.corr_k;

            let want = if gain_match && !s.bypass { self.match_target } else { 1.0 };
            self.match_gain += (want - self.match_gain) * self.match_k;
            if (self.match_gain - 1.0).abs() > 1e-4 {
                ol *= self.match_gain;
                or *= self.match_gain;
            }
            left[i] = ol;
            right[i] = or;
        }

        let Some(sh) = shared else {
            return;
        };
        if self.listener.mode != REQ_NONE {
            self.listener.report(sh);
        }
        sh.spec_in.write(&spec_in[..n]);
        sh.spec_out.write(&spec_out[..n]);
        sh.scope[0].write(&left[..n]);
        sh.scope[1].write(&right[..n]);
        for c in 0..2 {
            MasterShared::meter(&sh.in_peak[c], in_peak[c]);
            MasterShared::meter(&sh.out_peak[c], out_peak[c]);
        }
        sh.momentary.store(self.meter.momentary, Ordering::Relaxed);
        sh.short_term.store(self.meter.short_term, Ordering::Relaxed);
        sh.integrated.store(self.meter.integrated, Ordering::Relaxed);
        sh.range.store(self.meter.range, Ordering::Relaxed);
        sh.true_peak.store(gain_to_db(self.tp_hold), Ordering::Relaxed);
        sh.in_short_term.store(self.in_meter.short_term, Ordering::Relaxed);
        MasterShared::meter(&sh.glue_gr, glue_gr);
        for b in 0..MB_BANDS {
            MasterShared::meter(&sh.mb_gr[b], if s.mb_on { mb_gr[b] } else { 0.0 });
        }
        MasterShared::meter(&sh.max_gr, max_gr);
        MasterShared::meter(&sh.clip_gr, self.clip.take_reduction_db());
        sh.match_db.store(gain_to_db(self.match_gain), Ordering::Relaxed);
        let denom = (self.corr[1] * self.corr[2]).sqrt();
        sh.correlation.store(if denom > 1e-10 { (self.corr[0] / denom).clamp(-1.0, 1.0) } else { 1.0 }, Ordering::Relaxed);
        let (mid, side) = (self.corr[1] + self.corr[2] + 2.0 * self.corr[0], self.corr[1] + self.corr[2] - 2.0 * self.corr[0]);
        sh.width.store(if mid > 1e-10 { (side.max(0.0) / mid).sqrt().min(2.0) } else { 0.0 }, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::master::auto;
    use crate::master::params::Genre;

    pub(crate) fn defaults() -> MasterSettings {
        MasterSettings::from_params(&MasterParams::default(), 0)
    }

    fn run(c: &mut MasterChain, s: &MasterSettings, x: &(Vec<f32>, Vec<f32>), sh: Option<&MasterShared>) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = x.clone();
        for (a, b) in l.chunks_mut(64).zip(r.chunks_mut(64)) {
            c.process(a, b, s, sh);
        }
        (l, r)
    }

    fn everything_off() -> MasterSettings {
        let mut s = defaults();
        for id in ["eq_mod", "glue_on", "mb_on", "exc_on", "img_on", "max_on"] {
            assert!(s.set(id, 0.0));
        }
        s.bal_amount = 0.0;
        s
    }

    #[test]
    fn nothing_switched_on_is_a_delay() {
        let fs = 48_000.0;
        let x = crate::testsignal::mix(fs, 1.0);
        let mut c = MasterChain::new(fs);
        let (l, r) = run(&mut c, &everything_off(), &x, None);
        let lat = c.latency();
        for i in lat..l.len() {
            assert!((l[i] - x.0[i - lat]).abs() < 1e-6 && (r[i] - x.1[i - lat]).abs() < 1e-6, "sample {i}");
        }
    }

    #[test]
    fn bypass_is_the_input_delayed() {
        let fs = 44_100.0;
        let x = crate::testsignal::mix(fs, 1.0);
        let mut c = MasterChain::new(fs);
        let mut s = defaults();
        s.max_gain_db = 9.0;
        s.bypass = true;
        let (l, _) = run(&mut c, &s, &x, None);
        let lat = c.latency();
        for i in lat..l.len() {
            assert_eq!(l[i], x.0[i - lat]);
        }
    }

    #[test]
    fn pushed_hard_it_holds_the_ceiling() {
        let fs = 48_000.0;
        let x = crate::testsignal::mix(fs, 4.0);
        let mut c = MasterChain::new(fs);
        let mut s = defaults();
        for (id, v) in [("mb_on", 1.0), ("exc_on", 1.0), ("exc_amt_3", 0.4), ("exc_amt_4", 0.5), ("img_w_4", 1.5), ("img_mono", 1.0), ("max_gain", 14.0), ("max_clip", 0.5), ("eq_g_6", 3.0), ("dith", 1.0)] {
            assert!(s.set(id, v), "{id}");
        }
        let (l, r) = run(&mut c, &s, &x, None);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        let (lufs, tp) = c.loudness();
        assert!(tp <= s.ceiling_db + 0.3, "true peak {tp}");
        assert!(lufs > -13.0, "only {lufs} LUFS");
    }

    #[test]
    fn listener_reads_pink_noise_flat() {
        let fs = 48_000.0;
        // Pink noise (Paul Kellet's filter), a little different left and right.
        let mut seed = 0x2545_F491u32;
        let mut white = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let mut st = [[0.0f32; 7]; 2];
        let mut pink = |ch: usize, w: f32| {
            let b = &mut st[ch];
            b[0] = 0.99886 * b[0] + w * 0.0555179;
            b[1] = 0.99332 * b[1] + w * 0.0750759;
            b[2] = 0.96900 * b[2] + w * 0.1538520;
            b[3] = 0.86650 * b[3] + w * 0.3104856;
            b[4] = 0.55000 * b[4] + w * 0.5329522;
            b[5] = -0.7616 * b[5] - w * 0.0168980;
            let out = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
            b[6] = w * 0.115926;
            out * 0.05
        };
        let n = (fs * 14.0) as usize;
        let x: (Vec<f32>, Vec<f32>) = (0..n).map(|_| (pink(0, white()), pink(1, white()))).unzip();
        let sh = MasterShared::default();
        sh.learn_request.store(REQ_LISTEN, Ordering::Relaxed);
        let mut c = MasterChain::new(fs);
        run(&mut c, &defaults(), &x, Some(&sh));
        assert_eq!(sh.learn_state.load(Ordering::Relaxed), LEARN_HEARD);
        let b: Vec<f32> = sh.heard.bands_db.iter().map(|a| a.load(Ordering::Relaxed)).collect();
        let mean = b[1..7].iter().sum::<f32>() / 6.0;
        for (i, v) in b.iter().enumerate().take(7).skip(1) {
            assert!((v - mean).abs() < 1.2, "band {i}: {b:?}");
        }
        // Independent noise left and right: side is as loud as mid.
        let side = sh.heard.side_ratio.load(Ordering::Relaxed);
        assert!((side - 1.0).abs() < 0.15, "{side}");
    }

    /// The whole Auto Master flow, offline: listen, decide, refine, and the master lands on
    /// the target loudness under the ceiling.
    #[test]
    fn auto_master_lands_on_the_target() {
        let fs = 48_000.0;
        let x = crate::testsignal::mix(fs, 14.0);
        for (genre, target) in [(Genre::HipHop, -10.0f32), (Genre::Pop, -14.0), (Genre::Acoustic, -16.0)] {
            let sh = MasterShared::default();
            let mut s = defaults();
            let mut c = MasterChain::new(fs);
            sh.learn_request.store(REQ_LISTEN, Ordering::Relaxed);
            run(&mut c, &s, &x, Some(&sh));
            assert_eq!(sh.learn_state.load(Ordering::Relaxed), LEARN_HEARD);
            let heard = auto::Heard::read(&sh);
            let out = auto::decide(&heard, genre, target);
            for (id, v) in &out.values {
                assert!(s.set(id, *v), "auto set unknown id {id}");
            }
            for _ in 0..2 {
                sh.learn_request.store(REQ_REFINE, Ordering::Relaxed);
                run(&mut c, &s, &x, Some(&sh));
                assert_eq!(sh.learn_state.load(Ordering::Relaxed), LEARN_REFINED);
                let (gain, _) = auto::refine(sh.heard.out_lufs.load(Ordering::Relaxed), target, s.max_gain_db);
                s.max_gain_db = gain;
            }
            let mut c = MasterChain::new(fs);
            let (l, r) = run(&mut c, &s, &x, None);
            let (lufs, tp) = c.loudness();
            assert!(l.iter().chain(&r).all(|v| v.is_finite()));
            assert!((lufs - target).abs() < 1.0, "{genre:?}: {lufs} LUFS for a target of {target}");
            assert!(tp <= s.ceiling_db + 0.3, "{genre:?}: true peak {tp}");
        }
    }

    #[test]
    fn every_preset_renders_cleanly() {
        let fs = 44_100.0;
        let x = crate::testsignal::mix(fs, 3.0);
        for p in crate::master::presets::PRESETS {
            let mut s = defaults();
            for (id, v) in p.values {
                assert!(s.set(id, *v), "{}: unknown id {id}", p.name);
            }
            let mut c = MasterChain::new(fs);
            let (l, r) = run(&mut c, &s, &x, None);
            let (lufs, tp) = c.loudness();
            assert!(l.iter().chain(&r).all(|v| v.is_finite()), "{}", p.name);
            assert!(tp <= s.ceiling_db + 0.3, "{}: true peak {tp}", p.name);
            assert!((-30.0..-5.0).contains(&lufs), "{}: {lufs} LUFS", p.name);
        }
    }
}
