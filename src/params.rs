//! Every parameter, grouped per module. The string ids are stable: styles, user presets and
//! saved projects refer to them, so never rename one.

use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use parking_lot::RwLock;
use std::sync::Arc;

use crate::dsp::saturation::SatMode;
use crate::dsp::space::ReverbType;
use crate::dsp::tune::detector::VoiceRange;

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Key {
    C,
    #[name = "C#"]
    Cs,
    D,
    #[name = "D#"]
    Ds,
    E,
    F,
    #[name = "F#"]
    Fs,
    G,
    #[name = "G#"]
    Gs,
    A,
    #[name = "A#"]
    As,
    B,
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Scale {
    Chromatic,
    Major,
    Minor,
    #[name = "Harmonic Minor"]
    HarmonicMinor,
    Dorian,
    Phrygian,
    Mixolydian,
    #[name = "Major Pentatonic"]
    MajorPentatonic,
    #[name = "Minor Pentatonic"]
    MinorPentatonic,
    Blues,
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Voice {
    #[name = "Low (deep male)"]
    Low,
    #[name = "Mid"]
    Mid,
    #[name = "High (female / falsetto)"]
    High,
    #[name = "Wide (any)"]
    Wide,
}

impl Voice {
    pub fn range(self) -> VoiceRange {
        match self {
            Voice::Low => VoiceRange::Low,
            Voice::Mid => VoiceRange::Mid,
            Voice::High => VoiceRange::High,
            Voice::Wide => VoiceRange::Wide,
        }
    }
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Saturation {
    Tape,
    Tube,
    Fuzz,
    Phone,
    #[name = "Lo-Fi"]
    LoFi,
}

impl Saturation {
    pub fn mode(self) -> SatMode {
        match self {
            Saturation::Tape => SatMode::Tape,
            Saturation::Tube => SatMode::Tube,
            Saturation::Fuzz => SatMode::Fuzz,
            Saturation::Phone => SatMode::Phone,
            Saturation::LoFi => SatMode::LoFi,
        }
    }
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Division {
    #[name = "1/16"]
    D16,
    #[name = "1/8 T"]
    D8T,
    #[name = "1/8"]
    D8,
    #[name = "1/8 D"]
    D8D,
    #[name = "1/4 T"]
    D4T,
    #[name = "1/4"]
    D4,
    #[name = "1/4 D"]
    D4D,
    #[name = "1/2"]
    D2,
    #[name = "1/2 D"]
    D2D,
    #[name = "1 bar"]
    D1,
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Room {
    Plate,
    Room,
    Hall,
    Chamber,
}

impl Room {
    pub fn kind(self) -> ReverbType {
        match self {
            Room::Plate => ReverbType::Plate,
            Room::Room => ReverbType::Room,
            Room::Hall => ReverbType::Hall,
            Room::Chamber => ReverbType::Chamber,
        }
    }
}

fn db(name: &str, default: f32, min: f32, max: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Linear { min, max })
        .with_unit(" dB")
        .with_step_size(0.1)
        .with_value_to_string(formatters::v2s_f32_rounded(1))
        .with_smoother(SmoothingStyle::Linear(30.0))
}

fn hz(name: &str, default: f32, min: f32, max: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Skewed { min, max, factor: FloatRange::skew_factor(-1.6) })
        .with_value_to_string(formatters::v2s_f32_hz_then_khz(1))
        .with_string_to_value(formatters::s2v_f32_hz_then_khz())
}

fn pct(name: &str, default: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Linear { min: 0.0, max: 1.0 })
        .with_unit("%")
        .with_value_to_string(formatters::v2s_f32_percentage(0))
        .with_string_to_value(formatters::s2v_f32_percentage())
        .with_smoother(SmoothingStyle::Linear(30.0))
}

fn ms(name: &str, default: f32, min: f32, max: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Skewed { min, max, factor: FloatRange::skew_factor(-1.8) })
        .with_unit(" ms")
        .with_value_to_string(formatters::v2s_f32_rounded(1))
}

#[derive(Params)]
pub struct GateParams {
    #[id = "g_on"]
    pub on: BoolParam,
    #[id = "g_thr"]
    pub thresh: FloatParam,
    #[id = "g_rng"]
    pub range: FloatParam,
}

#[derive(Params)]
pub struct TuneParams {
    #[id = "t_on"]
    pub on: BoolParam,
    #[id = "t_key"]
    pub key: EnumParam<Key>,
    #[id = "t_scale"]
    pub scale: EnumParam<Scale>,
    #[id = "t_speed"]
    pub speed: FloatParam,
    #[id = "t_hum"]
    pub humanize: FloatParam,
    #[id = "t_flex"]
    pub flex: FloatParam,
    #[id = "t_amt"]
    pub amount: FloatParam,
    #[id = "t_voice"]
    pub voice: EnumParam<Voice>,
    #[id = "t_guard"]
    pub guard: FloatParam,
}

#[derive(Params)]
pub struct CleanParams {
    #[id = "hp_on"]
    pub hp_on: BoolParam,
    #[id = "hp_f"]
    pub hp_freq: FloatParam,
    #[id = "mud_g"]
    pub mud_gain: FloatParam,
    #[id = "mud_f"]
    pub mud_freq: FloatParam,
    #[id = "hsh_g"]
    pub harsh_gain: FloatParam,
    #[id = "hsh_f"]
    pub harsh_freq: FloatParam,
}

#[derive(Params)]
pub struct DeessParams {
    #[id = "ds_on"]
    pub on: BoolParam,
    #[id = "ds_f"]
    pub freq: FloatParam,
    #[id = "ds_thr"]
    pub thresh: FloatParam,
    #[id = "ds_rng"]
    pub range: FloatParam,
}

#[derive(Params)]
pub struct PunchParams {
    #[id = "fet_on"]
    pub on: BoolParam,
    #[id = "fet_thr"]
    pub thresh: FloatParam,
    #[id = "fet_rat"]
    pub ratio: FloatParam,
    #[id = "fet_att"]
    pub attack: FloatParam,
    #[id = "fet_rel"]
    pub release: FloatParam,
    #[id = "fet_mk"]
    pub makeup: FloatParam,
    #[id = "fet_col"]
    pub color: FloatParam,
}

#[derive(Params)]
pub struct LevelParams {
    #[id = "opt_on"]
    pub on: BoolParam,
    #[id = "opt_amt"]
    pub amount: FloatParam,
    #[id = "opt_mk"]
    pub makeup: FloatParam,
}

#[derive(Params)]
pub struct SatParams {
    #[id = "sat_on"]
    pub on: BoolParam,
    #[id = "sat_mode"]
    pub mode: EnumParam<Saturation>,
    #[id = "sat_drv"]
    pub drive: FloatParam,
    #[id = "sat_mix"]
    pub mix: FloatParam,
}

#[derive(Params)]
pub struct ToneParams {
    #[id = "lo_g"]
    pub low_gain: FloatParam,
    #[id = "lo_f"]
    pub low_freq: FloatParam,
    #[id = "pr_g"]
    pub pres_gain: FloatParam,
    #[id = "pr_f"]
    pub pres_freq: FloatParam,
    #[id = "air_g"]
    pub air_gain: FloatParam,
    #[id = "air_f"]
    pub air_freq: FloatParam,
}

#[derive(Params)]
pub struct DoublerParams {
    #[id = "dbl_on"]
    pub on: BoolParam,
    #[id = "dbl_mix"]
    pub mix: FloatParam,
    #[id = "dbl_det"]
    pub detune: FloatParam,
    #[id = "dbl_dly"]
    pub delay: FloatParam,
    #[id = "dbl_wid"]
    pub width: FloatParam,
}

#[derive(Params)]
pub struct DelayParams {
    #[id = "dly_on"]
    pub on: BoolParam,
    #[id = "dly_div"]
    pub division: EnumParam<Division>,
    #[id = "dly_fb"]
    pub feedback: FloatParam,
    #[id = "dly_mix"]
    pub mix: FloatParam,
    #[id = "dly_pp"]
    pub ping_pong: BoolParam,
    #[id = "dly_hp"]
    pub hp: FloatParam,
    #[id = "dly_lp"]
    pub lp: FloatParam,
    #[id = "dly_duck"]
    pub duck: FloatParam,
}

#[derive(Params)]
pub struct ReverbParams {
    #[id = "rev_on"]
    pub on: BoolParam,
    #[id = "rev_type"]
    pub kind: EnumParam<Room>,
    #[id = "rev_size"]
    pub size: FloatParam,
    #[id = "rev_dec"]
    pub decay: FloatParam,
    #[id = "rev_pre"]
    pub predelay: FloatParam,
    #[id = "rev_damp"]
    pub damping: FloatParam,
    #[id = "rev_lc"]
    pub lowcut: FloatParam,
    #[id = "rev_mix"]
    pub mix: FloatParam,
    #[id = "rev_duck"]
    pub duck: FloatParam,
}

/// The six "vibe" knobs. 50% leaves the style exactly as designed.
#[derive(Params)]
pub struct VibeParams {
    #[id = "v_tune"]
    pub tune: FloatParam,
    #[id = "v_punch"]
    pub punch: FloatParam,
    #[id = "v_grit"]
    pub grit: FloatParam,
    #[id = "v_air"]
    pub air: FloatParam,
    #[id = "v_space"]
    pub space: FloatParam,
    #[id = "v_width"]
    pub width: FloatParam,
}

pub const MATCH_BANDS: usize = 8;
/// Centres of the Match EQ's bands, Hz.
pub const MATCH_HZ: [f32; MATCH_BANDS] = [120.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 14000.0];

/// Match My Voice: the corrective EQ it learned, and how much of it to apply.
#[derive(Params)]
pub struct MatchParams {
    #[id = "m_amt"]
    pub amount: FloatParam,
    #[id = "m_b0"]
    pub b0: FloatParam,
    #[id = "m_b1"]
    pub b1: FloatParam,
    #[id = "m_b2"]
    pub b2: FloatParam,
    #[id = "m_b3"]
    pub b3: FloatParam,
    #[id = "m_b4"]
    pub b4: FloatParam,
    #[id = "m_b5"]
    pub b5: FloatParam,
    #[id = "m_b6"]
    pub b6: FloatParam,
    #[id = "m_b7"]
    pub b7: FloatParam,
}

impl MatchParams {
    pub fn bands(&self) -> [&FloatParam; MATCH_BANDS] {
        [&self.b0, &self.b1, &self.b2, &self.b3, &self.b4, &self.b5, &self.b6, &self.b7]
    }
}

#[derive(Params)]
pub struct PersonaParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,
    /// The style (or user preset) last loaded, shown in the header.
    #[persist = "style"]
    pub style: Arc<RwLock<String>>,
    /// What Match My Voice learned about the singer.
    #[persist = "voice"]
    pub voice: Arc<RwLock<crate::profile::VoiceProfile>>,

    #[id = "in"]
    pub input: FloatParam,
    #[id = "out"]
    pub output: FloatParam,
    #[id = "mix"]
    pub mix: FloatParam,
    /// Set by each style so they all come out about as loud as they go in: compare styles by
    /// sound, not by volume.
    #[id = "trim"]
    pub trim: FloatParam,
    #[id = "lim_on"]
    pub limiter: BoolParam,
    #[id = "lim_ceil"]
    pub ceiling: FloatParam,

    #[nested(group = "Vibe")]
    pub vibe: VibeParams,
    #[nested(group = "Match")]
    pub matched: MatchParams,
    #[nested(group = "Gate")]
    pub gate: GateParams,
    #[nested(group = "Tune")]
    pub tune: TuneParams,
    #[nested(group = "Clean EQ")]
    pub clean: CleanParams,
    #[nested(group = "De-esser")]
    pub deess: DeessParams,
    #[nested(group = "Punch")]
    pub punch: PunchParams,
    #[nested(group = "Level")]
    pub level: LevelParams,
    #[nested(group = "Saturation")]
    pub sat: SatParams,
    #[nested(group = "Tone")]
    pub tone: ToneParams,
    #[nested(group = "Doubler")]
    pub doubler: DoublerParams,
    #[nested(group = "Delay")]
    pub delay: DelayParams,
    #[nested(group = "Reverb")]
    pub reverb: ReverbParams,
}

impl Default for PersonaParams {
    fn default() -> Self {
        let band = |name: &str| db(name, 0.0, -9.0, 9.0);
        Self {
            editor_state: EguiState::from_size(crate::editor::WIDTH, crate::editor::HEIGHT),
            style: Arc::new(RwLock::new(String::from("Clean Studio"))),
            voice: Arc::new(RwLock::new(crate::profile::VoiceProfile::default())),

            input: db("Input", 0.0, -24.0, 24.0),
            output: db("Output", 0.0, -24.0, 12.0),
            mix: pct("Mix", 1.0),
            trim: db("Style trim", 2.1, -12.0, 18.0),
            limiter: BoolParam::new("Limiter", true),
            ceiling: db("Ceiling", -1.0, -12.0, 0.0),

            vibe: VibeParams {
                tune: pct("Vibe: Tune", 0.5),
                punch: pct("Vibe: Punch", 0.5),
                grit: pct("Vibe: Grit", 0.5),
                air: pct("Vibe: Air", 0.5),
                space: pct("Vibe: Space", 0.5),
                width: pct("Vibe: Width", 0.5),
            },
            matched: MatchParams {
                amount: pct("Match amount", 0.0),
                b0: band("Match 120 Hz"),
                b1: band("Match 250 Hz"),
                b2: band("Match 500 Hz"),
                b3: band("Match 1 kHz"),
                b4: band("Match 2 kHz"),
                b5: band("Match 4 kHz"),
                b6: band("Match 8 kHz"),
                b7: band("Match 14 kHz"),
            },
            gate: GateParams {
                on: BoolParam::new("Gate", false),
                thresh: db("Gate threshold", -55.0, -80.0, -20.0),
                range: db("Gate range", 18.0, 0.0, 40.0),
            },
            tune: TuneParams {
                on: BoolParam::new("Tune", false),
                key: EnumParam::new("Key", Key::C),
                scale: EnumParam::new("Scale", Scale::Minor),
                speed: ms("Retune speed", 20.0, 0.0, 200.0),
                humanize: pct("Humanize", 0.0),
                flex: pct("Flex", 0.0),
                amount: pct("Tune amount", 1.0),
                voice: EnumParam::new("Voice", Voice::Mid),
                guard: pct("Consonant guard", 0.6),
            },
            clean: CleanParams {
                hp_on: BoolParam::new("High-pass", true),
                hp_freq: hz("High-pass freq", 80.0, 20.0, 400.0),
                mud_gain: db("Mud cut", 0.0, -12.0, 6.0),
                mud_freq: hz("Mud freq", 300.0, 120.0, 800.0),
                harsh_gain: db("Harshness cut", 0.0, -12.0, 6.0),
                harsh_freq: hz("Harshness freq", 3500.0, 1500.0, 8000.0),
            },
            deess: DeessParams {
                on: BoolParam::new("De-esser", true),
                freq: hz("De-ess freq", 6500.0, 3000.0, 12000.0),
                thresh: db("De-ess threshold", -24.0, -50.0, 0.0),
                range: db("De-ess range", 8.0, 0.0, 20.0),
            },
            punch: PunchParams {
                on: BoolParam::new("Punch", true),
                thresh: db("Punch threshold", -13.5, -40.0, 0.0),
                ratio: FloatParam::new("Punch ratio", 4.0, FloatRange::Skewed { min: 1.5, max: 20.0, factor: FloatRange::skew_factor(-1.5) })
                    .with_unit(":1")
                    .with_value_to_string(formatters::v2s_f32_rounded(1)),
                attack: ms("Punch attack", 1.0, 0.05, 30.0),
                release: ms("Punch release", 120.0, 20.0, 1000.0),
                makeup: db("Punch makeup", 3.0, -6.0, 18.0),
                color: pct("Punch color", 0.3),
            },
            level: LevelParams {
                on: BoolParam::new("Level", true),
                amount: pct("Level amount", 0.44),
                makeup: db("Level makeup", 2.0, -6.0, 12.0),
            },
            sat: SatParams {
                on: BoolParam::new("Saturation", false),
                mode: EnumParam::new("Saturation type", Saturation::Tape),
                drive: pct("Drive", 0.3),
                mix: pct("Saturation mix", 1.0),
            },
            tone: ToneParams {
                low_gain: db("Body", 0.0, -6.0, 6.0),
                low_freq: hz("Body freq", 180.0, 60.0, 400.0),
                pres_gain: db("Presence", 0.0, -6.0, 9.0),
                pres_freq: hz("Presence freq", 3500.0, 1500.0, 7000.0),
                air_gain: db("Air", 0.0, -6.0, 12.0),
                air_freq: hz("Air freq", 11000.0, 6000.0, 16000.0),
            },
            doubler: DoublerParams {
                on: BoolParam::new("Doubler", false),
                mix: pct("Doubler mix", 0.35),
                detune: FloatParam::new("Doubler detune", 9.0, FloatRange::Linear { min: 0.0, max: 30.0 })
                    .with_unit(" ct")
                    .with_value_to_string(formatters::v2s_f32_rounded(1)),
                delay: ms("Doubler delay", 14.0, 5.0, 40.0),
                width: pct("Doubler width", 0.8),
            },
            delay: DelayParams {
                on: BoolParam::new("Delay", false),
                division: EnumParam::new("Delay time", Division::D4),
                feedback: pct("Delay feedback", 0.3),
                mix: pct("Delay mix", 0.15),
                ping_pong: BoolParam::new("Ping-pong", true),
                hp: hz("Delay low cut", 300.0, 60.0, 2000.0),
                lp: hz("Delay high cut", 6000.0, 1500.0, 16000.0),
                duck: pct("Delay ducking", 0.5),
            },
            reverb: ReverbParams {
                on: BoolParam::new("Reverb", false),
                kind: EnumParam::new("Reverb type", Room::Plate),
                size: pct("Reverb size", 0.5),
                decay: FloatParam::new("Reverb decay", 1.6, FloatRange::Skewed { min: 0.2, max: 10.0, factor: FloatRange::skew_factor(-1.5) })
                    .with_unit(" s")
                    .with_value_to_string(formatters::v2s_f32_rounded(2)),
                predelay: ms("Pre-delay", 20.0, 0.0, 150.0),
                damping: pct("Reverb damping", 0.5),
                lowcut: hz("Reverb low cut", 300.0, 60.0, 1000.0),
                mix: pct("Reverb mix", 0.15),
                duck: pct("Reverb ducking", 0.3),
            },
        }
    }
}
