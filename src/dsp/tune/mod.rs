//! Pitch correction: detect, stabilise, guard consonants, pick a target, shift. Built from
//! HelixTune's engine (MIT, github.com/GPTmadeit/HelixTune), cut down to one mono lead voice.
//!
//! Analysis runs on a fixed ~5 ms hop whatever the host block size, and keeps running while
//! correction is off: the editor shows the live note and Match My Voice reads the key from it.

pub mod detector;
pub mod guard;
pub mod key;
pub mod psola;
pub mod retune;
pub mod scale;
pub mod stabilizer;

use detector::{Detector, VoiceRange};
use guard::Guard;
use key::KeyDetector;
use psola::Psola;
use retune::{Retune, RetuneParams};
use stabilizer::Stabilizer;

#[derive(Clone, Copy, Debug)]
pub struct TuneSettings {
    pub enabled: bool,
    pub range: VoiceRange,
    pub root: i32,
    pub mask: u16,
    pub retune: RetuneParams,
    /// 0..1 of the correction applied.
    pub amount: f32,
    /// Consonant protection, 0..1.
    pub guard: f32,
}

/// What the tuner heard on the last hop, for the editor.
#[derive(Clone, Copy, Default, Debug)]
pub struct TuneLive {
    pub midi: f32,
    pub voiced: bool,
    pub target: f32,
    pub correction: f32,
    pub confidence: f32,
}

pub struct TuneEngine {
    hop: usize,
    hop_counter: usize,
    history: Vec<f32>,
    detector: Detector,
    stabilizer: Stabilizer,
    retune: Retune,
    guard: Guard,
    pub key: KeyDetector,
    psola: Psola,
    range: VoiceRange,
    fs: f32,
    ratio: f32,
    period: f32,
    voiced: bool,
    running: bool,
    pub live: TuneLive,
}

impl TuneEngine {
    pub fn new(fs: f32, max_block: usize) -> Self {
        // ~5 ms analysis hop, a power of two.
        let hop = ((fs * 0.005) as usize).next_power_of_two().clamp(128, 1024);
        let hop_s = hop as f32 / fs;
        let detector = Detector::new(fs);
        let history = vec![0.0; detector.max_frame_size().max(hop)];
        let (lowest, _) = VoiceRange::Low.hz();
        let mut e = Self {
            hop,
            hop_counter: 0,
            history,
            detector,
            stabilizer: Stabilizer::default(),
            retune: Retune::new(hop_s),
            guard: Guard::new(hop_s),
            key: KeyDetector::new(hop_s),
            psola: Psola::new(fs, lowest, max_block.max(hop)),
            range: VoiceRange::Low,
            fs,
            ratio: 1.0,
            period: 200.0,
            voiced: false,
            running: false,
            live: TuneLive::default(),
        };
        e.set_range(VoiceRange::Mid);
        e
    }

    /// Algorithmic delay while correcting: PSOLA reads up to three periods of the lowest note.
    pub fn latency(&self) -> usize {
        self.psola.latency()
    }

    pub fn latency_for(&self, enabled: bool) -> usize {
        if enabled {
            self.psola.latency()
        } else {
            0
        }
    }

    pub fn set_range(&mut self, r: VoiceRange) {
        if r == self.range {
            return;
        }
        self.range = r;
        self.detector.set_range(r);
        self.psola.set_min_frequency(r.hz().0);
        self.stabilizer.reset();
    }

    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.hop_counter = 0;
        self.detector.reset();
        self.stabilizer.reset();
        self.retune.reset();
        self.guard.reset();
        self.psola.reset();
        self.ratio = 1.0;
        self.voiced = false;
        self.running = false;
    }

    /// Analyses `x` (mono) and, when enabled, corrects it in place.
    pub fn process(&mut self, x: &mut [f32], s: &TuneSettings) {
        self.set_range(s.range);
        self.guard.set_sensitivity(s.guard);
        if s.enabled && !self.running {
            // PSOLA stopped consuming input while off; a clean start beats stale marks.
            self.psola.reset();
        }
        self.running = s.enabled;
        let mut pos = 0;
        while pos < x.len() {
            let chunk = (self.hop - self.hop_counter).min(x.len() - pos);
            let h = self.history.len();
            if chunk >= h {
                self.history.copy_from_slice(&x[pos + chunk - h..pos + chunk]);
            } else {
                self.history.copy_within(chunk.., 0);
                self.history[h - chunk..].copy_from_slice(&x[pos..pos + chunk]);
            }
            if s.enabled {
                self.psola.process(&mut x[pos..pos + chunk], self.ratio, self.period, self.voiced);
            }
            pos += chunk;
            self.hop_counter += chunk;
            if self.hop_counter >= self.hop {
                self.hop_counter = 0;
                self.analyse(s);
            }
        }
    }

    fn analyse(&mut self, s: &TuneSettings) {
        let raw = self.detector.process(&self.history);
        let det = self.stabilizer.process(&raw, self.range.hz());
        let h = self.history.len();
        let consonant = self.guard.process(&self.history[h - self.hop..], det.confidence);
        self.key.push(det.midi, det.voiced, det.confidence * (raw.rms * 40.0).clamp(0.0, 1.0));
        let target = if det.voiced { scale::nearest(det.midi, s.root, s.mask) } else { None };
        let out = self.retune.process(det.midi, det.voiced, target, &s.retune);
        // A consonant has no pitch: fade the correction out across it.
        let semis = out.correction * s.amount * (1.0 - consonant);
        self.ratio = 2f32.powf(semis / 12.0);
        self.voiced = det.voiced;
        if det.voiced && det.hz > 1.0 {
            self.period = self.fs / det.hz;
        }
        self.live = TuneLive {
            midi: if det.voiced { det.midi } else { 0.0 },
            voiced: det.voiced,
            target: out.target,
            correction: semis,
            confidence: det.confidence,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_a_flat_note_onto_the_scale() {
        let fs = 48_000.0;
        let mut e = TuneEngine::new(fs, 512);
        let s = TuneSettings {
            enabled: true,
            range: VoiceRange::Mid,
            root: 0,
            mask: scale::SCALES[1].1, // C major
            retune: RetuneParams { retune_ms: 0.0, humanize: 0.0, flex: 0.0 },
            amount: 1.0,
            guard: 0.0,
        };
        // 40 cents flat of A3 (220 Hz): A is in C major, so it should come out at 220 Hz.
        let hz = 220.0 * 2f32.powf(-0.40 / 12.0);
        let n = 96_000;
        let mut x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                (1..6).map(|h| (std::f32::consts::TAU * hz * h as f32 * t).sin() / h as f32).sum::<f32>() * 0.3
            })
            .collect();
        for chunk in x.chunks_mut(512) {
            e.process(chunk, &s);
        }
        let mut d = Detector::new(fs);
        let r = d.process(&x[n - d.frame_size()..]);
        let cents = 1200.0 * (r.hz / 220.0).log2();
        assert!(cents.abs() < 6.0, "came out at {} Hz ({cents:.1} cents from A3)", r.hz);
        assert!(e.latency() > 0);
    }
}
