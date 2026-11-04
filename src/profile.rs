//! Match My Voice, the part that decides: turns what the audio thread measured about the
//! singer into settings, and keeps the two that depend on the style (de-esser threshold and
//! high-pass) so every style loaded afterwards is fitted to the same voice.

use crate::params::{MATCH_BANDS, MATCH_HZ};
use crate::styles::Style;

/// What Match learned that has to be re-applied on top of each style. Saved with the project.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct VoiceProfile {
    pub learned: bool,
    /// How much hotter (+) or softer (-) this singer's esses are than the styles assume, at the
    /// level the styles expect, dB.
    pub sib_excess_db: f32,
    /// Highest high-pass that leaves the singer's fundamental alone, Hz; 0 = no limit.
    pub hp_cap_hz: f32,
}

/// What the Learner measured; see `shared::LearnResult`.
#[derive(Clone, Debug)]
pub struct Measured {
    pub level_db: f32,
    pub bands_db: [f32; MATCH_BANDS],
    pub sibilance_db: f32,
    pub median_midi: f32,
    /// (root, minor, confidence) when the key detector had an answer.
    pub key: Option<(i32, bool, f32)>,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub profile: VoiceProfile,
    pub input_db: f32,
    pub bands_db: [f32; MATCH_BANDS],
    pub match_amount: f32,
    /// Index into `params::Voice`.
    pub voice: Option<usize>,
    pub key: Option<(i32, bool)>,
    pub summary: Vec<String>,
}

/// The level every style was designed for: average voiced RMS into the chain, dBFS.
pub const TARGET_LEVEL_DB: f32 = -18.0;
/// Where the styles expect the loudest sibilance to sit at that level, dBFS.
const TYPICAL_SIBILANCE_DB: f32 = -19.0;
/// Long-term spectrum of a plain, well-recorded voice in the Match bands (speech LTASS shape
/// in octave bands), dB relative to 250 Hz. The styles' own EQ adds the character on top.
const REFERENCE_DB: [f32; MATCH_BANDS] = [-2.0, 0.0, -2.0, -8.0, -13.0, -15.0, -20.0, -25.0];
/// How far each band may move, dB: (cut, boost). Lows and the top boost carefully.
const LIMITS: [(f32, f32); MATCH_BANDS] =
    [(-6.0, 3.0), (-6.0, 3.0), (-6.0, 3.0), (-4.0, 4.0), (-4.0, 4.0), (-5.0, 5.0), (-6.0, 5.0), (-6.0, 4.0)];
/// A style whose high-pass sits above this is using it as an effect: Match leaves it alone.
const HP_EFFECT_HZ: f32 = 170.0;

pub fn midi_to_hz(midi: f32) -> f32 {
    440.0 * 2f32.powf((midi - 69.0) / 12.0)
}

pub fn note_name(midi: f32) -> String {
    let m = midi.round() as i32;
    format!("{}{}", crate::dsp::tune::scale::NOTE_NAMES[m.rem_euclid(12) as usize], m.div_euclid(12) - 1)
}

pub fn analyse(m: &Measured) -> Outcome {
    let mut summary = Vec::new();

    let input_db = ((TARGET_LEVEL_DB - m.level_db).clamp(-24.0, 24.0) * 2.0).round() / 2.0;
    summary.push(format!("Input {:+.1} dB (your take averaged {:.0} dBFS)", input_db, m.level_db));

    // Sibilance as the de-esser will see it, after the input gain. With the level matched, a
    // style's threshold already catches typical esses; sharp ones need more range than it
    // allows, soft ones less de-essing (or the voice starts to lisp).
    let sib = m.sibilance_db + input_db;
    let sib_excess_db = ((sib - TYPICAL_SIBILANCE_DB).clamp(-10.0, 10.0) * 2.0).round() / 2.0;
    summary.push(if sib_excess_db >= 1.5 {
        format!("De-esser works harder: your esses are sharp ({sib_excess_db:+.1} dB)")
    } else if sib_excess_db <= -1.5 {
        format!("De-esser eases off: your esses are soft ({sib_excess_db:+.1} dB)")
    } else {
        "De-esser as designed".to_string()
    });

    let f0 = if m.median_midi > 0.0 { Some(midi_to_hz(m.median_midi)) } else { None };
    let voice = m.median_midi.gt(&0.0).then(|| match m.median_midi {
        x if x < 50.0 => 0,
        x if x < 62.0 => 1,
        _ => 2,
    });
    let hp_cap_hz = f0.map(|f| (f * 0.8).max(60.0).round()).unwrap_or(0.0);
    match (voice, f0) {
        (Some(v), Some(f)) => summary.push(format!(
            "Voice {} (you sit around {}, {:.0} Hz)",
            ["low", "mid", "high"][v],
            note_name(m.median_midi),
            f
        )),
        _ => summary.push("No steady pitch heard: voice range unchanged".to_string()),
    }

    let key = match m.key {
        Some((root, minor, conf)) if conf >= 0.12 => {
            summary.push(format!(
                "Key {} {}",
                crate::dsp::tune::scale::NOTE_NAMES[root.rem_euclid(12) as usize],
                if minor { "minor" } else { "major" }
            ));
            Some((root, minor))
        }
        _ => {
            summary.push("Key not clear from this take: set it by hand if you tune".to_string());
            None
        }
    };

    let bands_db = match_eq(&m.bands_db, f0);
    let mut moves: Vec<(usize, f32)> = bands_db.iter().copied().enumerate().filter(|(_, g)| g.abs() >= 1.0).collect();
    moves.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
    if moves.is_empty() {
        summary.push("EQ: your tone already fits".to_string());
    } else {
        let parts: Vec<String> = moves.iter().take(3).map(|(i, g)| format!("{g:+.1} dB at {}", hz_label(MATCH_HZ[*i]))).collect();
        summary.push(format!("EQ {}", parts.join(", ")));
    }

    Outcome {
        profile: VoiceProfile { learned: true, sib_excess_db, hp_cap_hz },
        input_db,
        bands_db,
        match_amount: 0.6,
        voice,
        key,
        summary,
    }
}

/// The corrective EQ: the take's spectral balance compared with the reference, judged on the
/// mids so overall level doesn't matter, and never boosting below the singer's fundamental.
fn match_eq(bands: &[f32; MATCH_BANDS], f0: Option<f32>) -> [f32; MATCH_BANDS] {
    let mid = |v: &[f32; MATCH_BANDS]| v[2..=5].iter().sum::<f32>() / 4.0;
    let (mm, rm) = (mid(bands), mid(&REFERENCE_DB));
    let mut raw = [0.0f32; MATCH_BANDS];
    for i in 0..MATCH_BANDS {
        let below_voice = f0.is_some_and(|f| MATCH_HZ[i] < f * 0.75);
        if bands[i] < -85.0 || below_voice {
            continue;
        }
        let want = (REFERENCE_DB[i] - rm) - (bands[i] - mm);
        raw[i] = want.clamp(LIMITS[i].0, LIMITS[i].1);
    }
    // A little smoothing across neighbours: EQ moves in broad strokes, not spikes.
    std::array::from_fn(|i| {
        let a = raw[i.saturating_sub(1)];
        let b = raw[(i + 1).min(MATCH_BANDS - 1)];
        let g = (0.25 * a + 0.5 * raw[i] + 0.25 * b).clamp(LIMITS[i].0, LIMITS[i].1);
        (g * 10.0).round() / 10.0
    })
}

fn hz_label(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{} kHz", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

/// A style's plain values, fitted to the voice: the de-esser follows how sharp the singer's
/// esses are and a high-pass that would thin out a deep voice comes down. `default_of` gives a
/// parameter's default for values the style leaves alone.
pub fn fit_style(style: &Style, profile: &VoiceProfile, default_of: &dyn Fn(&str) -> f32) -> Vec<(String, f32)> {
    let mut v: Vec<(String, f32)> = style.values.iter().map(|(id, x)| (id.to_string(), *x)).collect();
    fit_values(&mut v, profile, default_of);
    v
}

pub fn fit_values(v: &mut Vec<(String, f32)>, profile: &VoiceProfile, default_of: &dyn Fn(&str) -> f32) {
    if !profile.learned {
        return;
    }
    let mut set = |id: &str, f: &dyn Fn(f32) -> f32| {
        let cur = v.iter().find(|(k, _)| k == id).map(|(_, x)| *x).unwrap_or_else(|| default_of(id));
        let new = f(cur);
        match v.iter_mut().find(|(k, _)| k == id) {
            Some(slot) => slot.1 = new,
            None => v.push((id.to_string(), new)),
        }
    };
    let d = profile.sib_excess_db;
    set("ds_thr", &|t| (t - 0.3 * d).clamp(-50.0, 0.0));
    set("ds_rng", &|r| (r + 0.8 * d.max(0.0)).clamp(0.0, 20.0));
    if profile.hp_cap_hz > 0.0 {
        set("hp_f", &|f| if f <= HP_EFFECT_HZ { f.min(profile.hp_cap_hz) } else { f });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured() -> Measured {
        Measured {
            level_db: -26.0,
            // A flat-ish, dull take: plenty of 250 Hz, little top.
            bands_db: [-30.0, -27.0, -30.0, -37.0, -44.0, -49.0, -56.0, -66.0],
            sibilance_db: -31.0,
            median_midi: 47.0,
            key: Some((9, true, 0.3)),
        }
    }

    #[test]
    fn fits_level_range_key_and_tone() {
        let o = analyse(&measured());
        assert_eq!(o.input_db, 8.0);
        assert_eq!(o.voice, Some(0));
        assert_eq!(o.key, Some((9, true)));
        // Sibilance -31 + 8 = -23, 4 dB softer than the styles expect.
        assert_eq!(o.profile.sib_excess_db, -4.0);
        // Dull take: the top bands come up, nothing goes past its limit.
        assert!(o.bands_db[5] > 1.0 && o.bands_db[6] > 1.0, "{:?}", o.bands_db);
        for (g, (lo, hi)) in o.bands_db.iter().zip(LIMITS) {
            assert!((lo..=hi).contains(g));
        }
        assert_eq!(o.summary.len(), 5);
    }

    #[test]
    fn leaves_a_reference_voice_alone() {
        let mut m = measured();
        m.bands_db = REFERENCE_DB.map(|r| r - 30.0);
        let o = analyse(&m);
        assert!(o.bands_db.iter().all(|g| g.abs() < 0.05), "{:?}", o.bands_db);
    }

    #[test]
    fn fitting_moves_only_the_voice_dependent_values() {
        let style = crate::styles::find("Playboi Carti").unwrap();
        let p = VoiceProfile { learned: true, sib_excess_db: 5.0, hp_cap_hz: 90.0 };
        let v = fit_style(style, &p, &|_| 8.0);
        let get = |v: &Vec<(String, f32)>, id: &str| v.iter().find(|(k, _)| k == id).unwrap().1;
        assert_eq!(get(&v, "hp_f"), 90.0);
        assert_eq!(get(&v, "ds_thr"), -25.5);
        assert_eq!(get(&v, "ds_rng"), 12.0);
        assert_eq!(get(&v, "fet_rat"), 10.0);
        let soft = VoiceProfile { sib_excess_db: -5.0, ..p };
        let phone = crate::styles::find("Telephone Hook").unwrap();
        let v = fit_style(phone, &soft, &|id| if id == "ds_thr" { -24.0 } else { 8.0 });
        assert_eq!(get(&v, "hp_f"), 200.0);
        assert_eq!(get(&v, "ds_thr"), -22.5);
        assert_eq!(get(&v, "ds_rng"), 8.0);
    }
}
