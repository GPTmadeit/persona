//! Auto Master, the part that decides: turns what the audio thread measured about the mix
//! into settings for the Balance EQ, the compressors, the imager and the maximizer.
//!
//! The genre targets are our reading of how finished records in each style are balanced, as
//! long-term levels in the eight Balance bands relative to pink noise. They are starting
//! points: every setting Auto Master makes can be changed afterwards.

use std::sync::atomic::Ordering;

use super::engine::AUTO_XOVER;
use super::params::{Genre, BAL_BANDS, BAL_HZ, MB_BANDS};
use super::shared::MasterShared;

/// What the listener measured; see `shared::Heard`.
#[derive(Clone, Debug)]
pub struct Heard {
    pub lufs: f32,
    pub true_peak_db: f32,
    pub range: f32,
    pub bands_db: [f32; BAL_BANDS],
    pub mb_db: [f32; MB_BANDS],
    pub glue_db: f32,
    pub side_ratio: f32,
    pub low_side_ratio: f32,
}

impl Heard {
    pub fn read(sh: &MasterShared) -> Self {
        let h = &sh.heard;
        let ld = |a: &nih_plug::prelude::AtomicF32| a.load(Ordering::Relaxed);
        Self {
            lufs: ld(&h.lufs),
            true_peak_db: ld(&h.true_peak_db),
            range: ld(&h.range),
            bands_db: std::array::from_fn(|i| ld(&h.bands_db[i])),
            mb_db: std::array::from_fn(|i| ld(&h.mb_db[i])),
            glue_db: ld(&h.glue_db),
            side_ratio: ld(&h.side_ratio),
            low_side_ratio: ld(&h.low_side_ratio),
        }
    }
}

/// How a genre is mastered.
pub struct Profile {
    /// Tonal balance in the Balance bands, dB relative to pink noise.
    pub curve: [f32; BAL_BANDS],
    /// Gain reduction on the loud moments: (Glue dB, Glue ratio), then each band.
    pub glue: (f32, f32),
    pub bands: [f32; MB_BANDS],
    /// How wide the top two bands may be opened on a narrow mix.
    pub width: [f32; 2],
    pub clip: f32,
    pub character: f32,
    pub release_ms: f32,
}

pub fn profile(g: Genre) -> Profile {
    let p = |curve, glue, bands, width, clip, character, release_ms| Profile { curve, glue, bands, width, clip, character, release_ms };
    match g {
        Genre::HipHop => p([5.0, 2.5, 0.0, -0.5, -1.5, -3.5, -6.0, -10.0], (1.5, 2.0), [1.5, 1.0, 1.0, 1.0], [1.1, 1.2], 0.4, 0.5, 60.0),
        Genre::Trap => p([6.5, 2.5, -0.5, -1.0, -1.5, -3.0, -5.5, -9.0], (1.0, 2.0), [2.0, 1.0, 1.0, 1.0], [1.1, 1.2], 0.6, 0.6, 40.0),
        Genre::RnB => p([4.0, 2.0, 0.0, -0.5, -1.5, -3.5, -6.0, -9.5], (1.5, 2.0), [1.0, 1.0, 1.0, 1.0], [1.15, 1.25], 0.2, 0.4, 80.0),
        Genre::Pop => p([3.0, 1.5, 0.0, 0.0, -1.0, -3.0, -5.5, -9.0], (1.5, 2.0), [1.0, 1.0, 1.0, 1.0], [1.1, 1.2], 0.25, 0.5, 70.0),
        Genre::Edm => p([5.5, 2.5, -0.5, -1.0, -1.5, -3.0, -5.0, -8.5], (1.0, 2.0), [1.5, 1.0, 1.0, 1.5], [1.15, 1.25], 0.5, 0.6, 40.0),
        Genre::Rock => p([2.0, 1.5, 0.5, 0.0, -0.5, -2.5, -6.0, -10.5], (2.0, 2.0), [1.0, 1.0, 1.0, 1.0], [1.05, 1.1], 0.2, 0.5, 90.0),
        Genre::Acoustic => p([0.5, 1.0, 0.5, 0.0, -1.0, -3.5, -7.0, -11.5], (0.8, 1.5), [0.0; 4], [1.0, 1.0], 0.0, 0.3, 150.0),
        Genre::LoFi => p([4.0, 2.5, 0.5, 0.0, -2.0, -5.0, -9.0, -15.0], (2.5, 2.5), [1.5, 1.0, 1.0, 0.5], [1.0, 1.0], 0.3, 0.5, 100.0),
        Genre::Podcast => p([-6.0, 1.0, 1.0, 0.0, -2.0, -5.0, -10.0, -16.0], (3.0, 3.0), [0.0, 1.5, 1.5, 1.0], [1.0, 1.0], 0.0, 0.4, 120.0),
        Genre::Cinematic => p([3.0, 2.0, 0.5, 0.0, -1.5, -4.0, -7.5, -12.0], (0.8, 1.5), [0.0; 4], [1.1, 1.15], 0.0, 0.3, 200.0),
    }
}

/// The genre's target at any frequency, dB relative to pink noise, for drawing.
pub fn curve_at(g: Genre, hz: f32) -> f32 {
    let c = profile(g).curve;
    let x = hz.max(1.0).ln();
    if x <= BAL_HZ[0].ln() {
        return c[0];
    }
    for i in 1..BAL_BANDS {
        let (a, b) = (BAL_HZ[i - 1].ln(), BAL_HZ[i].ln());
        if x <= b {
            return c[i - 1] + (c[i] - c[i - 1]) * (x - a) / (b - a);
        }
    }
    c[BAL_BANDS - 1]
}

pub struct Outcome {
    /// Parameter id and plain value.
    pub values: Vec<(String, f32)>,
    pub report: Vec<String>,
}

/// How far each Balance band may move, dB: (cut, boost). The sub and the very top boost
/// carefully.
const LIMITS: [(f32, f32); BAL_BANDS] = [(-5.0, 3.0), (-5.0, 4.0), (-5.0, 4.0), (-4.0, 4.0), (-4.0, 4.0), (-5.0, 5.0), (-5.0, 5.0), (-5.0, 4.0)];

fn round(v: f32, step: f32) -> f32 {
    (v / step).round() * step
}

pub fn decide(h: &Heard, genre: Genre, target: f32) -> Outcome {
    let p = profile(genre);
    let mut v: Vec<(String, f32)> = Vec::new();
    let mut report = Vec::new();
    let mut set = |id: &str, x: f32| v.push((id.to_string(), x));
    report.push(format!("Your mix: {:.1} LUFS, peaks at {:.1} dBTP, {:.0} LU of range", h.lufs, h.true_peak_db, h.range));

    // ---- tonal balance, judged on the mids so level doesn't matter
    let mid = |b: &[f32; BAL_BANDS]| b[2..=5].iter().sum::<f32>() / 4.0;
    let (mm, tm) = (mid(&h.bands_db), mid(&p.curve));
    let raw: [f32; BAL_BANDS] = std::array::from_fn(|i| {
        if h.bands_db[i] < -90.0 {
            0.0
        } else {
            (((p.curve[i] - tm) - (h.bands_db[i] - mm)) * 0.7).clamp(LIMITS[i].0, LIMITS[i].1)
        }
    });
    let bal: [f32; BAL_BANDS] = std::array::from_fn(|i| {
        let (a, b) = (raw[i.saturating_sub(1)], raw[(i + 1).min(BAL_BANDS - 1)]);
        round((0.25 * a + 0.5 * raw[i] + 0.25 * b).clamp(LIMITS[i].0, LIMITS[i].1), 0.1)
    });
    set("bal_amt", 1.0);
    for (i, g) in bal.iter().enumerate() {
        set(&format!("bal_{i}"), *g);
    }
    let mut moves: Vec<(usize, f32)> = bal.iter().copied().enumerate().filter(|(_, g)| g.abs() >= 0.8).collect();
    moves.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
    report.push(if moves.is_empty() {
        format!("Balance: already sits like {}", genre_name(genre))
    } else {
        let parts: Vec<String> = moves.iter().take(3).map(|(i, g)| format!("{g:+.1} dB at {}", super::hz_label(BAL_HZ[*i]))).collect();
        format!("Balance: {}", parts.join(", "))
    });

    // ---- glue: a threshold that takes the wanted reduction off the loud moments
    let over = |gr: f32, ratio: f32| gr / (1.0 - 1.0 / ratio.max(1.01));
    let (glue_gr, glue_ratio) = p.glue;
    // A mix that is already squashed gets less.
    let dense = h.true_peak_db - h.lufs < 9.0;
    let scale = if dense { 0.5 } else { 1.0 };
    set("glue_on", 1.0);
    set("glue_rat", glue_ratio);
    set("glue_thr", round((h.glue_db - over(glue_gr * scale, glue_ratio)).clamp(-40.0, 0.0), 0.5));
    set("glue_att", 30.0);
    set("glue_auto", 1.0);
    set("glue_hp", 90.0);
    set("glue_mix", 1.0);
    set("glue_mk", round(glue_gr * scale * 0.5, 0.1));

    // ---- multiband: the same idea per band, following what the Balance EQ did to each
    let band_shift = [(bal[0] + bal[1]) * 0.5, (bal[2] + bal[3]) * 0.5, (bal[4] + bal[5]) * 0.5, (bal[6] + bal[7]) * 0.5];
    let mb = p.bands.iter().any(|g| *g > 0.0);
    set("mb_on", if mb { 1.0 } else { 0.0 });
    set("x_lo", AUTO_XOVER[0]);
    set("x_mid", AUTO_XOVER[1]);
    set("x_hi", AUTO_XOVER[2]);
    for b in 0..MB_BANDS {
        let n = b + 1;
        let gr = p.bands[b] * scale;
        let ratio = if b == 0 { 2.5 } else { 2.0 };
        let thr = if gr > 0.0 { h.mb_db[b] + band_shift[b] - over(gr, ratio) } else { 0.0 };
        set(&format!("mb_thr_{n}"), round(thr.clamp(-50.0, 0.0), 0.5));
        set(&format!("mb_rat_{n}"), ratio);
        set(&format!("mb_att_{n}"), [30.0, 15.0, 8.0, 3.0][b]);
        set(&format!("mb_rel_{n}"), [250.0, 180.0, 120.0, 80.0][b]);
        set(&format!("mb_gain_{n}"), round(gr * 0.4, 0.1));
    }
    report.push(match (mb, dense) {
        (true, false) => format!("Dynamics: about {glue_gr:.1} dB of glue and gentle multiband on the loud parts"),
        (true, true) => "Dynamics: your mix is already dense, so only a touch of compression".to_string(),
        (false, _) => format!("Dynamics: {:.1} dB of glue, no multiband for {}", glue_gr * scale, genre_name(genre)),
    });

    // ---- stereo: open the top of a narrow mix, keep the bass in the middle
    let room = ((0.5 - h.side_ratio) / 0.25).clamp(0.0, 1.0);
    let w = [1.0, 1.0, round(1.0 + (p.width[0] - 1.0) * room, 0.01), round(1.0 + (p.width[1] - 1.0) * room, 0.01)];
    let mono = h.low_side_ratio > 0.2;
    set("img_on", 1.0);
    for (b, x) in w.iter().enumerate() {
        set(&format!("img_w_{}", b + 1), *x);
    }
    set("img_mono", if mono { 1.0 } else { 0.0 });
    set("img_mono_f", 110.0);
    report.push(match (w[3] > 1.02, mono) {
        (true, true) => format!("Stereo: highs opened to {:.0}%, bass below 110 Hz kept mono", w[3] * 100.0),
        (true, false) => format!("Stereo: highs opened to {:.0}%", w[3] * 100.0),
        (false, true) => "Stereo: width left alone, bass below 110 Hz kept mono".to_string(),
        (false, false) => "Stereo: already wide enough, left alone".to_string(),
    });
    set("exc_on", 0.0);

    // ---- loudness
    let gain = round((target - h.lufs + 1.0).clamp(0.0, 24.0), 0.1);
    set("max_on", 1.0);
    set("max_gain", gain);
    set("max_ceil", -1.0);
    set("max_tp", 1.0);
    set("max_rel", p.release_ms);
    set("max_char", p.character);
    // Clipping only earns its keep when the limiter has real work to do.
    set("max_clip", if gain > 3.0 { p.clip } else { 0.0 });
    report.push(if target - h.lufs < -0.5 {
        format!("Loudness: the mix is already louder than {target:.0} LUFS. Turn Input down or raise the target")
    } else {
        format!("Loudness: +{gain:.1} dB into the maximizer for {target:.0} LUFS, ceiling -1 dBTP")
    });

    Outcome { values: v, report }
}

/// After the refine pass: the maximizer gain that closes the gap to the target, and a line
/// for the report.
pub fn refine(out_lufs: f32, target: f32, gain_db: f32) -> (f32, String) {
    if out_lufs < -90.0 {
        return (gain_db, "Loudness: nothing to measure".to_string());
    }
    let new = round((gain_db + (target - out_lufs)).clamp(0.0, 24.0), 0.1);
    (new, format!("Fine-tuned: the master read {out_lufs:.1} LUFS, maximizer now +{new:.1} dB"))
}

pub fn genre_name(g: Genre) -> &'static str {
    match g {
        Genre::HipHop => "hip-hop",
        Genre::Trap => "trap",
        Genre::RnB => "R&B",
        Genre::Pop => "pop",
        Genre::Edm => "EDM",
        Genre::Rock => "rock",
        Genre::Acoustic => "acoustic music",
        Genre::LoFi => "lo-fi",
        Genre::Podcast => "spoken word",
        Genre::Cinematic => "cinematic music",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard() -> Heard {
        let c = profile(Genre::HipHop).curve;
        Heard { lufs: -18.0, true_peak_db: -3.0, range: 6.0, bands_db: c.map(|v| v - 30.0), mb_db: [-12.0, -16.0, -20.0, -26.0], glue_db: -10.0, side_ratio: 0.2, low_side_ratio: 0.05 }
    }

    #[test]
    fn a_mix_on_target_is_left_flat() {
        let o = decide(&heard(), Genre::HipHop, -10.0);
        let get = |id: &str| o.values.iter().find(|(k, _)| k == id).unwrap().1;
        for i in 0..BAL_BANDS {
            assert!(get(&format!("bal_{i}")).abs() < 0.05);
        }
        assert_eq!(get("max_gain"), 9.0);
        assert_eq!(get("img_mono"), 0.0);
        assert!(get("img_w_4") > 1.1);
        assert_eq!(o.report.len(), 5);
    }

    #[test]
    fn a_dull_mix_gets_its_top_back_within_limits() {
        let mut h = heard();
        for i in 5..8 {
            h.bands_db[i] -= 8.0;
        }
        let o = decide(&h, Genre::HipHop, -10.0);
        let get = |id: &str| o.values.iter().find(|(k, _)| k == id).unwrap().1;
        assert!(get("bal_6") > 2.0);
        for i in 0..BAL_BANDS {
            let g = get(&format!("bal_{i}"));
            assert!((LIMITS[i].0..=LIMITS[i].1).contains(&g));
        }
    }

    #[test]
    fn curve_passes_through_its_points() {
        let c = profile(Genre::Pop).curve;
        for i in 0..BAL_BANDS {
            assert!((curve_at(Genre::Pop, BAL_HZ[i]) - c[i]).abs() < 1e-3);
        }
    }
}
