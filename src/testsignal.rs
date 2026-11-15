//! A synthetic rap vocal for tests, calibration and the offline renderer: a buzzing voice with
//! vowel formants, eight syllables a second over a little A-minor melody, sharp "s" sounds and
//! breaths between phrases. About -18 dBFS RMS, the level the styles are designed for.

use std::f32::consts::TAU;

pub fn vocal(fs: f32, seconds: f32) -> Vec<f32> {
    let n = (fs * seconds) as usize;
    let melody = [57.0f32, 57.0, 60.0, 59.0, 57.0, 55.0, 57.0, 64.0, 62.0, 60.0, 59.0, 57.0];
    let formant = |f: f32, vowel: usize| {
        let (f1, f2, f3) = [(730.0, 1090.0, 2440.0), (530.0, 1840.0, 2480.0), (300.0, 870.0, 2240.0)][vowel % 3];
        let bump = |c: f32, w: f32| 1.0 / (1.0 + ((f - c) / w).powi(2));
        bump(f1, 160.0) + 0.7 * bump(f2, 220.0) + 0.35 * bump(f3, 300.0) + 0.02
    };
    let mut seed = 0x9E37_79B9u32;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32 * 2.0 - 1.0
    };
    let mut phase = 0.0f32;
    let mut last_noise = 0.0f32;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / fs;
        let syl = (t * 8.0) as usize;
        let within = (t * 8.0).fract();
        let phrase = (t / 2.0).fract();
        let note = melody[syl % melody.len()];
        let hz = 440.0 * 2f32.powf((note - 69.0) / 12.0) * (1.0 + 0.006 * (t * 5.3 * TAU).sin());
        phase = (phase + hz / fs).fract();
        let mut v = 0.0;
        let mut h = 1;
        while h as f32 * hz < 11000.0 {
            v += formant(h as f32 * hz, syl) * (TAU * phase * h as f32).sin() / (h as f32).sqrt();
            h += 1;
        }
        // Syllable envelope, with a gap at the end of every 2 s phrase.
        let env = if phrase > 0.85 { 0.0 } else { (within * 12.0).min(1.0) * ((1.0 - within) * 5.0).min(1.0) };
        let w = noise();
        let hiss = w - last_noise;
        last_noise = w;
        let talking = phrase <= 0.85;
        let s = if talking && syl % 5 == 2 && within > 0.7 { hiss * 0.12 } else { 0.0 };
        let breath = if talking { hiss * 0.006 * env } else { hiss * 0.003 };
        out.push(v * 0.2 * env + s + breath);
    }
    out
}

/// A synthetic stereo mix for the mastering tests: a kick on every beat at 120 BPM, a sub bass
/// line, a chord pad that leans left and right, hats and the test vocal in the middle. It has
/// what a real mix has that matters here: sharp transients over sustained material, most of
/// the energy at the bottom, a top end that rolls off, and some stereo. About -20 LUFS with
/// peaks around -6 dBFS.
pub fn mix(fs: f32, seconds: f32) -> (Vec<f32>, Vec<f32>) {
    let n = (fs * seconds) as usize;
    let voice = vocal(fs, seconds);
    let mut seed = 0x51ED_270Bu32;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32 * 2.0 - 1.0
    };
    let bass_notes = [45.0f32, 45.0, 48.0, 43.0];
    let chord = [57.0f32, 60.0, 64.0, 67.0];
    let hz_of = |m: f32| 440.0 * 2f32.powf((m - 69.0) / 12.0);
    let (mut left, mut right) = (Vec::with_capacity(n), Vec::with_capacity(n));
    let (mut hat_l, mut hat_r) = (0.0f32, 0.0f32);
    let mut kick_phase = 0.0f32;
    let mut bass_phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / fs;
        let beat = t * 2.0;
        let in_beat = beat.fract();
        // Kick: a sine dropping from 150 to 50 Hz, with a click on the front.
        let kick_hz = 50.0 + 100.0 * (-in_beat * 30.0).exp();
        kick_phase = (kick_phase + kick_hz / fs).fract();
        if in_beat < 1.0 / fs * 2.0 {
            kick_phase = 0.0;
        }
        let kick = (TAU * kick_phase).sin() * (-in_beat * 7.0).exp() * 0.5 + (-in_beat * 900.0).exp() * 0.25;
        // Sub bass, ducking under the kick the way a sidechained 808 does.
        let bass_hz = hz_of(bass_notes[(beat / 4.0) as usize % bass_notes.len()] - 12.0);
        bass_phase = (bass_phase + bass_hz / fs).fract();
        let bass = ((TAU * bass_phase).sin() + 0.3 * (TAU * 2.0 * bass_phase).sin()) * 0.22 * (0.35 + 0.65 * (in_beat * 3.0).min(1.0));
        // Pad: four notes, each a slightly different level left and right.
        let (mut pad_l, mut pad_r) = (0.0, 0.0);
        for (k, m) in chord.iter().enumerate() {
            let hz = hz_of(*m);
            let v = (TAU * hz * t).sin() + 0.4 * (TAU * 2.0 * hz * t + 0.6).sin() + 0.2 * (TAU * 3.0 * hz * t + 1.1).sin();
            let pan = [0.25f32, 0.75, 0.4, 0.6][k];
            pad_l += v * (1.0 - pan);
            pad_r += v * pan;
        }
        // Hats on the off-beats: differentiated noise, different on each side.
        let hat_env = if in_beat >= 0.5 { (-(in_beat - 0.5) * 45.0).exp() } else { 0.0 };
        let (a, b) = (noise(), noise());
        let (hl, hr) = (a - hat_l, b - hat_r);
        hat_l = a;
        hat_r = b;
        let centre = kick + bass + voice[i] * 0.55;
        left.push(centre + pad_l * 0.035 + hl * 0.05 * hat_env);
        right.push(centre + pad_r * 0.035 + hr * 0.05 * hat_env);
    }
    (left, right)
}
