//! The vocal chain, in order: input → gate → tune → clean-up EQ → Match EQ → de-esser →
//! Punch (FET) → Level (opto) → saturation → tone, all mono; then the doubler, delay and reverb
//! make it stereo, the dry/wet mix lines up against the latency, and the limiter catches peaks.
//!
//! Plain Rust over plain settings, so tests and the offline renderer can drive it directly.

use std::sync::atomic::Ordering;

use crate::dsp::dynamics::{DeEsser, Fet, FetSettings, Gate, Opto};
use crate::dsp::filters::{db_to_gain, gain_to_db, one_pole, Biquad, Kind};
use crate::dsp::limiter::Limiter;
use crate::dsp::saturation::{SatMode, Saturator};
use crate::dsp::space::{Delay, DelaySettings, Doubler, DoublerSettings, DuckFollower, Reverb, ReverbSettings, ReverbType, DIVISION_BEATS};
use crate::dsp::tune::detector::VoiceRange;
use crate::dsp::tune::retune::RetuneParams;
use crate::dsp::tune::scale::SCALES;
use crate::dsp::tune::{TuneEngine, TuneSettings};
use crate::params::{PersonaParams, MATCH_BANDS, MATCH_HZ};
use crate::shared::{Shared, LEARN_DONE, LEARN_FAILED, LEARN_IDLE, LEARN_LISTENING};

/// Everything the chain needs for one block, read from the parameters once per block.
#[derive(Clone, Debug)]
pub struct Settings {
    pub input_db: f32,
    pub output_db: f32,
    pub trim_db: f32,
    pub mix: f32,
    pub limiter: bool,
    pub ceiling_db: f32,
    /// tune, punch, grit, air, space, width; 0.5 = as designed.
    pub vibe: [f32; 6],
    pub match_amount: f32,
    pub match_db: [f32; MATCH_BANDS],
    pub gate_on: bool,
    pub gate_thresh: f32,
    pub gate_range: f32,
    pub tune_on: bool,
    pub tune_root: i32,
    pub tune_mask: u16,
    pub tune_speed_ms: f32,
    pub tune_humanize: f32,
    pub tune_flex: f32,
    pub tune_amount: f32,
    pub tune_range: VoiceRange,
    pub tune_guard: f32,
    pub hp_on: bool,
    pub hp_freq: f32,
    pub mud_gain: f32,
    pub mud_freq: f32,
    pub harsh_gain: f32,
    pub harsh_freq: f32,
    pub ds_on: bool,
    pub ds_freq: f32,
    pub ds_thresh: f32,
    pub ds_range: f32,
    pub punch_on: bool,
    pub punch_thresh: f32,
    pub punch_ratio: f32,
    pub punch_attack: f32,
    pub punch_release: f32,
    pub punch_makeup: f32,
    pub punch_color: f32,
    pub level_on: bool,
    pub level_amount: f32,
    pub level_makeup: f32,
    pub sat_on: bool,
    pub sat_mode: SatMode,
    pub sat_drive: f32,
    pub sat_mix: f32,
    pub low_gain: f32,
    pub low_freq: f32,
    pub pres_gain: f32,
    pub pres_freq: f32,
    pub air_gain: f32,
    pub air_freq: f32,
    pub dbl_on: bool,
    pub dbl_mix: f32,
    pub dbl_detune: f32,
    pub dbl_delay: f32,
    pub dbl_width: f32,
    pub dly_on: bool,
    pub dly_beats: f32,
    pub dly_feedback: f32,
    pub dly_mix: f32,
    pub dly_ping_pong: bool,
    pub dly_hp: f32,
    pub dly_lp: f32,
    pub dly_duck: f32,
    pub rev_on: bool,
    pub rev_kind: ReverbType,
    pub rev_size: f32,
    pub rev_decay: f32,
    pub rev_predelay: f32,
    pub rev_damping: f32,
    pub rev_lowcut: f32,
    pub rev_mix: f32,
    pub rev_duck: f32,
    pub bpm: f32,
}

impl Settings {
    /// Reads the current (smoothed) values. `advance` steps the smoothers by that many samples.
    pub fn from_params(p: &PersonaParams, bpm: f32, advance: u32) -> Self {
        let f = |param: &nih_plug::prelude::FloatParam| {
            if advance > 0 && param.smoothed.is_smoothing() {
                param.smoothed.next_step(advance)
            } else {
                param.value()
            }
        };
        let bands = p.matched.bands();
        Settings {
            input_db: f(&p.input),
            output_db: f(&p.output),
            trim_db: f(&p.trim),
            mix: f(&p.mix),
            limiter: p.limiter.value(),
            ceiling_db: p.ceiling.value(),
            vibe: [
                f(&p.vibe.tune),
                f(&p.vibe.punch),
                f(&p.vibe.grit),
                f(&p.vibe.air),
                f(&p.vibe.space),
                f(&p.vibe.width),
            ],
            match_amount: f(&p.matched.amount),
            match_db: std::array::from_fn(|i| bands[i].value()),
            gate_on: p.gate.on.value(),
            gate_thresh: p.gate.thresh.value(),
            gate_range: p.gate.range.value(),
            tune_on: p.tune.on.value(),
            tune_root: p.tune.key.value() as i32,
            tune_mask: SCALES[p.tune.scale.value() as usize].1,
            tune_speed_ms: p.tune.speed.value(),
            tune_humanize: p.tune.humanize.value(),
            tune_flex: p.tune.flex.value(),
            tune_amount: f(&p.tune.amount),
            tune_range: p.tune.voice.value().range(),
            tune_guard: p.tune.guard.value(),
            hp_on: p.clean.hp_on.value(),
            hp_freq: p.clean.hp_freq.value(),
            mud_gain: f(&p.clean.mud_gain),
            mud_freq: p.clean.mud_freq.value(),
            harsh_gain: f(&p.clean.harsh_gain),
            harsh_freq: p.clean.harsh_freq.value(),
            ds_on: p.deess.on.value(),
            ds_freq: p.deess.freq.value(),
            ds_thresh: f(&p.deess.thresh),
            ds_range: f(&p.deess.range),
            punch_on: p.punch.on.value(),
            punch_thresh: f(&p.punch.thresh),
            punch_ratio: p.punch.ratio.value(),
            punch_attack: p.punch.attack.value(),
            punch_release: p.punch.release.value(),
            punch_makeup: f(&p.punch.makeup),
            punch_color: f(&p.punch.color),
            level_on: p.level.on.value(),
            level_amount: f(&p.level.amount),
            level_makeup: f(&p.level.makeup),
            sat_on: p.sat.on.value(),
            sat_mode: p.sat.mode.value().mode(),
            sat_drive: f(&p.sat.drive),
            sat_mix: f(&p.sat.mix),
            low_gain: f(&p.tone.low_gain),
            low_freq: p.tone.low_freq.value(),
            pres_gain: f(&p.tone.pres_gain),
            pres_freq: p.tone.pres_freq.value(),
            air_gain: f(&p.tone.air_gain),
            air_freq: p.tone.air_freq.value(),
            dbl_on: p.doubler.on.value(),
            dbl_mix: f(&p.doubler.mix),
            dbl_detune: p.doubler.detune.value(),
            dbl_delay: p.doubler.delay.value(),
            dbl_width: f(&p.doubler.width),
            dly_on: p.delay.on.value(),
            dly_beats: DIVISION_BEATS[p.delay.division.value() as usize],
            dly_feedback: f(&p.delay.feedback),
            dly_mix: f(&p.delay.mix),
            dly_ping_pong: p.delay.ping_pong.value(),
            dly_hp: p.delay.hp.value(),
            dly_lp: p.delay.lp.value(),
            dly_duck: f(&p.delay.duck),
            rev_on: p.reverb.on.value(),
            rev_kind: p.reverb.kind.value().kind(),
            rev_size: p.reverb.size.value(),
            rev_decay: p.reverb.decay.value(),
            rev_predelay: p.reverb.predelay.value(),
            rev_damping: p.reverb.damping.value(),
            rev_lowcut: p.reverb.lowcut.value(),
            rev_mix: f(&p.reverb.mix),
            rev_duck: f(&p.reverb.duck),
            bpm,
        }
    }
}

/// The Match My Voice listener: measures the raw take while the editor asks it to.
struct Learner {
    active: bool,
    voiced: usize,
    total: usize,
    need: usize,
    give_up: usize,
    env: f32,
    env_att: f32,
    env_rel: f32,
    level_acc: f64,
    bands: [Biquad; MATCH_BANDS],
    band_acc: [f64; MATCH_BANDS],
    sib: Biquad,
    sib_env: f32,
    sib_hist: [u32; 96],
    pitch_hist: [u32; 128],
}

impl Learner {
    fn new(fs: f32) -> Self {
        Self {
            active: false,
            voiced: 0,
            total: 0,
            need: (fs * 8.0) as usize,
            give_up: (fs * 60.0) as usize,
            env: 0.0,
            env_att: one_pole(fs, 0.005),
            env_rel: one_pole(fs, 0.1),
            level_acc: 0.0,
            bands: std::array::from_fn(|i| Biquad::new(Kind::BandPass, fs, MATCH_HZ[i], 1.4, 0.0)),
            band_acc: [0.0; MATCH_BANDS],
            sib: Biquad::new(Kind::BandPass, fs, 7000.0, 0.9, 0.0),
            sib_env: 0.0,
            sib_hist: [0; 96],
            pitch_hist: [0; 128],
        }
    }

    fn start(&mut self) {
        self.active = true;
        self.voiced = 0;
        self.total = 0;
        self.level_acc = 0.0;
        self.band_acc = [0.0; MATCH_BANDS];
        self.sib_hist = [0; 96];
        self.pitch_hist = [0; 128];
        for b in &mut self.bands {
            b.reset();
        }
        self.sib.reset();
    }

    #[inline]
    fn tick(&mut self, x: f32) {
        let a = x.abs();
        let k = if a > self.env { self.env_att } else { self.env_rel };
        self.env += (a - self.env) * k;
        let bands: [f32; MATCH_BANDS] = std::array::from_fn(|i| self.bands[i].tick(x));
        let s = self.sib.tick(x).abs();
        self.sib_env += (s - self.sib_env) * if s > self.sib_env { self.env_att } else { self.env_rel };
        self.total += 1;
        // Only the singing counts: room tone between phrases would drag every figure down.
        if self.env > 0.0056 {
            self.voiced += 1;
            self.level_acc += (x * x) as f64;
            for (acc, b) in self.band_acc.iter_mut().zip(bands) {
                *acc += (b * b) as f64;
            }
            let bin = (gain_to_db(self.sib_env) + 96.0).clamp(0.0, 95.0) as usize;
            self.sib_hist[bin] += 1;
        }
    }

    fn note(&mut self, midi: f32, voiced: bool) {
        if voiced && midi > 0.0 {
            self.pitch_hist[(midi.round() as usize).min(127)] += 1;
        }
    }

    fn finish(&mut self, shared: &Shared, tune: &TuneEngine) {
        self.active = false;
        let n = self.voiced.max(1) as f64;
        let r = &shared.learn;
        r.level_db.store((10.0 * (self.level_acc / n).max(1e-12).log10()) as f32, Ordering::Relaxed);
        for (slot, acc) in r.bands_db.iter().zip(self.band_acc) {
            slot.store((10.0 * (acc / n).max(1e-12).log10()) as f32, Ordering::Relaxed);
        }
        let count: u32 = self.sib_hist.iter().sum();
        let mut seen = 0;
        let mut p95 = 0;
        for (i, &c) in self.sib_hist.iter().enumerate() {
            seen += c;
            if seen as f32 >= count as f32 * 0.95 {
                p95 = i;
                break;
            }
        }
        r.sibilance_db.store(p95 as f32 - 96.0, Ordering::Relaxed);
        let notes: u32 = self.pitch_hist.iter().sum();
        let mut median = 0.0;
        if notes > 100 {
            let mut acc = 0;
            for (i, &c) in self.pitch_hist.iter().enumerate() {
                acc += c;
                if acc * 2 >= notes {
                    median = i as f32;
                    break;
                }
            }
        }
        r.median_midi.store(median, Ordering::Relaxed);
        let k = tune.key.estimate;
        r.key_root.store(k.root, Ordering::Relaxed);
        r.key_minor.store(k.minor, Ordering::Relaxed);
        r.key_valid.store(k.valid, Ordering::Relaxed);
        r.key_confidence.store(k.confidence, Ordering::Relaxed);
        shared.learn_progress.store(1.0, Ordering::Relaxed);
        shared.learn_state.store(LEARN_DONE, Ordering::Relaxed);
    }
}

/// The vibe knobs folded into the settings of the modules they drive. At 50% every knob leaves
/// the style as designed. Grit, Space and Width also fade their module in from nothing when the
/// style has it switched off, so every knob always does something.
#[derive(Clone, Debug)]
pub struct Effective {
    pub tune_speed_ms: f32,
    pub tune_amount: f32,
    pub punch: FetSettings,
    pub level_amount: f32,
    pub level_makeup: f32,
    /// (mode, drive, mix); None when saturation is out of the chain.
    pub sat: Option<(SatMode, f32, f32)>,
    /// Gain after the saturator that takes back most of the loudness Grit adds or removes.
    pub sat_trim: f32,
    pub pres_gain: f32,
    pub air_gain: f32,
    /// Doubler send; 0 when it's out of the chain.
    pub dbl_mix: f32,
    pub dbl: DoublerSettings,
    pub dly_mix: f32,
    pub rev_mix: f32,
}

impl Effective {
    pub fn new(s: &Settings) -> Self {
        let [tune, punch, grit, air, space, width] = s.vibe;
        let pv = punch - 0.5;
        let gv = grit - 0.5;
        let av = air - 0.5;
        let sat = if s.sat_on {
            Some((s.sat_mode, (s.sat_drive + gv * 0.8).clamp(0.0, 1.0), s.sat_mix))
        } else if gv > 0.005 {
            Some((s.sat_mode, gv * 0.9, 1.0))
        } else {
            None
        };
        // Drive raises quiet passages more than peaks (the curve is level-matched at the peaks),
        // so Grit would mostly make things louder; compensate by the small-signal gain change.
        let sat_trim = match sat {
            Some((mode, drive, mix)) => {
                let small = |d: f32| 20.0 * (Saturator::curve(mode, d, 0.01) / 0.01).abs().max(1e-6).log10();
                let base = if s.sat_on { s.sat_drive } else { 0.0 };
                crate::dsp::filters::db_to_gain(-0.45 * (small(drive) - small(base)) * mix)
            }
            None => 1.0,
        };
        let push = (2.0 * space).min(2.0);
        let (dly_mix, rev_mix) = if s.dly_on || s.rev_on {
            (
                if s.dly_on { (s.dly_mix * push).clamp(0.0, 1.0) } else { 0.0 },
                if s.rev_on { (s.rev_mix * push).clamp(0.0, 1.0) } else { 0.0 },
            )
        } else {
            (0.0, ((space - 0.5) * 2.0).max(0.0) * 0.16)
        };
        let dbl_mix = if s.dbl_on { (s.dbl_mix * 2.0 * width).clamp(0.0, 1.0) } else { ((width - 0.5) * 2.0).max(0.0) * 0.35 };
        Effective {
            tune_speed_ms: s.tune_speed_ms * 2f32.powf((0.5 - tune) * 4.0),
            tune_amount: (s.tune_amount * (2.0 * tune).min(1.0)).clamp(0.0, 1.0),
            punch: FetSettings {
                thresh_db: s.punch_thresh - pv * 16.0,
                ratio: s.punch_ratio,
                attack_ms: s.punch_attack,
                release_ms: s.punch_release,
                // Most of the extra gain reduction comes back as makeup: more Punch means
                // denser, not quieter.
                makeup_db: s.punch_makeup + pv * 16.0 * (1.0 - 1.0 / s.punch_ratio.max(1.0)) * if pv > 0.0 { 1.6 } else { 0.8 },
                color: (s.punch_color * (1.0 + gv * 2.0)).clamp(0.0, 1.0),
            },
            level_amount: (s.level_amount + pv * 0.6).clamp(0.0, 1.0),
            level_makeup: s.level_makeup + pv * 0.6 * 32.0 * (2.0 / 3.0) * if pv > 0.0 { 0.7 } else { 0.35 },
            sat,
            sat_trim,
            pres_gain: s.pres_gain + av * 3.0,
            air_gain: s.air_gain + av * 10.0,
            dbl_mix,
            dbl: DoublerSettings {
                detune_cents: s.dbl_detune,
                delay_ms: s.dbl_delay,
                width: (s.dbl_width * (0.5 + width)).clamp(0.0, 1.0),
            },
            dly_mix,
            rev_mix,
        }
    }
}

/// The static EQ stages (clean-up, Match and tone) as the chain will run them, for drawing.
pub fn eq_filters(s: &Settings, fs: f32) -> Vec<Biquad> {
    let e = Effective::new(s);
    let mut v = Vec::with_capacity(16);
    if s.hp_on {
        v.push(Biquad::new(Kind::HighPass, fs, s.hp_freq, 0.707, 0.0));
        v.push(Biquad::new(Kind::HighPass, fs, s.hp_freq, 0.707, 0.0));
    }
    v.push(Biquad::new(Kind::Peak, fs, s.mud_freq, 1.2, s.mud_gain));
    v.push(Biquad::new(Kind::Peak, fs, s.harsh_freq, 2.0, s.harsh_gain));
    if s.match_amount > 0.001 {
        for (i, hz) in MATCH_HZ.iter().enumerate() {
            v.push(Biquad::new(Kind::Peak, fs, *hz, 1.1, s.match_db[i] * s.match_amount));
        }
    }
    v.push(Biquad::new(Kind::LowShelf, fs, s.low_freq, 0.7, s.low_gain));
    v.push(Biquad::new(Kind::Peak, fs, s.pres_freq, 0.9, e.pres_gain));
    v.push(Biquad::new(Kind::HighShelf, fs, s.air_freq, 0.7, e.air_gain));
    v
}

pub struct Chain {
    fs: f32,
    scratch: Vec<f32>,
    dry: [Vec<f32>; 2],
    dry_w: usize,
    gate: Gate,
    tune: TuneEngine,
    hp: [Biquad; 2],
    mud: Biquad,
    harsh: Biquad,
    matcher: [Biquad; MATCH_BANDS],
    deess: DeEsser,
    punch: Fet,
    level: Opto,
    sat: Saturator,
    low: Biquad,
    pres: Biquad,
    air: Biquad,
    doubler: Doubler,
    delay: Delay,
    reverb: Reverb,
    duck: DuckFollower,
    limiter: Limiter,
    learner: Learner,
    /// Which of saturation, doubler, delay and reverb ran last block: one coming back in
    /// starts from silence instead of replaying the state it was frozen with.
    running: [bool; 4],
}

impl Chain {
    pub fn new(fs: f32, max_block: usize) -> Self {
        let dry_len = (fs * 0.2) as usize;
        Self {
            fs,
            scratch: vec![0.0; max_block.max(64)],
            dry: [vec![0.0; dry_len], vec![0.0; dry_len]],
            dry_w: 0,
            gate: Gate::new(fs),
            tune: TuneEngine::new(fs, max_block.max(64)),
            hp: [Biquad::default(); 2],
            mud: Biquad::default(),
            harsh: Biquad::default(),
            matcher: [Biquad::default(); MATCH_BANDS],
            deess: DeEsser::new(fs),
            punch: Fet::new(fs),
            level: Opto::new(fs),
            sat: Saturator::new(fs),
            low: Biquad::default(),
            pres: Biquad::default(),
            air: Biquad::default(),
            doubler: Doubler::new(fs),
            delay: Delay::new(fs),
            reverb: Reverb::new(fs),
            duck: DuckFollower::new(fs),
            limiter: Limiter::new(fs),
            learner: Learner::new(fs),
            running: [false; 4],
        }
    }

    /// Total delay through the chain for these settings, in samples.
    pub fn latency(&self, s: &Settings) -> usize {
        self.tune_latency(s) + self.limiter.latency()
    }

    fn tune_latency(&self, s: &Settings) -> usize {
        self.tune.latency_for(s.tune_on)
    }

    pub fn reset(&mut self) {
        for d in &mut self.dry {
            d.fill(0.0);
        }
        self.gate.reset();
        self.tune.reset();
        for b in self.hp.iter_mut().chain([&mut self.mud, &mut self.harsh, &mut self.low, &mut self.pres, &mut self.air]) {
            b.reset();
        }
        for b in &mut self.matcher {
            b.reset();
        }
        self.deess.reset();
        self.punch.reset();
        self.level.reset();
        self.sat.reset();
        self.doubler.reset();
        self.delay.reset();
        self.reverb.reset();
        self.limiter.reset();
        self.running = [false; 4];
    }

    /// Lets the tuner follow a new voice range before latency is reported for it.
    pub fn prepare_range(&mut self, s: &Settings) {
        self.tune.set_range(s.tune_range);
    }

    /// Processes one block in place. `shared` receives meters and drives Match My Voice.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], s: &Settings, shared: Option<&Shared>) {
        let n = left.len().min(right.len());
        if n == 0 {
            return;
        }
        if self.scratch.len() < n {
            // Hosts should never exceed the declared block size; growing beats overrunning.
            self.scratch.resize(n, 0.0);
        }
        if let Some(sh) = shared {
            if sh.learn_cancel.swap(false, Ordering::Relaxed) {
                self.learner.active = false;
                sh.learn_state.store(LEARN_IDLE, Ordering::Relaxed);
            }
            if sh.learn_request.swap(false, Ordering::Relaxed) {
                self.learner.start();
                sh.learn_progress.store(0.0, Ordering::Relaxed);
                sh.learn_state.store(LEARN_LISTENING, Ordering::Relaxed);
            }
        }
        let fs = self.fs;
        let e = Effective::new(s);
        let in_gain = db_to_gain(s.input_db);

        // ---- mono input, gate, and the Match listener on the raw take
        let mut in_peak = 0.0f32;
        for i in 0..n {
            let raw = 0.5 * (left[i] + right[i]);
            if self.learner.active {
                self.learner.tick(raw);
            }
            let x = raw * in_gain;
            in_peak = in_peak.max(x.abs());
            self.scratch[i] = if s.gate_on { self.gate.tick(x, s.gate_thresh, s.gate_range) } else { x };
        }

        // ---- tune (block based; also listens when off, for the readout and Match)
        let tune = TuneSettings {
            enabled: s.tune_on,
            range: s.tune_range,
            root: s.tune_root,
            mask: s.tune_mask,
            retune: RetuneParams { retune_ms: e.tune_speed_ms, humanize: s.tune_humanize, flex: s.tune_flex },
            amount: e.tune_amount,
            guard: s.tune_guard,
        };
        self.tune.process(&mut self.scratch[..n], &tune);
        if self.learner.active {
            let live = self.tune.live;
            self.learner.note(live.midi, live.voiced);
        }

        // ---- per-block coefficients
        for b in &mut self.hp {
            b.set(Kind::HighPass, fs, s.hp_freq, 0.707, 0.0);
        }
        self.mud.set(Kind::Peak, fs, s.mud_freq, 1.2, s.mud_gain);
        self.harsh.set(Kind::Peak, fs, s.harsh_freq, 2.0, s.harsh_gain);
        let use_match = s.match_amount > 0.001;
        if use_match {
            for (i, b) in self.matcher.iter_mut().enumerate() {
                // Rounded so a slow automation sweep doesn't recompute every block.
                b.set(Kind::Peak, fs, MATCH_HZ[i], 1.1, (s.match_db[i] * s.match_amount * 100.0).round() / 100.0);
            }
        }
        self.deess.set(s.ds_freq);
        self.low.set(Kind::LowShelf, fs, s.low_freq, 0.7, s.low_gain);
        self.pres.set(Kind::Peak, fs, s.pres_freq, 0.9, e.pres_gain);
        self.air.set(Kind::HighShelf, fs, s.air_freq, 0.7, e.air_gain);
        self.delay.set_filters(s.dly_hp, s.dly_lp);
        let dly = DelaySettings {
            seconds: s.dly_beats * 60.0 / s.bpm.clamp(40.0, 300.0),
            feedback: s.dly_feedback,
            ping_pong: s.dly_ping_pong,
            hp_hz: s.dly_hp,
            lp_hz: s.dly_lp,
        };
        self.reverb.set_lowcut(s.rev_lowcut);
        let rev = ReverbSettings {
            kind: s.rev_kind,
            size: s.rev_size,
            decay_s: s.rev_decay,
            predelay_ms: s.rev_predelay,
            damping: s.rev_damping,
        };
        let run = [e.sat.is_some(), e.dbl_mix > 0.0, e.dly_mix > 0.0, e.rev_mix > 0.0];
        if run[0] && !self.running[0] {
            self.sat.reset();
        }
        if run[1] && !self.running[1] {
            self.doubler.reset();
        }
        if run[2] && !self.running[2] {
            self.delay.reset();
        }
        if run[3] && !self.running[3] {
            self.reverb.reset();
        }
        self.running = run;

        let out_gain = db_to_gain(s.output_db);
        let trim = db_to_gain(s.trim_db);
        let ceiling = db_to_gain(s.ceiling_db);
        let dry_delay = self.tune_latency(s);
        let dry_len = self.dry[0].len();
        let mut space_peak = 0.0f32;
        let mut out_peak = [0.0f32; 2];
        let mut min_lim = 1.0f32;

        // ---- the rest, per sample
        for i in 0..n {
            let mut x = self.scratch[i];
            if s.hp_on {
                x = self.hp[0].tick(x);
                x = self.hp[1].tick(x);
            }
            if s.mud_gain.abs() > 0.01 {
                x = self.mud.tick(x);
            }
            if s.harsh_gain.abs() > 0.01 {
                x = self.harsh.tick(x);
            }
            if use_match {
                for b in &mut self.matcher {
                    x = b.tick(x);
                }
            }
            if s.ds_on {
                x = self.deess.tick(x, s.ds_thresh, s.ds_range);
            }
            if s.punch_on {
                x = self.punch.tick(x, &e.punch);
            }
            if s.level_on {
                x = self.level.tick(x, e.level_amount, e.level_makeup);
            }
            if let Some((mode, drive, mix)) = e.sat {
                x = self.sat.tick(x, mode, drive, mix) * e.sat_trim;
            }
            x = self.low.tick(x);
            x = self.pres.tick(x);
            x = self.air.tick(x);

            // ---- stereo
            let (mut l, mut r) = (x, x);
            if run[1] {
                let (dl, dr) = self.doubler.tick(x, &e.dbl);
                l += dl * e.dbl_mix;
                r += dr * e.dbl_mix;
            }
            self.duck.tick(x);
            if run[2] {
                let (dl, dr) = self.delay.tick(x, &dly);
                let g = e.dly_mix * self.duck.gain(s.dly_duck);
                l += dl * g;
                r += dr * g;
                space_peak = space_peak.max((dl * g).abs());
            }
            if run[3] {
                let (rl, rr) = self.reverb.tick(x, &rev);
                let g = e.rev_mix * self.duck.gain(s.rev_duck);
                l += rl * g;
                r += rr * g;
                space_peak = space_peak.max((rl * g).abs());
            }

            // ---- dry/wet against the input delayed by exactly the tuner's latency
            self.dry[0][self.dry_w] = left[i];
            self.dry[1][self.dry_w] = right[i];
            let rd = (self.dry_w + dry_len - dry_delay.min(dry_len - 1)) % dry_len;
            let (dry_l, dry_r) = (self.dry[0][rd], self.dry[1][rd]);
            self.dry_w = (self.dry_w + 1) % dry_len;
            let ol = (dry_l + (l * trim - dry_l) * s.mix) * out_gain;
            let or = (dry_r + (r * trim - dry_r) * s.mix) * out_gain;
            let (ol, or, g) = self.limiter.tick(ol, or, ceiling, s.limiter);
            min_lim = min_lim.min(g);
            out_peak[0] = out_peak[0].max(ol.abs());
            out_peak[1] = out_peak[1].max(or.abs());
            left[i] = ol;
            right[i] = or;
        }

        // ---- Match My Voice bookkeeping and the editor's meters
        if let Some(sh) = shared {
            if self.learner.active {
                let progress = self.learner.voiced as f32 / self.learner.need as f32;
                sh.learn_progress.store(progress.min(1.0), Ordering::Relaxed);
                if self.learner.voiced >= self.learner.need {
                    self.learner.finish(sh, &self.tune);
                } else if self.learner.total >= self.learner.give_up {
                    if self.learner.voiced as f32 >= self.learner.need as f32 * 0.4 {
                        self.learner.finish(sh, &self.tune);
                    } else {
                        self.learner.active = false;
                        sh.learn_state.store(LEARN_FAILED, Ordering::Relaxed);
                    }
                }
            }
            Shared::meter(&sh.in_level, in_peak);
            Shared::meter(&sh.out_level[0], out_peak[0]);
            Shared::meter(&sh.out_level[1], out_peak[1]);
            Shared::meter(&sh.space_level, space_peak);
            Shared::meter(&sh.gate_gr, if s.gate_on { self.gate.reduction_db() } else { 0.0 });
            Shared::meter(&sh.deess_gr, if s.ds_on { self.deess.reduction_db() } else { 0.0 });
            Shared::meter(&sh.punch_gr, if s.punch_on { self.punch.reduction_db() } else { 0.0 });
            Shared::meter(&sh.level_gr, if s.level_on { self.level.reduction_db() } else { 0.0 });
            Shared::meter(&sh.limit_gr, -gain_to_db(min_lim));
            let live = self.tune.live;
            sh.pitch_midi.store(live.midi, Ordering::Relaxed);
            sh.pitch_target.store(live.target, Ordering::Relaxed);
            sh.pitch_voiced.store(live.voiced, Ordering::Relaxed);
            sh.pitch_correction.store(live.correction, Ordering::Relaxed);
            let k = self.tune.key.estimate;
            sh.key_root.store(k.root, Ordering::Relaxed);
            sh.key_minor.store(k.minor, Ordering::Relaxed);
            sh.key_valid.store(k.valid, Ordering::Relaxed);
            sh.key_confidence.store(k.confidence, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::from_params(&PersonaParams::default(), 120.0, 0)
    }

    /// A breathy, vowel-like test voice: a 150 Hz buzz with vibrato, phrases and an "s".
    fn voice(fs: f32, seconds: f32) -> Vec<f32> {
        let n = (fs * seconds) as usize;
        let mut seed = 0x1234_5678u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let mut phase = 0.0f32;
        (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                let hz = 150.0 * (1.0 + 0.01 * (t * 5.5 * std::f32::consts::TAU).sin());
                phase = (phase + hz / fs) % 1.0;
                let buzz: f32 = (1..12).map(|h| (phase * h as f32 * std::f32::consts::TAU).sin() / h as f32).sum();
                let within = t % 1.0;
                let env = if within < 0.7 { 1.0 } else { 0.0 };
                let s = if (0.7..0.8).contains(&within) { noise() * 0.15 } else { 0.0 };
                buzz * 0.12 * env + s
            })
            .collect()
    }

    #[test]
    fn every_module_on_stays_finite() {
        let fs = 48_000.0;
        let mut c = Chain::new(fs, 512);
        let mut s = settings();
        s.gate_on = true;
        s.tune_on = true;
        s.sat_on = true;
        s.dbl_on = true;
        s.dly_on = true;
        s.rev_on = true;
        s.match_amount = 1.0;
        s.match_db = [3.0, -2.0, 1.0, 0.0, 2.0, -3.0, 4.0, 1.0];
        c.prepare_range(&s);
        let x = voice(fs, 3.0);
        let (mut l, mut r) = (x.clone(), x);
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            c.process(cl, cr, &s, None);
        }
        assert!(l.iter().chain(r.iter()).all(|v| v.is_finite() && v.abs() <= 1.0));
        let rms = (l.iter().map(|v| v * v).sum::<f32>() / l.len() as f32).sqrt();
        assert!(rms > 0.01, "chain went silent (rms {rms})");
    }

    #[test]
    fn dry_mix_lines_up_with_the_wet_path() {
        // At 0% mix the output is the input, delayed by exactly the reported latency.
        let fs = 48_000.0;
        let mut c = Chain::new(fs, 256);
        let mut s = settings();
        s.tune_on = true;
        s.mix = 0.0;
        s.limiter = false;
        c.prepare_range(&s);
        let lat = c.latency(&s);
        let x = voice(fs, 1.0);
        let (mut l, mut r) = (x.clone(), x.clone());
        for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            c.process(cl, cr, &s, None);
        }
        let lim = c.limiter.latency();
        for i in lat..x.len() {
            assert!((l[i] - x[i - lat]).abs() < 1e-5, "sample {i}: {} vs {}", l[i], x[i - lat]);
        }
        assert!(lat > lim);
    }

    #[test]
    fn match_my_voice_measures_the_take() {
        let fs = 48_000.0;
        let mut c = Chain::new(fs, 512);
        let s = settings();
        let sh = Shared::default();
        sh.learn_request.store(true, Ordering::Relaxed);
        let x = voice(fs, 14.0);
        let (mut l, mut r) = (x.clone(), x);
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            c.process(cl, cr, &s, Some(&sh));
        }
        assert_eq!(sh.learn_state.load(Ordering::Relaxed), LEARN_DONE);
        let level = sh.learn.level_db.load(Ordering::Relaxed);
        assert!((-30.0..-10.0).contains(&level), "level {level}");
        let midi = sh.learn.median_midi.load(Ordering::Relaxed);
        // 150 Hz is a little above D3 (MIDI 50).
        assert!((49.0..=51.0).contains(&midi), "median {midi}");
    }

    #[test]
    fn every_style_renders_cleanly() {
        let fs = 44_100.0;
        let defaults = PersonaParams::default();
        let x = voice(fs, 2.0);
        for style in crate::styles::STYLES {
            let mut s = Settings::from_params(&defaults, 120.0, 0);
            for (id, v) in style.values {
                apply_plain(&mut s, id, *v);
            }
            let mut c = Chain::new(fs, 512);
            c.prepare_range(&s);
            let (mut l, mut r) = (x.clone(), x.clone());
            for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
                c.process(cl, cr, &s, None);
            }
            let peak = l.iter().chain(r.iter()).fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(l.iter().chain(r.iter()).all(|v| v.is_finite()), "{}: not finite", style.name);
            assert!(peak > 0.02 && peak <= 0.9, "{}: peak {peak}", style.name);
        }
    }

    struct Measure {
        fet_p95: f32,
        opto_avg: f32,
        deess_p95: f32,
        gain_db: f32,
    }

    fn p95(mut v: Vec<f32>) -> f32 {
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        v[(v.len() as f32 * 0.95) as usize]
    }

    fn rms_db(x: &[f32]) -> f32 {
        10.0 * (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).max(1e-12).log10()
    }

    /// Gain reduction the styles' compressors apply to the test vocal, and the level change.
    fn measure(s: &Settings, x: &[f32], fs: f32) -> Measure {
        let mut c = Chain::new(fs, 64);
        c.prepare_range(s);
        let (mut l, mut r) = (x.to_vec(), x.to_vec());
        let (mut fet, mut opto, mut ds) = (Vec::new(), Vec::new(), Vec::new());
        let settle = (fs * 0.5) as usize / 64;
        for (i, (cl, cr)) in l.chunks_mut(64).zip(r.chunks_mut(64)).enumerate() {
            let inp = &x[i * 64..i * 64 + cl.len()];
            let rms = (inp.iter().map(|v| v * v).sum::<f32>() / inp.len() as f32).sqrt();
            c.process(cl, cr, s, None);
            if i > settle && rms > 0.01 {
                fet.push(if s.punch_on { c.punch.reduction_db() } else { 0.0 });
                opto.push(if s.level_on { c.level.reduction_db() } else { 0.0 });
                ds.push(if s.ds_on { c.deess.reduction_db() } else { 0.0 });
            }
        }
        let skip = (fs * 0.5) as usize;
        let n = opto.len().max(1) as f32;
        Measure {
            fet_p95: p95(fet),
            opto_avg: opto.iter().sum::<f32>() / n,
            deess_p95: p95(ds),
            gain_db: rms_db(&l[skip..]).max(rms_db(&r[skip..])) - rms_db(&x[skip..]),
        }
    }

    fn style_settings(style: &crate::styles::Style) -> Settings {
        let mut s = settings();
        for (id, v) in style.values {
            apply_plain(&mut s, id, *v);
        }
        s
    }

    /// Every style's compressors work as hard as its `squeeze` says on the test vocal at the
    /// design level, and its trim brings it out about as loud as it went in.
    #[test]
    fn styles_are_gain_staged() {
        let fs = 48_000.0;
        let x = crate::testsignal::vocal(fs, 4.0);
        let mut bad = Vec::new();
        for style in crate::styles::STYLES {
            let s = style_settings(style);
            let m = measure(&s, &x, fs);
            let (fet, opto) = style.squeeze;
            if s.punch_on && (m.fet_p95 - fet).abs() > 1.5 {
                bad.push(format!("{}: Punch takes {:.1} dB, designed for {fet}", style.name, m.fet_p95));
            }
            if s.level_on && (m.opto_avg - opto).abs() > 1.0 {
                bad.push(format!("{}: Level takes {:.1} dB, designed for {opto}", style.name, m.opto_avg));
            }
            if !(-2.5..=3.5).contains(&m.gain_db) {
                bad.push(format!("{}: comes out {:+.1} dB", style.name, m.gain_db));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Turning the vibe knobs changes the sound, not mainly the volume.
    #[test]
    fn vibe_knobs_hold_the_level() {
        let fs = 48_000.0;
        let x = crate::testsignal::vocal(fs, 4.0);
        for name in ["J. Cole", "Playboi Carti", "Drake"] {
            let base = style_settings(crate::styles::find(name).unwrap());
            for (k, label) in ["tune", "punch", "grit", "air", "space", "width"].iter().enumerate() {
                for v in [0.0, 1.0] {
                    let mut s = base.clone();
                    s.vibe[k] = v;
                    let g = measure(&s, &x, fs).gain_db;
                    assert!((-1.5..=2.5).contains(&g), "{name}: {label} at {v} comes out {g:+.1} dB");
                }
            }
        }
    }

    /// Re-derives each style's Punch threshold, Level amount and trim for the gain staging above.
    /// `cargo test --release --lib calibrate -- --ignored --nocapture`, then paste the output
    /// through tools/apply_calibration.py.
    #[test]
    #[ignore]
    fn calibrate_styles() {
        let fs = 48_000.0;
        let x = crate::testsignal::vocal(fs, 4.0);
        for style in crate::styles::STYLES {
            let mut s = style_settings(style);
            s.limiter = false;
            s.trim_db = 0.0;
            let (fet_target, opto_target) = style.squeeze;
            if s.punch_on {
                let (mut lo, mut hi) = (-45.0f32, 0.0f32);
                for _ in 0..14 {
                    s.punch_thresh = 0.5 * (lo + hi);
                    if measure(&s, &x, fs).fet_p95 > fet_target {
                        lo = s.punch_thresh;
                    } else {
                        hi = s.punch_thresh;
                    }
                }
                s.punch_thresh = (s.punch_thresh * 2.0).round() / 2.0;
            }
            if s.level_on {
                let (mut lo, mut hi) = (0.0f32, 1.0f32);
                for _ in 0..12 {
                    s.level_amount = 0.5 * (lo + hi);
                    if measure(&s, &x, fs).opto_avg < opto_target {
                        lo = s.level_amount;
                    } else {
                        hi = s.level_amount;
                    }
                }
                s.level_amount = (s.level_amount * 100.0).round() / 100.0;
            }
            let m = measure(&s, &x, fs);
            let trim = ((0.5 - m.gain_db) * 10.0).round() / 10.0;
            s.trim_db = trim;
            let check = measure(&s, &x, fs);
            println!(
                "CAL|{}|{}|{}|{}|fet {:.1}/{:.1} opto {:.1}/{:.1} deess {:.1} out {:+.1}",
                style.name, s.punch_thresh, s.level_amount, trim, m.fet_p95, fet_target, m.opto_avg, opto_target, m.deess_p95, check.gain_db
            );
        }
    }

    /// Enough of the id -> field mapping to render the styles in tests.
    fn apply_plain(s: &mut Settings, id: &str, v: f32) {
        use crate::dsp::saturation::SatMode::*;
        let on = v > 0.5;
        match id {
            "t_on" => s.tune_on = on,
            "t_speed" => s.tune_speed_ms = v,
            "t_hum" => s.tune_humanize = v,
            "t_flex" => s.tune_flex = v,
            "t_amt" => s.tune_amount = v,
            "hp_f" => s.hp_freq = v,
            "mud_g" => s.mud_gain = v,
            "mud_f" => s.mud_freq = v,
            "hsh_g" => s.harsh_gain = v,
            "hsh_f" => s.harsh_freq = v,
            "ds_thr" => s.ds_thresh = v,
            "ds_rng" => s.ds_range = v,
            "fet_thr" => s.punch_thresh = v,
            "fet_rat" => s.punch_ratio = v,
            "fet_att" => s.punch_attack = v,
            "fet_rel" => s.punch_release = v,
            "fet_mk" => s.punch_makeup = v,
            "fet_col" => s.punch_color = v,
            "opt_amt" => s.level_amount = v,
            "opt_mk" => s.level_makeup = v,
            "sat_on" => s.sat_on = on,
            "sat_mode" => s.sat_mode = [Tape, Tube, Fuzz, Phone, LoFi][v as usize],
            "sat_drv" => s.sat_drive = v,
            "sat_mix" => s.sat_mix = v,
            "lo_g" => s.low_gain = v,
            "lo_f" => s.low_freq = v,
            "pr_g" => s.pres_gain = v,
            "pr_f" => s.pres_freq = v,
            "air_g" => s.air_gain = v,
            "air_f" => s.air_freq = v,
            "dbl_on" => s.dbl_on = on,
            "dbl_mix" => s.dbl_mix = v,
            "dbl_det" => s.dbl_detune = v,
            "dbl_dly" => s.dbl_delay = v,
            "dbl_wid" => s.dbl_width = v,
            "dly_on" => s.dly_on = on,
            "dly_div" => s.dly_beats = DIVISION_BEATS[v as usize],
            "dly_fb" => s.dly_feedback = v,
            "dly_mix" => s.dly_mix = v,
            "dly_pp" => s.dly_ping_pong = on,
            "dly_lp" => s.dly_lp = v,
            "dly_duck" => s.dly_duck = v,
            "rev_on" => s.rev_on = on,
            "rev_type" => s.rev_kind = [ReverbType::Plate, ReverbType::Room, ReverbType::Hall, ReverbType::Chamber][v as usize],
            "rev_size" => s.rev_size = v,
            "rev_dec" => s.rev_decay = v,
            "rev_pre" => s.rev_predelay = v,
            "rev_damp" => s.rev_damping = v,
            "rev_mix" => s.rev_mix = v,
            "rev_duck" => s.rev_duck = v,
            "trim" => s.trim_db = v,
            other => panic!("style uses {other}, which the test mapping doesn't know"),
        }
    }
}
