//! Where the pitch should go and how fast it should get there. Everything that makes
//! correction musical rather than robotic is decided here. Ported from HelixTune's
//! RetuneEngine (MIT), without the graph/MIDI target modes Persona doesn't need.

#[derive(Clone, Copy, Debug)]
pub struct RetuneParams {
    /// 0 = instant snap: the hard-tune sound.
    pub retune_ms: f32,
    /// 0..1: relaxes the retune on held notes only, so onsets stay tight.
    pub humanize: f32,
    /// 0..1: ignores pitch far from a target, so scoops and slides survive.
    pub flex: f32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct RetuneOut {
    pub target: f32,
    /// Semitones to move the input by.
    pub correction: f32,
}

pub struct Retune {
    hop_s: f32,
    carrier: f32,
    smoothed: f32,
    prev: f32,
    note_age: f32,
    was_voiced: bool,
    have_carrier: bool,
}

impl Retune {
    pub fn new(hop_s: f32) -> Self {
        Self { hop_s, carrier: 0.0, smoothed: 0.0, prev: 0.0, note_age: 0.0, was_voiced: false, have_carrier: false }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.hop_s);
    }

    fn alpha(&self, tau_s: f32) -> f32 {
        if tau_s <= 1e-5 {
            1.0
        } else {
            1.0 - (-self.hop_s / tau_s).exp()
        }
    }

    pub fn process(&mut self, midi: f32, voiced: bool, target: Option<f32>, p: &RetuneParams) -> RetuneOut {
        if !voiced {
            // Relax back to zero so the next note doesn't start with a jump applied.
            self.smoothed *= 0.9;
            self.was_voiced = false;
            self.have_carrier = false;
            self.note_age = 0.0;
            return RetuneOut::default();
        }
        let onset = !self.was_voiced || (midi - self.prev).abs() > 0.7;
        if onset || !self.have_carrier {
            self.carrier = midi;
            self.have_carrier = true;
            self.note_age = 0.0;
        } else {
            self.note_age += self.hop_s;
        }
        // A ~120 ms one-pole separates the contour from the singer's vibrato.
        self.carrier += (midi - self.carrier) * self.alpha(0.12);
        self.prev = midi;
        self.was_voiced = true;
        let Some(target) = target else {
            return RetuneOut::default();
        };
        let deviation = midi - target;
        let mut weight = 1.0;
        if p.flex > 1e-4 {
            let inner = 0.5 * (1.0 - p.flex);
            let mag = deviation.abs();
            if mag > inner {
                let x = ((mag - inner) / (0.5 - inner).max(1e-4)).clamp(0.0, 1.0);
                weight = 1.0 - x * x * (3.0 - 2.0 * x);
            }
        }
        let mut retune_ms = p.retune_ms;
        if p.humanize > 1e-4 {
            let sustain = (self.note_age / 0.2).clamp(0.0, 1.0);
            retune_ms *= 1.0 + p.humanize * 4.0 * sustain;
        }
        let desired = (target - midi) * weight;
        self.smoothed += (desired - self.smoothed) * self.alpha(retune_ms * 0.001);
        RetuneOut { target, correction: self.smoothed }
    }
}
