//! Temporal decoding of the detector's candidate lattice: an online Viterbi forward pass where
//! staying near the previous pitch is cheap and leaping is expensive but never forbidden. One
//! frame can't tell 220 Hz from 110 Hz; a singer doesn't jump an octave for 5 ms and back.
//! Ported from HelixTune's PitchStabilizer (MIT).

use super::detector::{Detection, MAX_CANDIDATES};

#[derive(Clone, Copy, Default, Debug)]
pub struct Stable {
    pub hz: f32,
    pub midi: f32,
    pub confidence: f32,
    pub voiced: bool,
}

pub struct Stabilizer {
    smoothing: f32,
    tracking: f32,
    prev_hz: [f32; MAX_CANDIDATES],
    prev_cost: [f32; MAX_CANDIDATES],
    prev_count: usize,
    unvoiced_cost: f32,
    was_voiced: bool,
    last_hz: f32,
    unvoiced_run: u32,
}

impl Default for Stabilizer {
    fn default() -> Self {
        Self {
            smoothing: 0.55,
            tracking: 0.5,
            prev_hz: [0.0; MAX_CANDIDATES],
            prev_cost: [0.0; MAX_CANDIDATES],
            prev_count: 0,
            unvoiced_cost: 0.0,
            was_voiced: false,
            last_hz: 0.0,
            unvoiced_run: 0,
        }
    }
}

fn to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

impl Stabilizer {
    pub fn reset(&mut self) {
        *self = Self { smoothing: self.smoothing, tracking: self.tracking, ..Self::default() };
    }

    pub fn set_tracking(&mut self, t: f32) {
        self.tracking = t.clamp(0.0, 1.0);
    }

    pub fn process(&mut self, det: &Detection, (lo, hi): (f32, f32)) -> Stable {
        let mut out = Stable::default();
        let jump_penalty = 0.25 + self.smoothing * 1.15;
        let silence = (det.rms * 60.0).clamp(0.0, 1.0);
        let unvoiced_emission = (0.42 + self.tracking * 0.2) * silence + (1.0 - silence) * 0.02;

        let mut cur_hz = [0.0f32; MAX_CANDIDATES];
        let mut cur_cost = [0.0f32; MAX_CANDIDATES];
        let mut cur_emission = [0.0f32; MAX_CANDIDATES];
        let mut cur = 0;
        for c in &det.candidates[..det.count] {
            if c.hz < lo || c.hz > hi || cur >= MAX_CANDIDATES {
                continue;
            }
            // Arriving from unvoiced: a note onset.
            let mut best = c.cost + self.unvoiced_cost + 0.30;
            for j in 0..self.prev_count {
                let octaves = (c.hz / self.prev_hz[j]).log2().abs();
                // Sub-linear: a semitone of vibrato is nearly free, an octave is costly.
                best = best.min(self.prev_cost[j] + jump_penalty * (octaves * 4.0).sqrt() + c.cost);
            }
            cur_hz[cur] = c.hz;
            cur_cost[cur] = best;
            cur_emission[cur] = c.cost;
            cur += 1;
        }
        let best_prev = self.prev_cost[..self.prev_count].iter().copied().fold(f32::INFINITY, f32::min);
        let mut new_unvoiced = self.unvoiced_cost + unvoiced_emission;
        if self.prev_count > 0 {
            new_unvoiced = new_unvoiced.min(best_prev + unvoiced_emission + 0.30);
        }
        let mut best_state = None;
        let mut best_score = new_unvoiced;
        for i in 0..cur {
            if cur_cost[i] < best_score {
                best_score = cur_cost[i];
                best_state = Some(i);
            }
        }
        // Only differences matter: renormalise so a long take can't drift toward infinity.
        for c in &mut cur_cost[..cur] {
            *c -= best_score;
        }
        new_unvoiced -= best_score;
        self.prev_hz[..cur].copy_from_slice(&cur_hz[..cur]);
        self.prev_cost[..cur].copy_from_slice(&cur_cost[..cur]);
        self.prev_count = cur;
        self.unvoiced_cost = new_unvoiced;

        let Some(b) = best_state else {
            self.unvoiced_run = (self.unvoiced_run + 1).min(1000);
            // A brief dropout inside a note is the tracker stumbling on a consonant: hold.
            if self.was_voiced && self.unvoiced_run < 6 && self.last_hz > 0.0 {
                return Stable { hz: self.last_hz, midi: to_midi(self.last_hz), confidence: 0.25, voiced: true };
            }
            self.was_voiced = false;
            return out;
        };
        self.unvoiced_run = 0;
        out.hz = cur_hz[b];
        out.midi = to_midi(out.hz);
        out.confidence = (1.0 - cur_emission[b]).clamp(0.0, 1.0);
        out.voiced = true;
        self.was_voiced = true;
        self.last_hz = out.hz;
        out
    }
}
