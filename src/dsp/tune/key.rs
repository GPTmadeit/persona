//! Key and mode from the sung pitch: a pitch-class histogram weighted by how long and how
//! confidently each note was held, correlated against the Krumhansl-Schmuckler profiles. Working
//! from detected f0 instead of a spectral chroma is cheaper and, for a solo voice, more accurate.
//! Ported from HelixTune's KeyDetector (MIT).

const MAJOR: [f32; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR: [f32; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct KeyEstimate {
    pub root: i32,
    pub minor: bool,
    /// Margin over the runner-up, 0..1.
    pub confidence: f32,
    pub valid: bool,
}

pub struct KeyDetector {
    decay: f32,
    interval: u32,
    until: u32,
    chroma: [f32; 12],
    total: f32,
    pub estimate: KeyEstimate,
}

impl KeyDetector {
    pub fn new(hop_s: f32) -> Self {
        Self {
            // ~20 s of memory: a whole section, but a key change still gets picked up.
            decay: (-hop_s / 20.0).exp(),
            interval: ((0.2 / hop_s) as u32).max(1),
            until: 0,
            chroma: [0.0; 12],
            total: 0.0,
            estimate: KeyEstimate::default(),
        }
    }

    pub fn reset(&mut self) {
        self.chroma = [0.0; 12];
        self.total = 0.0;
        self.until = 0;
        self.estimate = KeyEstimate::default();
    }

    pub fn chroma(&self) -> [f32; 12] {
        let t = self.total.max(1e-6);
        self.chroma.map(|c| c / t)
    }

    /// Seconds of confident singing in the histogram (it needs a few before it means anything).
    pub fn weight(&self) -> f32 {
        self.total
    }

    pub fn push(&mut self, midi: f32, voiced: bool, weight: f32) {
        for c in &mut self.chroma {
            *c *= self.decay;
        }
        self.total *= self.decay;
        if voiced && midi > 0.0 && midi.is_finite() {
            let pc = (midi.round() as i32).rem_euclid(12) as usize;
            let w = weight.clamp(0.0, 1.0);
            self.chroma[pc] += w;
            self.total += w;
        }
        if self.until == 0 {
            self.until = self.interval;
            self.recompute();
        }
        self.until -= 1;
    }

    fn recompute(&mut self) {
        if self.total < 20.0 {
            self.estimate.valid = false;
            return;
        }
        let mean = self.chroma.iter().sum::<f32>() / 12.0;
        let centred: [f32; 12] = std::array::from_fn(|i| self.chroma[i] - mean);
        let norm = centred.iter().map(|c| c * c).sum::<f32>().sqrt();
        if norm < 1e-6 {
            self.estimate.valid = false;
            return;
        }
        let correlate = |profile: &[f32; 12], rot: usize| {
            let pm = profile.iter().sum::<f32>() / 12.0;
            let (mut dot, mut pn) = (0.0, 0.0);
            for i in 0..12 {
                let p = profile[(i + 12 - rot) % 12] - pm;
                dot += centred[i] * p;
                pn += p * p;
            }
            let pn = pn.sqrt();
            if pn > 1e-6 {
                dot / (norm * pn)
            } else {
                0.0
            }
        };
        let (mut best, mut second, mut root, mut minor) = (-2.0f32, -2.0f32, 0, false);
        for r in 0..12 {
            for (m, profile) in [(false, &MAJOR), (true, &MINOR)] {
                let s = correlate(profile, r);
                if s > best {
                    second = best;
                    best = s;
                    root = r as i32;
                    minor = m;
                } else if s > second {
                    second = s;
                }
            }
        }
        // The margin, not the correlation: a take that fits C major also fits A minor.
        let margin = ((best - second) * 4.0).clamp(0.0, 1.0);
        self.estimate = KeyEstimate { root, minor, confidence: margin * best.clamp(0.0, 1.0), valid: best > 0.2 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hears_a_minor_melody() {
        let mut k = KeyDetector::new(0.005);
        // A natural minor phrase leaning on A, C and E, sung for ~16 s.
        let melody = [57.0, 60.0, 64.0, 62.0, 60.0, 59.0, 57.0, 64.0, 65.0, 64.0, 62.0, 60.0, 57.0, 52.0];
        for i in 0..3200 {
            let note = melody[(i / 230) % melody.len()];
            k.push(note, true, 1.0);
        }
        let e = k.estimate;
        assert!(e.valid);
        assert_eq!((e.root, e.minor), (9, true), "{e:?}");
    }
}
