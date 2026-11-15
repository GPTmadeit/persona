//! Factory presets for Persona Master: parameter id -> plain value over the defaults. Starting
//! points for a mix that already sounds right; Auto Master fits the chain to the actual song.
//!
//! Enum values are indices: exc_mode 0 Tube 1 Tape 2 Crisp; eq_type 0 Bell 1 Low shelf
//! 2 High shelf 3 Low cut 4 High cut; eq_ch 0 Stereo 1 Mid 2 Side.

pub struct Preset {
    pub name: &'static str,
    pub blurb: &'static str,
    pub values: &'static [(&'static str, f32)],
}

/// Parameters that belong to the session, not the sound: loading a preset keeps them.
pub const KEEP_ON_PRESET: &[&str] = &["bypass", "m_in", "genre", "tgt", "dith", "dith_shape"];

macro_rules! v {
    ($($id:literal = $val:expr),* $(,)?) => { &[$(($id, $val as f32)),*] };
}

pub const PRESETS: &[Preset] = &[
    Preset { name: "Clean Master", blurb: "Light glue and a true-peak ceiling. Nothing added: the place to start.", values: v! {} },
    Preset {
        name: "Streaming Ready",
        blurb: "Gentle and open, for a -14 LUFS delivery with room to breathe.",
        values: v! { "glue_thr" = -14, "glue_rat" = 1.5, "max_gain" = 3, "max_rel" = 100, "max_char" = 0.3, "img_w_4" = 1.1 },
    },
    Preset {
        name: "Loud & Punchy",
        blurb: "Clips the drum peaks, then limits: loud without losing the hit.",
        values: v! { "glue_thr" = -14, "glue_att" = 30, "glue_mk" = 1, "mb_on" = 1, "mb_thr_1" = -14, "mb_rat_1" = 2.5, "max_gain" = 7, "max_clip" = 0.5, "max_rel" = 50, "max_char" = 0.6 },
    },
    Preset {
        name: "Trap Banger",
        blurb: "Tight, mono 808s, bright hats, pushed hard into the clipper.",
        values: v! {
            "eq_on_1" = 1, "eq_f_1" = 28, "eq_g_2" = 1.5, "eq_f_2" = 60, "eq_g_6" = 1.5,
            "glue_thr" = -12, "glue_att" = 30, "mb_on" = 1, "mb_thr_1" = -12, "mb_rat_1" = 3, "mb_gain_1" = 1,
            "img_mono" = 1, "img_w_3" = 1.1, "img_w_4" = 1.25, "max_gain" = 9, "max_clip" = 0.65, "max_rel" = 40, "max_char" = 0.6,
        },
    },
    Preset {
        name: "Warm Tape",
        blurb: "Tape harmonics low down, a softer top and slow glue.",
        values: v! {
            "eq_g_2" = 1, "eq_g_6" = -1, "glue_thr" = -16, "glue_att" = 30, "glue_auto" = 1, "glue_mk" = 1,
            "exc_on" = 1, "exc_mode" = 1, "exc_amt_1" = 0.2, "exc_amt_2" = 0.25, "exc_amt_3" = 0.1, "max_gain" = 4, "max_rel" = 120, "max_char" = 0.4,
        },
    },
    Preset {
        name: "Wide & Bright",
        blurb: "Opens the sides and the air band. For dark, narrow mixes.",
        values: v! {
            "eq_g_5" = 1, "eq_g_6" = 2.5, "eq_ch_6" = 2, "exc_on" = 1, "exc_mode" = 2, "exc_amt_4" = 0.2,
            "img_w_2" = 1.05, "img_w_3" = 1.2, "img_w_4" = 1.4, "img_mono" = 1, "max_gain" = 4,
        },
    },
    Preset {
        name: "R&B Silk",
        blurb: "Smooth low end, soft tube warmth and a relaxed limiter.",
        values: v! {
            "eq_g_2" = 1, "eq_g_3" = -1, "eq_g_6" = 1.5, "glue_thr" = -15, "glue_rat" = 1.5, "exc_on" = 1, "exc_mode" = 0, "exc_amt_2" = 0.15, "exc_amt_3" = 0.1,
            "img_w_4" = 1.2, "max_gain" = 5, "max_rel" = 90, "max_char" = 0.35, "max_clip" = 0.2,
        },
    },
    Preset {
        name: "Lo-Fi Glue",
        blurb: "Heavy glue, rolled-off top, tape on everything.",
        values: v! {
            "eq_on_1" = 1, "eq_f_1" = 40, "eq_type_6" = 4, "eq_f_6" = 9000, "glue_thr" = -20, "glue_rat" = 3, "glue_mk" = 2.5, "glue_mix" = 0.8,
            "exc_on" = 1, "exc_mode" = 1, "exc_amt_1" = 0.2, "exc_amt_2" = 0.3, "exc_amt_3" = 0.3, "max_gain" = 4, "max_rel" = 120,
        },
    },
    Preset {
        name: "Podcast Voice",
        blurb: "Even, clear speech: rumble out, firm levelling, mono-safe.",
        values: v! {
            "eq_on_1" = 1, "eq_f_1" = 70, "eq_g_3" = -1.5, "eq_g_5" = 2, "glue_thr" = -20, "glue_rat" = 3, "glue_att" = 10, "glue_mk" = 3, "glue_hp" = 60,
            "mb_on" = 1, "mb_thr_2" = -20, "mb_thr_3" = -24, "img_on" = 0, "max_gain" = 6, "max_rel" = 120, "max_char" = 0.4,
        },
    },
    Preset {
        name: "Just Limit",
        blurb: "Only the true-peak maximizer. For a mix that needs level and nothing else.",
        values: v! { "eq_mod" = 0, "glue_on" = 0, "img_on" = 0, "max_gain" = 4 },
    },
];

pub fn find(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}
