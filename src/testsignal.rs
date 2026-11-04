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
