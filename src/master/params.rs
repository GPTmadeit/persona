//! Persona Master's parameters. The ids are stable (presets and saved projects use them), so
//! never rename one.

use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use parking_lot::RwLock;
use std::sync::Arc;

use crate::dsp::master_fx::{DitherBits, ExciteMode};
use crate::params::{db, hz, ms, pct};

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum EqType {
    Bell,
    #[name = "Low Shelf"]
    LowShelf,
    #[name = "High Shelf"]
    HighShelf,
    #[name = "Low Cut"]
    LowCut,
    #[name = "High Cut"]
    HighCut,
}

/// Which part of the stereo image an EQ band works on.
#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Channel {
    Stereo,
    Mid,
    Side,
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Excite {
    Tube,
    Tape,
    Crisp,
}

impl Excite {
    pub fn mode(self) -> ExciteMode {
        match self {
            Excite::Tube => ExciteMode::Tube,
            Excite::Tape => ExciteMode::Tape,
            Excite::Crisp => ExciteMode::Crisp,
        }
    }
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Dither {
    Off,
    #[name = "16-bit"]
    Bits16,
    #[name = "24-bit"]
    Bits24,
}

impl Dither {
    pub fn bits(self) -> DitherBits {
        match self {
            Dither::Off => DitherBits::Off,
            Dither::Bits16 => DitherBits::Bits16,
            Dither::Bits24 => DitherBits::Bits24,
        }
    }
}

/// What Auto Master aims the tonal balance and dynamics at.
#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Genre {
    #[name = "Hip-Hop"]
    HipHop,
    Trap,
    #[name = "R&B"]
    RnB,
    Pop,
    #[name = "EDM"]
    Edm,
    Rock,
    Acoustic,
    #[name = "Lo-Fi"]
    LoFi,
    #[name = "Podcast / Voice"]
    Podcast,
    Cinematic,
}

pub const EQ_BANDS: usize = 6;
pub const BAL_BANDS: usize = 8;
/// Centres of the Balance EQ (and of Auto Master's measuring bands), Hz.
pub const BAL_HZ: [f32; BAL_BANDS] = [60.0, 150.0, 350.0, 800.0, 1800.0, 4000.0, 8000.0, 14000.0];
pub const MB_BANDS: usize = 4;
pub const MB_NAMES: [&str; MB_BANDS] = ["Low", "Low-mid", "High-mid", "High"];

#[derive(Params)]
pub struct EqBand {
    #[id = "eq_on"]
    pub on: BoolParam,
    #[id = "eq_type"]
    pub kind: EnumParam<EqType>,
    #[id = "eq_f"]
    pub freq: FloatParam,
    #[id = "eq_g"]
    pub gain: FloatParam,
    #[id = "eq_q"]
    pub q: FloatParam,
    #[id = "eq_ch"]
    pub channel: EnumParam<Channel>,
}

impl EqBand {
    fn new(i: usize, on: bool, kind: EqType, freq: f32) -> Self {
        let n = i + 1;
        Self {
            on: BoolParam::new(format!("EQ {n}"), on),
            kind: EnumParam::new(format!("EQ {n} type"), kind),
            freq: hz(&format!("EQ {n} freq"), freq, 20.0, 20000.0),
            gain: db(&format!("EQ {n} gain"), 0.0, -12.0, 12.0),
            q: FloatParam::new(format!("EQ {n} Q"), 0.8, FloatRange::Skewed { min: 0.2, max: 8.0, factor: FloatRange::skew_factor(-1.5) })
                .with_value_to_string(formatters::v2s_f32_rounded(2)),
            channel: EnumParam::new(format!("EQ {n} channel"), Channel::Stereo),
        }
    }
}

#[derive(Params)]
pub struct MbBand {
    #[id = "mb_thr"]
    pub thresh: FloatParam,
    #[id = "mb_rat"]
    pub ratio: FloatParam,
    #[id = "mb_att"]
    pub attack: FloatParam,
    #[id = "mb_rel"]
    pub release: FloatParam,
    #[id = "mb_gain"]
    pub gain: FloatParam,
    #[id = "exc_amt"]
    pub excite: FloatParam,
    #[id = "img_w"]
    pub width: FloatParam,
}

impl MbBand {
    fn new(i: usize, attack: f32, release: f32) -> Self {
        let n = MB_NAMES[i];
        Self {
            thresh: db(&format!("{n} threshold"), -18.0, -50.0, 0.0),
            ratio: ratio(&format!("{n} ratio"), 2.0),
            attack: ms(&format!("{n} attack"), attack, 0.5, 100.0),
            release: ms(&format!("{n} release"), release, 20.0, 1000.0),
            gain: db(&format!("{n} gain"), 0.0, -12.0, 12.0),
            excite: pct(&format!("{n} excite"), 0.0),
            width: FloatParam::new(format!("{n} width"), 1.0, FloatRange::Linear { min: 0.0, max: 2.0 })
                .with_unit("%")
                .with_value_to_string(Arc::new(|v| format!("{:.0}", v * 100.0)))
                .with_string_to_value(Arc::new(|s| s.trim_end_matches('%').trim().parse::<f32>().ok().map(|v| v / 100.0)))
                .with_smoother(SmoothingStyle::Linear(30.0)),
        }
    }
}

fn ratio(name: &str, default: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Skewed { min: 1.0, max: 10.0, factor: FloatRange::skew_factor(-1.2) })
        .with_unit(":1")
        .with_value_to_string(formatters::v2s_f32_rounded(1))
}

#[derive(Params)]
pub struct MasterParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,
    /// The preset last loaded, shown in the header.
    #[persist = "preset"]
    pub preset: Arc<RwLock<String>>,
    /// Auto Master's last report, so it is still there when the project reopens.
    #[persist = "report"]
    pub report: Arc<RwLock<Vec<String>>>,

    #[id = "bypass"]
    pub bypass: BoolParam,
    #[id = "m_in"]
    pub input: FloatParam,
    #[id = "m_out"]
    pub output: FloatParam,
    #[id = "genre"]
    pub genre: EnumParam<Genre>,
    #[id = "tgt"]
    pub target: FloatParam,

    #[id = "eq_mod"]
    pub eq_on: BoolParam,
    #[nested(array, group = "EQ")]
    pub eq: [EqBand; EQ_BANDS],

    #[id = "bal_amt"]
    pub bal_amount: FloatParam,
    #[id = "bal_0"]
    pub bal0: FloatParam,
    #[id = "bal_1"]
    pub bal1: FloatParam,
    #[id = "bal_2"]
    pub bal2: FloatParam,
    #[id = "bal_3"]
    pub bal3: FloatParam,
    #[id = "bal_4"]
    pub bal4: FloatParam,
    #[id = "bal_5"]
    pub bal5: FloatParam,
    #[id = "bal_6"]
    pub bal6: FloatParam,
    #[id = "bal_7"]
    pub bal7: FloatParam,

    #[id = "glue_on"]
    pub glue_on: BoolParam,
    #[id = "glue_thr"]
    pub glue_thresh: FloatParam,
    #[id = "glue_rat"]
    pub glue_ratio: FloatParam,
    #[id = "glue_att"]
    pub glue_attack: FloatParam,
    #[id = "glue_rel"]
    pub glue_release: FloatParam,
    #[id = "glue_auto"]
    pub glue_auto: BoolParam,
    #[id = "glue_mk"]
    pub glue_makeup: FloatParam,
    #[id = "glue_hp"]
    pub glue_hp: FloatParam,
    #[id = "glue_mix"]
    pub glue_mix: FloatParam,

    #[id = "x_lo"]
    pub x_lo: FloatParam,
    #[id = "x_mid"]
    pub x_mid: FloatParam,
    #[id = "x_hi"]
    pub x_hi: FloatParam,
    #[id = "mb_on"]
    pub mb_on: BoolParam,
    #[id = "exc_on"]
    pub exc_on: BoolParam,
    #[id = "exc_mode"]
    pub exc_mode: EnumParam<Excite>,
    #[id = "img_on"]
    pub img_on: BoolParam,
    #[id = "img_mono"]
    pub mono_bass: BoolParam,
    #[id = "img_mono_f"]
    pub mono_freq: FloatParam,
    #[nested(array, group = "Band")]
    pub band: [MbBand; MB_BANDS],

    #[id = "max_on"]
    pub max_on: BoolParam,
    #[id = "max_gain"]
    pub max_gain: FloatParam,
    #[id = "max_ceil"]
    pub ceiling: FloatParam,
    #[id = "max_rel"]
    pub max_release: FloatParam,
    #[id = "max_char"]
    pub max_character: FloatParam,
    #[id = "max_tp"]
    pub true_peak: BoolParam,
    #[id = "max_clip"]
    pub clip: FloatParam,

    #[id = "dith"]
    pub dither: EnumParam<Dither>,
    #[id = "dith_shape"]
    pub dither_shape: BoolParam,
}

impl MasterParams {
    pub fn bal(&self) -> [&FloatParam; BAL_BANDS] {
        [&self.bal0, &self.bal1, &self.bal2, &self.bal3, &self.bal4, &self.bal5, &self.bal6, &self.bal7]
    }
}

impl Default for MasterParams {
    fn default() -> Self {
        let bal = |i: usize| db(&format!("Balance {}", crate::master::hz_label(BAL_HZ[i])), 0.0, -9.0, 9.0);
        Self {
            editor_state: EguiState::from_size(crate::master::editor::WIDTH, crate::master::editor::HEIGHT),
            preset: Arc::new(RwLock::new(String::from("Clean Master"))),
            report: Arc::new(RwLock::new(Vec::new())),

            bypass: BoolParam::new("Bypass", false).make_bypass(),
            input: db("Input", 0.0, -24.0, 24.0),
            output: db("Output", 0.0, -24.0, 0.0),
            genre: EnumParam::new("Target genre", Genre::HipHop),
            target: FloatParam::new("Target loudness", -10.0, FloatRange::Linear { min: -24.0, max: -6.0 })
                .with_unit(" LUFS")
                .with_step_size(0.5)
                .with_value_to_string(formatters::v2s_f32_rounded(1)),

            eq_on: BoolParam::new("EQ", true),
            eq: [
                EqBand::new(0, false, EqType::LowCut, 25.0),
                EqBand::new(1, true, EqType::LowShelf, 90.0),
                EqBand::new(2, true, EqType::Bell, 300.0),
                EqBand::new(3, true, EqType::Bell, 1500.0),
                EqBand::new(4, true, EqType::Bell, 4500.0),
                EqBand::new(5, true, EqType::HighShelf, 12000.0),
            ],

            bal_amount: pct("Balance amount", 1.0),
            bal0: bal(0),
            bal1: bal(1),
            bal2: bal(2),
            bal3: bal(3),
            bal4: bal(4),
            bal5: bal(5),
            bal6: bal(6),
            bal7: bal(7),

            glue_on: BoolParam::new("Glue", true),
            glue_thresh: db("Glue threshold", -12.0, -40.0, 0.0),
            glue_ratio: ratio("Glue ratio", 2.0),
            glue_attack: ms("Glue attack", 10.0, 0.1, 30.0),
            glue_release: ms("Glue release", 200.0, 50.0, 1200.0),
            glue_auto: BoolParam::new("Glue auto release", true),
            glue_makeup: db("Glue makeup", 0.0, 0.0, 12.0),
            glue_hp: hz("Glue side-chain HP", 90.0, 20.0, 300.0),
            glue_mix: pct("Glue mix", 1.0),

            x_lo: hz("Crossover low", 120.0, 40.0, 500.0),
            x_mid: hz("Crossover mid", 1000.0, 300.0, 4000.0),
            x_hi: hz("Crossover high", 6000.0, 2000.0, 14000.0),
            mb_on: BoolParam::new("Multiband", false),
            exc_on: BoolParam::new("Exciter", false),
            exc_mode: EnumParam::new("Exciter type", Excite::Tape),
            img_on: BoolParam::new("Imager", true),
            mono_bass: BoolParam::new("Mono bass", false),
            mono_freq: hz("Mono bass below", 110.0, 40.0, 300.0),
            band: [MbBand::new(0, 30.0, 250.0), MbBand::new(1, 15.0, 180.0), MbBand::new(2, 8.0, 120.0), MbBand::new(3, 3.0, 80.0)],

            max_on: BoolParam::new("Maximizer", true),
            max_gain: db("Maximizer gain", 0.0, 0.0, 24.0),
            ceiling: db("Ceiling", -1.0, -6.0, 0.0),
            max_release: ms("Maximizer release", 60.0, 5.0, 500.0),
            max_character: pct("Maximizer character", 0.5),
            true_peak: BoolParam::new("True peak", true),
            clip: pct("Soft clip", 0.0),

            dither: EnumParam::new("Dither", Dither::Off),
            dither_shape: BoolParam::new("Noise shaping", true),
        }
    }
}
