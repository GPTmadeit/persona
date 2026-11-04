//! Spots unpitched consonants so correction leaves them alone. A tracker always reports
//! *something* for an "s" or a "t"; correcting that is the lisping, warbling artefact. Cheap and
//! time-domain: fricatives sit high, cross zero constantly and are aperiodic. Ported from
//! HelixTune's TransientGuard (MIT).

pub struct Guard {
    sensitivity: f32,
    smoothed: f32,
    attack: f32,
    release: f32,
}

impl Guard {
    pub fn new(hop_s: f32) -> Self {
        // Catch the consonant at once, let go slowly so it doesn't chatter across a "sh".
        Self {
            sensitivity: 0.6,
            smoothed: 0.0,
            attack: 1.0 - (-hop_s / 0.004).exp(),
            release: 1.0 - (-hop_s / 0.070).exp(),
        }
    }

    pub fn reset(&mut self) {
        self.smoothed = 0.0;
    }

    pub fn set_sensitivity(&mut self, s: f32) {
        self.sensitivity = s.clamp(0.0, 1.0);
    }

    /// How much this hop should be left uncorrected, 0..1.
    pub fn process(&mut self, frame: &[f32], periodicity: f32) -> f32 {
        if self.sensitivity <= 1e-4 || frame.len() < 2 {
            self.smoothed += -self.smoothed * self.release;
            return self.smoothed;
        }
        let (mut total, mut high) = (0.0f64, 0.0f64);
        let mut crossings = 0;
        let mut prev = frame[0];
        for &x in &frame[1..] {
            let d = x - prev;
            total += x as f64 * x as f64;
            high += d as f64 * d as f64;
            if (x >= 0.0) != (prev >= 0.0) {
                crossings += 1;
            }
            prev = x;
        }
        if total < 1e-10 {
            self.smoothed += -self.smoothed * self.release;
            return self.smoothed;
        }
        let hf = (high / (total * 4.0)).clamp(0.0, 1.0) as f32;
        let zcr = (crossings as f32 / frame.len() as f32 * 4.0).clamp(0.0, 1.0);
        // Bright *and* aperiodic: a vowel can be bright, a quiet passage can be aperiodic.
        let spectral = 0.5 * hf + 0.5 * zcr;
        let score = ((spectral * (1.0 - periodicity.clamp(0.0, 1.0)) - 0.12) * 3.0).clamp(0.0, 1.0) * self.sensitivity;
        let k = if score > self.smoothed { self.attack } else { self.release };
        self.smoothed += (score - self.smoothed) * k;
        self.smoothed
    }
}
