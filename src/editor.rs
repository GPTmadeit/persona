//! The plugin window, drawn with egui in Inter.
//!
//! Left: the style browser. Right, top to bottom: the loaded style, the six vibe knobs and
//! Match My Voice, the chain in signal order, and the controls of whichever module is selected.

use nih_plug::prelude::*;
use nih_plug_egui::create_egui_editor;
use nih_plug_egui::egui::{
    self, epaint::Mesh, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Painter, Pos2, Rect,
    Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};
use std::collections::{HashMap, VecDeque};
use std::f32::consts::PI;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::chain::{eq_filters, Effective, Settings};
use crate::dsp::filters::{gain_to_db, Biquad, Kind};
use crate::dsp::saturation::{SatMode, Saturator};
use crate::dsp::tune::scale::NOTE_NAMES;
use crate::params::*;
use crate::profile::{self, Measured, VoiceProfile};
use crate::shared::{Shared, LEARN_DONE, LEARN_FAILED, LEARN_IDLE, LEARN_LISTENING};
use crate::styles::{Style, Tag, KEEP_ON_STYLE, STYLES};
use crate::update::{self, SharedStatus, Status};

pub const WIDTH: u32 = 1180;
pub const HEIGHT: u32 = 760;

// ---------------------------------------------------------------- palette and type

const BG: Color32 = Color32::from_rgb(11, 12, 17);
const SURFACE: Color32 = Color32::from_rgb(19, 21, 29);
const RAISED: Color32 = Color32::from_rgb(28, 31, 42);
const HOVER: Color32 = Color32::from_rgb(37, 41, 55);
const WELL: Color32 = Color32::from_rgb(13, 14, 20);
const EDGE: Color32 = Color32::from_rgb(42, 46, 61);
const TEXT: Color32 = Color32::from_rgb(237, 239, 246);
const DIM: Color32 = Color32::from_rgb(148, 154, 174);
const FAINT: Color32 = Color32::from_rgb(86, 92, 112);
const GOOD: Color32 = Color32::from_rgb(84, 222, 154);
const WARN: Color32 = Color32::from_rgb(255, 192, 84);
const HOT: Color32 = Color32::from_rgb(255, 94, 112);

fn stroke(width: f32, color: Color32) -> Stroke {
    Stroke::new(width, color)
}

fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

fn heavy(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("black".into()))
}

fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn alpha(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// A style color made bright enough to work as an accent on the dark surfaces.
fn accent_of(c: u32) -> Color32 {
    let (r, g, b) = (((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let s = if max > 0.0 { d / max } else { 0.0 };
    let h = if d < 1e-6 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    let s = if s < 0.12 { s } else { s.clamp(0.42, 0.78) };
    let v = 0.97;
    let cc = v * s;
    let x = cc * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (h / 60.0) as i32 {
        0 => (cc, x, 0.0),
        1 => (x, cc, 0.0),
        2 => (0.0, cc, x),
        3 => (0.0, x, cc),
        4 => (x, 0.0, cc),
        _ => (cc, 0.0, x),
    };
    let m = v - cc;
    Color32::from_rgb(((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8)
}

fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split([' ', '-']).filter(|w| !w.is_empty() && w.chars().any(|c| c.is_uppercase() || c.is_ascii_digit())).collect();
    let Some(first) = words.first() else {
        return name.chars().take(2).collect();
    };
    if first.chars().all(|c| c.is_ascii_digit()) && first.len() >= 2 {
        return first.chars().take(2).collect();
    }
    let mut out: String = words.iter().take(2).filter_map(|w| w.chars().find(|c| c.is_alphanumeric())).collect();
    if out.chars().count() < 2 {
        out = first.chars().filter(|c| c.is_alphanumeric()).take(2).collect();
    }
    out.to_uppercase()
}

// ---------------------------------------------------------------- modules

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Module {
    Gate,
    Tune,
    Clean,
    Match,
    DeEss,
    Punch,
    Level,
    Drive,
    Tone,
    Double,
    Delay,
    Reverb,
    Output,
}

const MODULES: [Module; 13] = [
    Module::Gate,
    Module::Tune,
    Module::Clean,
    Module::Match,
    Module::DeEss,
    Module::Punch,
    Module::Level,
    Module::Drive,
    Module::Tone,
    Module::Double,
    Module::Delay,
    Module::Reverb,
    Module::Output,
];

impl Module {
    fn label(self) -> &'static str {
        match self {
            Module::Gate => "Gate",
            Module::Tune => "Tune",
            Module::Clean => "Clean",
            Module::Match => "Match",
            Module::DeEss => "De-ess",
            Module::Punch => "Punch",
            Module::Level => "Level",
            Module::Drive => "Drive",
            Module::Tone => "Tone",
            Module::Double => "Double",
            Module::Delay => "Delay",
            Module::Reverb => "Reverb",
            Module::Output => "Output",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Module::Gate => "Noise gate",
            Module::Tune => "Pitch correction",
            Module::Clean => "Clean-up EQ",
            Module::Match => "Match EQ",
            Module::DeEss => "De-esser",
            Module::Punch => "Punch",
            Module::Level => "Level",
            Module::Drive => "Drive",
            Module::Tone => "Tone EQ",
            Module::Double => "Doubler",
            Module::Delay => "Delay",
            Module::Reverb => "Reverb",
            Module::Output => "Output",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Module::Gate => "Turns down room noise and headphone bleed between phrases.",
            Module::Tune => "From a natural nudge to the hard, robotic snap. Set the song's key.",
            Module::Clean => "Rumble, mud and harshness out before anything gets louder.",
            Module::Match => "The EQ Match My Voice learned for your mic and voice.",
            Module::DeEss => "Tames sharp S and T sounds without dulling the voice.",
            Module::Punch => "Fast FET-style compression: grabs the front of every word.",
            Module::Level => "Slow opto-style levelling: keeps the vocal steady and forward.",
            Module::Drive => "Harmonics and attitude, from warm tape to phone and fuzz.",
            Module::Tone => "The style's color: body, presence and air.",
            Module::Double => "Two detuned copies left and right for a thick, wide vocal.",
            Module::Delay => "Tempo-synced echoes that duck while the words are coming.",
            Module::Reverb => "The room, plate or hall the vocal sits in.",
            Module::Output => "Input and output gain, dry/wet mix and the peak limiter.",
        }
    }

    #[cfg_attr(not(feature = "standalone"), allow(dead_code))]
    fn by_name(name: &str) -> Option<Module> {
        MODULES.iter().copied().find(|m| m.label().eq_ignore_ascii_case(name))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Led {
    Off,
    On,
    /// Off in the style, but a vibe knob is fading it in.
    Macro,
}

fn module_power(p: &PersonaParams, m: Module) -> Option<&BoolParam> {
    match m {
        Module::Gate => Some(&p.gate.on),
        Module::Tune => Some(&p.tune.on),
        Module::DeEss => Some(&p.deess.on),
        Module::Punch => Some(&p.punch.on),
        Module::Level => Some(&p.level.on),
        Module::Drive => Some(&p.sat.on),
        Module::Double => Some(&p.doubler.on),
        Module::Delay => Some(&p.delay.on),
        Module::Reverb => Some(&p.reverb.on),
        _ => None,
    }
}

fn module_led(s: &Settings, e: &Effective, m: Module) -> Led {
    let on = |b: bool| if b { Led::On } else { Led::Off };
    match m {
        Module::Gate => on(s.gate_on),
        Module::Tune => on(s.tune_on),
        Module::Clean => on(s.hp_on || s.mud_gain.abs() > 0.05 || s.harsh_gain.abs() > 0.05),
        Module::Match => on(s.match_amount > 0.001 && s.match_db.iter().any(|g| g.abs() > 0.05)),
        Module::DeEss => on(s.ds_on),
        Module::Punch => on(s.punch_on),
        Module::Level => on(s.level_on),
        Module::Drive => match (s.sat_on, e.sat.is_some()) {
            (true, _) => Led::On,
            (false, true) => Led::Macro,
            _ => Led::Off,
        },
        Module::Tone => on(s.low_gain.abs() > 0.05 || e.pres_gain.abs() > 0.05 || e.air_gain.abs() > 0.05),
        Module::Double => match (s.dbl_on, e.dbl_mix > 0.0) {
            (true, _) => Led::On,
            (false, true) => Led::Macro,
            _ => Led::Off,
        },
        Module::Delay => on(s.dly_on),
        Module::Reverb => match (s.rev_on, e.rev_mix > 0.0) {
            (true, _) => Led::On,
            (false, true) => Led::Macro,
            _ => Led::Off,
        },
        Module::Output => Led::On,
    }
}

// ---------------------------------------------------------------- presets and state

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct UserPreset {
    name: String,
    /// The style it was made from, for its colors.
    #[serde(default)]
    base: Option<String>,
    values: Vec<(String, f32)>,
}

/// One row of the browser: a factory style or a preset the user saved.
struct Entry {
    name: String,
    tags: Vec<Tag>,
    blurb: String,
    colors: [u32; 2],
    style: Option<&'static Style>,
    values: Vec<(String, f32)>,
}

impl Entry {
    fn user(&self) -> bool {
        self.style.is_none()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Favorites,
    Mine,
    Tag(Tag),
}

struct Snapshot {
    values: Vec<(ParamPtr, f32)>,
    profile: VoiceProfile,
}

/// What the meters showed on one frame, for the scrolling views.
#[derive(Clone, Copy, Default)]
struct Frame {
    midi: f32,
    out_midi: f32,
    voiced: bool,
    gr: [f32; 5],
}

const HISTORY: usize = 240;

struct EditorState {
    params: Arc<PersonaParams>,
    shared: Arc<Shared>,
    ids: Vec<String>,
    ptrs: HashMap<String, ParamPtr>,
    entries: Vec<Entry>,
    favorites: Vec<String>,
    search: String,
    filter: Filter,
    module: Module,
    /// What the loaded style set every parameter to, to tell when it's been edited.
    loaded: Vec<(ParamPtr, f32)>,
    loaded_name: String,
    learn_pending: bool,
    learn_summary: Vec<String>,
    learn_failed: bool,
    undo: Option<Snapshot>,
    save_open: bool,
    save_name: String,
    confirm_delete: Option<(String, f64)>,
    update: SharedStatus,
    settings: update::Settings,
    settings_open: bool,
    toast: Option<(String, f64)>,
    history: VecDeque<Frame>,
    accent: Color32,
    fonts_ready: bool,
    startup_style: Option<String>,
}

fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Persona"))
}

fn user_preset_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("Presets"))
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn load_user_presets() -> Vec<UserPreset> {
    let Some(dir) = user_preset_dir() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<UserPreset> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    v.sort_by_key(|p| p.name.to_lowercase());
    v
}

fn load_favorites() -> Vec<String> {
    config_dir()
        .and_then(|d| std::fs::read_to_string(d.join("favorites.json")).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_favorites(f: &[String]) {
    if let Some(d) = config_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("favorites.json"), serde_json::to_string_pretty(f).unwrap_or_default());
    }
}

impl EditorState {
    fn reload_entries(&mut self) {
        self.entries = STYLES
            .iter()
            .map(|s| Entry {
                name: s.name.to_string(),
                tags: s.tags.to_vec(),
                blurb: s.blurb.to_string(),
                colors: s.colors,
                style: Some(s),
                values: Vec::new(),
            })
            .collect();
        for u in load_user_presets() {
            let base = u.base.as_deref().and_then(crate::styles::find);
            self.entries.push(Entry {
                blurb: match base {
                    Some(b) => format!("Your preset, made from {}.", b.name),
                    None => "Your preset.".to_string(),
                },
                tags: base.map(|b| b.tags.to_vec()).unwrap_or_default(),
                colors: base.map(|b| b.colors).unwrap_or([0x4B5568, 0x1A1E2A]),
                name: u.name,
                style: None,
                values: u.values,
            });
        }
    }

    fn ptr(&self, id: &str) -> Option<ParamPtr> {
        self.ptrs.get(id).copied()
    }

    fn default_plain(&self, id: &str) -> f32 {
        self.ptr(id).map(|p| unsafe { p.preview_plain(p.default_normalized_value()) }).unwrap_or(0.0)
    }

    fn plain(&self, id: &str) -> f32 {
        self.ptr(id).map(|p| unsafe { p.unmodulated_plain_value() }).unwrap_or(0.0)
    }

    fn entry_values(&self, e: &Entry) -> Vec<(String, f32)> {
        match e.style {
            Some(s) => profile::fit_style(s, &self.params.voice.read(), &|id| self.default_plain(id)),
            None => e.values.clone(),
        }
    }

    /// The normalized value every sound parameter gets when `e` loads.
    fn targets(&self, e: &Entry) -> Vec<(ParamPtr, f32)> {
        let values = self.entry_values(e);
        self.ids
            .iter()
            .filter(|id| !KEEP_ON_STYLE.contains(&id.as_str()))
            .map(|id| {
                let ptr = self.ptrs[id];
                let n = match values.iter().find(|(k, _)| k == id) {
                    Some((_, v)) => unsafe { ptr.preview_normalized(*v) },
                    None => unsafe { ptr.default_normalized_value() },
                };
                (ptr, n)
            })
            .collect()
    }

    fn apply(&self, setter: &ParamSetter, targets: &[(ParamPtr, f32)]) {
        let ctx = setter.raw_context;
        for &(ptr, n) in targets {
            unsafe {
                if (ptr.unmodulated_normalized_value() - n).abs() > 1e-6 {
                    ctx.raw_begin_set_parameter(ptr);
                    ctx.raw_set_parameter_normalized(ptr, n);
                    ctx.raw_end_set_parameter(ptr);
                }
            }
        }
    }

    fn set_plain(&self, setter: &ParamSetter, values: &[(&str, f32)]) {
        let t: Vec<(ParamPtr, f32)> = values
            .iter()
            .filter_map(|(id, v)| self.ptr(id).map(|p| (p, unsafe { p.preview_normalized(*v) })))
            .collect();
        self.apply(setter, &t);
    }

    fn current(&self) -> Option<usize> {
        let name = self.params.style.read().clone();
        self.entries.iter().position(|e| e.name == name)
    }

    fn load(&mut self, setter: &ParamSetter, idx: usize) {
        let Some(e) = self.entries.get(idx) else {
            return;
        };
        let t = self.targets(e);
        self.apply(setter, &t);
        let name = e.name.clone();
        *self.params.style.write() = name.clone();
        self.loaded = t;
        self.loaded_name = name;
        self.confirm_delete = None;
    }

    /// Recomputes what the current style sets, without touching anything: after the editor
    /// opens on a saved project, or after Match changes how styles are fitted.
    fn refresh_loaded(&mut self) {
        let name = self.params.style.read().clone();
        self.loaded = match self.entries.iter().find(|e| e.name == name) {
            Some(e) => self.targets(e),
            None => Vec::new(),
        };
        self.loaded_name = name;
    }

    fn edited(&self) -> bool {
        self.loaded.iter().any(|&(ptr, n)| unsafe { (ptr.unmodulated_normalized_value() - n).abs() > 1e-4 })
    }

    fn visible(&self) -> Vec<usize> {
        let q = self.search.trim().to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| match self.filter {
                Filter::All => true,
                Filter::Favorites => self.favorites.contains(&e.name),
                Filter::Mine => e.user(),
                Filter::Tag(t) => e.tags.contains(&t),
            })
            .filter(|(_, e)| {
                q.is_empty()
                    || e.name.to_lowercase().contains(&q)
                    || e.blurb.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.label().to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn step(&mut self, setter: &ParamSetter, dir: i32) {
        let mut list = self.visible();
        let cur = self.current();
        if list.is_empty() || cur.is_some_and(|c| !list.contains(&c)) {
            list = (0..self.entries.len()).collect();
        }
        let pos = cur.and_then(|c| list.iter().position(|&i| i == c));
        let next = match pos {
            Some(p) => (p as i32 + dir).rem_euclid(list.len() as i32) as usize,
            None => 0,
        };
        self.load(setter, list[next]);
    }

    fn toggle_favorite(&mut self, name: &str) {
        if let Some(i) = self.favorites.iter().position(|f| f == name) {
            self.favorites.remove(i);
        } else {
            self.favorites.push(name.to_string());
        }
        save_favorites(&self.favorites);
    }

    fn save_user(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("give the preset a name first".into());
        }
        if STYLES.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
            return Err("that name belongs to a built-in style".into());
        }
        let values = self
            .ids
            .iter()
            .filter(|id| !KEEP_ON_STYLE.contains(&id.as_str()))
            .map(|id| (id.clone(), self.plain(id)))
            .collect();
        let base = self
            .current()
            .and_then(|i| match self.entries[i].style {
                Some(s) => Some(s.name.to_string()),
                None => self.entries[i].blurb.strip_prefix("Your preset, made from ").map(|b| b.trim_end_matches('.').to_string()),
            });
        let dir = user_preset_dir().ok_or("no config folder on this system")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&UserPreset { name: name.to_string(), base, values }).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(format!("{}.json", sanitize(name))), json).map_err(|e| e.to_string())?;
        *self.params.style.write() = name.to_string();
        self.reload_entries();
        self.refresh_loaded();
        Ok(())
    }

    fn delete_user(&mut self, name: &str) {
        if let Some(dir) = user_preset_dir() {
            let _ = std::fs::remove_file(dir.join(format!("{}.json", sanitize(name))));
        }
        self.reload_entries();
        self.refresh_loaded();
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            values: self.ids.iter().map(|id| (self.ptrs[id], unsafe { self.ptrs[id].unmodulated_normalized_value() })).collect(),
            profile: self.params.voice.read().clone(),
        }
    }

    fn start_learn(&mut self) {
        self.shared.learn_state.store(LEARN_IDLE, Ordering::Relaxed);
        self.shared.learn_progress.store(0.0, Ordering::Relaxed);
        self.shared.learn_cancel.store(false, Ordering::Relaxed);
        self.shared.learn_request.store(true, Ordering::Relaxed);
        self.learn_pending = true;
        self.learn_failed = false;
        self.learn_summary.clear();
    }

    fn cancel_learn(&mut self) {
        self.shared.learn_request.store(false, Ordering::Relaxed);
        self.shared.learn_cancel.store(true, Ordering::Relaxed);
        self.learn_pending = false;
    }

    fn poll_learn(&mut self, setter: &ParamSetter) {
        if !self.learn_pending {
            return;
        }
        match self.shared.learn_state.load(Ordering::Relaxed) {
            LEARN_DONE => {
                self.learn_pending = false;
                self.apply_match(setter);
            }
            LEARN_FAILED => {
                self.learn_pending = false;
                self.learn_failed = true;
            }
            _ => {}
        }
    }

    fn apply_match(&mut self, setter: &ParamSetter) {
        let r = &self.shared.learn;
        let ld = |a: &AtomicF32| a.load(Ordering::Relaxed);
        let m = Measured {
            level_db: ld(&r.level_db),
            bands_db: std::array::from_fn(|i| ld(&r.bands_db[i])),
            sibilance_db: ld(&r.sibilance_db),
            median_midi: ld(&r.median_midi),
            key: r.key_valid.load(Ordering::Relaxed).then(|| {
                (r.key_root.load(Ordering::Relaxed), r.key_minor.load(Ordering::Relaxed), ld(&r.key_confidence))
            }),
        };
        let o = profile::analyse(&m);
        self.undo = Some(self.snapshot());
        let mut values: Vec<(String, f32)> = vec![("in".into(), o.input_db), ("m_amt".into(), o.match_amount)];
        for (i, g) in o.bands_db.iter().enumerate() {
            values.push((format!("m_b{i}"), *g));
        }
        if let Some(v) = o.voice {
            values.push(("t_voice".into(), v as f32));
        }
        if let Some((root, minor)) = o.key {
            values.push(("t_key".into(), root as f32));
            values.push(("t_scale".into(), if minor { 2.0 } else { 1.0 }));
        }
        *self.params.voice.write() = o.profile.clone();
        // Refit the voice-dependent values from the style's design, not from what they are now,
        // so matching twice doesn't add up.
        if let Some(i) = self.current() {
            let mut design: Vec<(String, f32)> = match self.entries[i].style {
                Some(s) => s.values.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                None => self.entries[i].values.clone(),
            };
            profile::fit_values(&mut design, &o.profile, &|id| self.default_plain(id));
            for id in ["ds_thr", "ds_rng", "hp_f"] {
                if let Some((_, v)) = design.iter().find(|(k, _)| k == id) {
                    values.push((id.to_string(), *v));
                }
            }
        }
        let refs: Vec<(&str, f32)> = values.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        self.set_plain(setter, &refs);
        self.refresh_loaded();
        self.learn_summary = o.summary;
    }

    fn undo_match(&mut self, setter: &ParamSetter) {
        if let Some(s) = self.undo.take() {
            self.apply(setter, &s.values);
            *self.params.voice.write() = s.profile;
            self.refresh_loaded();
            self.learn_summary.clear();
        }
    }

    fn forget_voice(&mut self, setter: &ParamSetter) {
        self.undo = Some(self.snapshot());
        *self.params.voice.write() = VoiceProfile::default();
        let mut v: Vec<(&str, f32)> = vec![("in", 0.0), ("m_amt", 0.0)];
        let ids: Vec<String> = (0..MATCH_BANDS).map(|i| format!("m_b{i}")).collect();
        for id in &ids {
            v.push((id, 0.0));
        }
        self.set_plain(setter, &v);
        if let Some(i) = self.current() {
            let t = self.targets(&self.entries[i]);
            let keep: Vec<(ParamPtr, f32)> = ["ds_thr", "ds_rng", "hp_f"]
                .iter()
                .filter_map(|id| self.ptr(id).and_then(|p| t.iter().find(|(q, _)| *q == p).copied()))
                .collect();
            self.apply(setter, &keep);
        }
        self.refresh_loaded();
        self.learn_summary.clear();
    }

    fn toast(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ctx.input(|i| i.time) + 2.6));
    }

    fn record_frame(&mut self) {
        let sh = &self.shared;
        let ld = |a: &AtomicF32| a.load(Ordering::Relaxed);
        let midi = ld(&sh.pitch_midi);
        let f = Frame {
            midi,
            out_midi: midi + ld(&sh.pitch_correction),
            voiced: sh.pitch_voiced.load(Ordering::Relaxed),
            gr: [ld(&sh.gate_gr), ld(&sh.deess_gr), ld(&sh.punch_gr), ld(&sh.level_gr), ld(&sh.limit_gr)],
        };
        if self.history.len() >= HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(f);
    }

    fn style_colors(&self) -> [u32; 2] {
        self.current().map(|i| self.entries[i].colors).unwrap_or([0x7B5CFF, 0x1B0F45])
    }
}

// ---------------------------------------------------------------- editor

pub fn create(params: Arc<PersonaParams>, shared: Arc<Shared>) -> Option<Box<dyn Editor>> {
    let map = params.param_map();
    let mut state = EditorState {
        ids: map.iter().map(|(id, _, _)| id.clone()).collect(),
        ptrs: map.iter().map(|(id, p, _)| (id.clone(), *p)).collect(),
        params: params.clone(),
        shared,
        entries: Vec::new(),
        favorites: load_favorites(),
        search: String::new(),
        filter: Filter::All,
        module: Module::Tune,
        loaded: Vec::new(),
        loaded_name: String::new(),
        learn_pending: false,
        learn_summary: Vec::new(),
        learn_failed: false,
        undo: None,
        save_open: false,
        save_name: String::new(),
        confirm_delete: None,
        update: Arc::new(parking_lot::Mutex::new(Status::Idle)),
        settings: update::load_settings(),
        settings_open: false,
        toast: None,
        history: VecDeque::with_capacity(HISTORY),
        accent: accent_of(0x7B5CFF),
        fonts_ready: false,
        startup_style: None,
    };
    state.reload_entries();
    state.accent = accent_of(state.style_colors()[0]);
    // Lets the standalone build open straight onto a style or module, for screenshots.
    #[cfg(feature = "standalone")]
    {
        state.startup_style = std::env::var("PERSONA_STYLE").ok();
        if let Some(m) = std::env::var("PERSONA_MODULE").ok().as_deref().and_then(Module::by_name) {
            state.module = m;
        }
        if let Ok(q) = std::env::var("PERSONA_SEARCH") {
            state.search = q;
        }
    }

    create_egui_editor(
        params.editor_state.clone(),
        state,
        |ctx, state| {
            setup_fonts(ctx);
            let mut style = (*ctx.style()).clone();
            style.visuals = egui::Visuals::dark();
            style.visuals.panel_fill = BG;
            style.visuals.window_fill = SURFACE;
            style.visuals.window_stroke = stroke(1.0, EDGE);
            style.visuals.window_corner_radius = CornerRadius::same(12);
            style.visuals.extreme_bg_color = WELL;
            style.visuals.override_text_color = Some(TEXT);
            style.visuals.widgets.inactive.weak_bg_fill = RAISED;
            style.visuals.widgets.inactive.bg_fill = RAISED;
            style.visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
            style.visuals.widgets.hovered.weak_bg_fill = HOVER;
            style.visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
            style.visuals.widgets.active.corner_radius = CornerRadius::same(8);
            style.visuals.widgets.open.corner_radius = CornerRadius::same(8);
            style.visuals.selection.bg_fill = alpha(state.accent, 0.35);
            style.visuals.selection.stroke = stroke(1.0, state.accent);
            style.spacing.item_spacing = vec2(8.0, 6.0);
            style.spacing.button_padding = vec2(10.0, 4.0);
            style.spacing.interact_size.y = 26.0;
            style.text_styles.insert(egui::TextStyle::Body, regular(13.0));
            style.text_styles.insert(egui::TextStyle::Button, semi(12.5));
            style.text_styles.insert(egui::TextStyle::Small, regular(11.0));
            style.text_styles.insert(egui::TextStyle::Heading, semi(17.0));
            ctx.set_style(style);
            if state.settings.auto_check && *state.update.lock() == Status::Idle {
                update::spawn_check(state.update.clone());
            }
        },
        |ctx, setter, state| {
            // Named font families exist from the frame after `set_fonts`.
            if !state.fonts_ready {
                state.fonts_ready = true;
                ctx.request_repaint();
                return;
            }
            if let Some(name) = state.startup_style.take() {
                if let Some(i) = state.entries.iter().position(|e| e.name.eq_ignore_ascii_case(&name)) {
                    state.load(setter, i);
                }
            }
            if *state.params.style.read() != state.loaded_name {
                state.refresh_loaded();
            }
            state.poll_learn(setter);
            state.record_frame();
            let target = accent_of(state.style_colors()[0]);
            state.accent = mix(state.accent, target, 0.18);
            ctx.style_mut(|s| {
                s.visuals.selection.bg_fill = alpha(state.accent, 0.35);
                s.visuals.selection.stroke = stroke(1.0, state.accent);
            });

            egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ctx, |ui| {
                let full = ui.max_rect();
                let m = 14.0;
                let header = Rect::from_min_size(full.min + vec2(m, 12.0), vec2(full.width() - 2.0 * m, 44.0));
                let top = header.bottom() + 10.0;
                let side = Rect::from_min_max(pos2(full.left() + m, top), pos2(full.left() + m + 292.0, full.bottom() - m));
                let main = Rect::from_min_max(pos2(side.right() + 12.0, top), pos2(full.right() - m, full.bottom() - m));
                let hero = Rect::from_min_size(main.min, vec2(main.width(), 134.0));
                let row = Rect::from_min_size(pos2(main.left(), hero.bottom() + 12.0), vec2(main.width(), 170.0));
                let vibe = Rect::from_min_size(row.min, vec2(row.width() - 292.0, row.height()));
                let learn = Rect::from_min_max(pos2(vibe.right() + 12.0, row.top()), row.max);
                let strip = Rect::from_min_size(pos2(main.left(), row.bottom() + 12.0), vec2(main.width(), 62.0));
                let module = Rect::from_min_max(pos2(main.left(), strip.bottom() + 12.0), main.max);

                header_bar(&mut child(ui, header), state);
                sidebar(&mut child(ui, side), setter, state);
                hero_card(&mut child(ui, hero), setter, state);
                vibe_panel(&mut child(ui, vibe), setter, state);
                match_panel(&mut child(ui, learn), setter, state);
                chain_strip(&mut child(ui, strip), setter, state);
                module_panel(&mut child(ui, module), setter, state);
            });

            if state.settings_open {
                settings_window(ctx, setter, state);
            }
            if state.save_open {
                save_window(ctx, state);
            }
            if let Some((msg, until)) = state.toast.clone() {
                if ctx.input(|i| i.time) > until {
                    state.toast = None;
                } else {
                    egui::Area::new(egui::Id::new("toast")).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -28.0)).show(ctx, |ui| {
                        egui::Frame::new()
                            .fill(RAISED)
                            .stroke(stroke(1.0, state.accent))
                            .corner_radius(10)
                            .inner_margin(egui::Margin::symmetric(14, 8))
                            .show(ui, |ui| ui.label(egui::RichText::new(msg).font(semi(12.5)).color(TEXT)));
                    });
                }
            }
            // Meters and the pitch trace follow the audio thread.
            ctx.request_repaint();
        },
    )
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let add = |fonts: &mut egui::FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.to_string(), Arc::new(egui::FontData::from_static(bytes)));
    };
    add(&mut fonts, "inter", include_bytes!("../assets/fonts/Inter-Regular.ttf"));
    add(&mut fonts, "inter-semibold", include_bytes!("../assets/fonts/Inter-SemiBold.ttf"));
    add(&mut fonts, "inter-black", include_bytes!("../assets/fonts/Inter-Black.ttf"));
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "inter".into());
    for (family, font) in [("semibold", "inter-semibold"), ("black", "inter-black")] {
        let mut list = vec![font.to_string()];
        list.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(family.into()), list);
    }
    ctx.set_fonts(fonts);
}

fn child(ui: &mut Ui, rect: Rect) -> Ui {
    ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)))
}

// ---------------------------------------------------------------- drawing helpers

fn card(p: &Painter, r: Rect) {
    p.rect_filled(r, 14, SURFACE);
    p.rect_stroke(r, 14, stroke(1.0, alpha(EDGE, 0.7)), StrokeKind::Inside);
}

/// A rounded rectangle filled with a linear gradient along `dir`.
fn gradient_rect(p: &Painter, r: Rect, radius: f32, c0: Color32, c1: Color32, dir: Vec2) {
    let rad = radius.min(r.width() / 2.0).min(r.height() / 2.0);
    let corners = [
        (pos2(r.right() - rad, r.bottom() - rad), 0.0),
        (pos2(r.left() + rad, r.bottom() - rad), 0.5 * PI),
        (pos2(r.left() + rad, r.top() + rad), PI),
        (pos2(r.right() - rad, r.top() + rad), 1.5 * PI),
    ];
    let mut pts = Vec::with_capacity(32);
    for (c, a0) in corners {
        for k in 0..=7 {
            let a = a0 + 0.5 * PI * k as f32 / 7.0;
            pts.push(c + vec2(a.cos(), a.sin()) * rad);
        }
    }
    let dir = dir.normalized();
    let proj = |q: Pos2| (q - r.min).dot(dir);
    let (lo, hi) = [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()]
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), q| (lo.min(proj(*q)), hi.max(proj(*q))));
    let color = |q: Pos2| {
        let t = (proj(q) - lo) / (hi - lo).max(1e-3);
        let a = c0.a() as f32 + (c1.a() as f32 - c0.a() as f32) * t;
        let rgb = mix(c0, c1, t);
        Color32::from_rgba_unmultiplied(rgb.r(), rgb.g(), rgb.b(), a as u8)
    };
    let mut mesh = Mesh::default();
    mesh.colored_vertex(r.center(), color(r.center()));
    for q in &pts {
        mesh.colored_vertex(*q, color(*q));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    p.add(Shape::mesh(mesh));
}

/// Translucent rings that read as a soft glow.
fn glow(p: &Painter, c: Pos2, r: f32, color: Color32, strength: f32) {
    for i in 0..6 {
        let t = i as f32 / 6.0;
        p.circle_filled(c, r * (1.0 + t * 0.9), alpha(color, strength * 0.09 * (1.0 - t)));
    }
}

fn arc(p: &Painter, c: Pos2, r: f32, a: f32, b: f32, stroke: Stroke) {
    let n = ((b - a).abs() * r / 3.0).ceil().clamp(2.0, 96.0) as usize;
    let pts: Vec<Pos2> = (0..=n).map(|i| a + (b - a) * i as f32 / n as f32).map(|t| c + vec2(t.cos(), t.sin()) * r).collect();
    p.add(Shape::line(pts, stroke));
}

fn star(p: &Painter, c: Pos2, r: f32, filled: bool, color: Color32) {
    let pts: Vec<Pos2> = (0..10)
        .map(|i| {
            let a = -0.5 * PI + i as f32 * PI / 5.0;
            let rr = if i % 2 == 0 { r } else { r * 0.45 };
            c + vec2(a.cos(), a.sin()) * rr
        })
        .collect();
    if filled {
        // Not convex: fill it as a fan from the centre.
        let mut mesh = Mesh::default();
        mesh.colored_vertex(c, color);
        for q in &pts {
            mesh.colored_vertex(*q, color);
        }
        for i in 0..10u32 {
            mesh.add_triangle(0, 1 + i, 1 + (i + 1) % 10);
        }
        p.add(Shape::mesh(mesh));
    } else {
        let mut closed = pts.clone();
        closed.push(pts[0]);
        p.add(Shape::line(closed, stroke(1.3, color)));
    }
}

fn chevron(p: &Painter, c: Pos2, left: bool, color: Color32) {
    let s = if left { -1.0 } else { 1.0 };
    p.add(Shape::line(vec![c + vec2(-3.0 * s, -5.0), c + vec2(3.0 * s, 0.0), c + vec2(-3.0 * s, 5.0)], stroke(2.0, color)));
}

fn gear(p: &Painter, c: Pos2, color: Color32) {
    for i in 0..8 {
        let a = i as f32 * PI / 4.0;
        p.line_segment([c + vec2(a.cos(), a.sin()) * 4.5, c + vec2(a.cos(), a.sin()) * 8.0], stroke(2.6, color));
    }
    p.circle_stroke(c, 5.0, stroke(2.2, color));
}

// ---------------------------------------------------------------- widgets

fn hover_cursor(r: Response) -> Response {
    r.on_hover_cursor(CursorIcon::PointingHand)
}

/// A flat button: `fill` behind, lighter on hover.
fn button(ui: &mut Ui, text: &str, size: Vec2, fill: Color32, color: Color32) -> Response {
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let f = if resp.is_pointer_button_down_on() {
        mix(fill, Color32::BLACK, 0.15)
    } else if resp.hovered() {
        mix(fill, Color32::WHITE, 0.08)
    } else {
        fill
    };
    ui.painter().rect_filled(r, 9, f);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, text, semi(12.5), color);
    hover_cursor(resp)
}

fn icon_button(ui: &mut Ui, size: f32, draw: impl FnOnce(&Painter, Pos2, Color32)) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 8, alpha(Color32::WHITE, 0.08));
    }
    draw(p, r.center(), if resp.hovered() { TEXT } else { DIM });
    hover_cursor(resp)
}

fn chip(ui: &mut Ui, text: &str, selected: bool, accent: Color32) -> Response {
    let font = semi(11.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), TEXT);
    let size = vec2(galley.size().x + 20.0, 24.0);
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let (fill, color) = if selected {
        (accent, Color32::from_rgb(12, 12, 16))
    } else if resp.hovered() {
        (HOVER, TEXT)
    } else {
        (RAISED, DIM)
    };
    p.rect_filled(r, 12, fill);
    p.text(r.center(), Align2::CENTER_CENTER, text, font, color);
    hover_cursor(resp)
}

/// Drag up/down (Shift = fine), scroll, double-click to reset.
fn drag_param(ui: &Ui, setter: &ParamSetter, param: &FloatParam, resp: &Response, travel: f32) {
    let id = resp.id;
    let fine = ui.input(|i| i.modifiers.shift || i.modifiers.ctrl);
    if resp.double_clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter_normalized(param, param.default_normalized_value());
        setter.end_set_parameter(param);
    } else if resp.drag_started() {
        setter.begin_set_parameter(param);
        ui.memory_mut(|m| m.data.insert_temp(id, param.unmodulated_normalized_value()));
    }
    if resp.dragged() {
        let mut v: f32 = ui.memory(|m| m.data.get_temp(id)).unwrap_or(param.unmodulated_normalized_value());
        v = (v - resp.drag_delta().y / travel * if fine { 0.15 } else { 1.0 }).clamp(0.0, 1.0);
        ui.memory_mut(|m| m.data.insert_temp(id, v));
        setter.set_parameter_normalized(param, v);
    }
    if resp.drag_stopped() {
        setter.end_set_parameter(param);
    }
    if resp.hovered() && !resp.dragged() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            let v = param.unmodulated_normalized_value();
            let n = (v + scroll.signum() * if fine { 0.002 } else { 0.02 }).clamp(0.0, 1.0);
            setter.begin_set_parameter(param);
            setter.set_parameter_normalized(param, n);
            setter.end_set_parameter(param);
        }
    }
}

fn knob_face(p: &Painter, c: Pos2, r: f32, value: f32, from: f32, accent: Color32, active: bool, track: f32) {
    let a0 = PI * 0.75;
    let sweep = PI * 1.5;
    p.circle_filled(c, r, RAISED);
    p.circle_stroke(c, r, stroke(1.0, alpha(Color32::WHITE, 0.06)));
    let tr = r - track * 0.5 - 3.0;
    arc(p, c, tr, a0, a0 + sweep, stroke(track, WELL));
    let (lo, hi) = if value >= from { (from, value) } else { (value, from) };
    if hi - lo > 0.002 {
        arc(p, c, tr, a0 + sweep * lo, a0 + sweep * hi, stroke(track + 4.0, alpha(accent, 0.18)));
        arc(p, c, tr, a0 + sweep * lo, a0 + sweep * hi, stroke(track, accent));
    }
    let inner = tr - track * 0.5 - 3.0;
    p.circle_filled(c, inner, if active { HOVER } else { SURFACE });
    let ang = a0 + sweep * value;
    p.circle_filled(c + vec2(ang.cos(), ang.sin()) * (inner - 5.0), 2.4, TEXT);
}

/// A compact knob with its name below; the value shows in place of the name while touched.
fn knob(ui: &mut Ui, setter: &ParamSetter, param: &FloatParam, label: &str, accent: Color32, bipolar: bool) -> Response {
    let d = 44.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(68.0, 72.0), Sense::click_and_drag());
    drag_param(ui, setter, param, &resp, 180.0);
    let p = ui.painter();
    let c = pos2(rect.center().x, rect.top() + d / 2.0 + 2.0);
    let active = resp.hovered() || resp.dragged();
    let from = if bipolar { param.preview_normalized(0.0) } else { 0.0 };
    knob_face(p, c, d / 2.0, param.modulated_normalized_value(), from, accent, active, 3.5);
    let text = if active { param.normalized_value_to_string(param.modulated_normalized_value(), true) } else { label.to_string() };
    p.text(pos2(rect.center().x, rect.bottom() - 11.0), Align2::CENTER_CENTER, text, semi(11.0), if active { accent } else { DIM });
    resp.on_hover_text(format!("{}: {}\nDrag (Shift = fine), scroll, double-click = reset", param.name(), param))
}

/// A vertical fader for the Match EQ bands, filled from 0 dB.
fn fader(ui: &mut Ui, setter: &ParamSetter, param: &FloatParam, label: &str, accent: Color32, height: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, height), Sense::click_and_drag());
    drag_param(ui, setter, param, &resp, height - 34.0);
    let p = ui.painter();
    let track = Rect::from_center_size(pos2(rect.center().x, rect.top() + (height - 30.0) / 2.0 + 4.0), vec2(6.0, height - 38.0));
    p.rect_filled(track, 3, WELL);
    let v = param.modulated_normalized_value();
    let zero = param.preview_normalized(0.0);
    let y = |n: f32| track.bottom() - n * track.height();
    let (a, b) = (y(zero), y(v));
    p.rect_filled(Rect::from_x_y_ranges(track.x_range(), a.min(b)..=a.max(b)), 3, accent);
    p.line_segment([pos2(track.left() - 5.0, a), pos2(track.right() + 5.0, a)], stroke(1.0, FAINT));
    let active = resp.hovered() || resp.dragged();
    p.circle_filled(pos2(track.center().x, b), if active { 7.0 } else { 6.0 }, TEXT);
    p.circle_filled(pos2(track.center().x, b), 2.5, accent);
    let text = if active { format!("{:+.1}", param.modulated_plain_value()) } else { label.to_string() };
    p.text(pos2(rect.center().x, rect.bottom() - 9.0), Align2::CENTER_CENTER, text, semi(10.5), if active { accent } else { DIM });
    resp.on_hover_text(format!("{}: {}", param.name(), param))
}

/// An iOS-style switch.
fn switch(ui: &mut Ui, setter: &ParamSetter, param: &BoolParam, accent: Color32) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
    let on = param.value();
    if resp.clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, !on);
        setter.end_set_parameter(param);
    }
    let t = ui.ctx().animate_bool(resp.id, on);
    let p = ui.painter();
    p.rect_filled(r, 11, mix(WELL, accent, t * 0.9));
    p.rect_stroke(r, 11, stroke(1.0, if on { accent } else { EDGE }), StrokeKind::Inside);
    let x = r.left() + 11.0 + t * (r.width() - 22.0);
    p.circle_filled(pos2(x, r.center().y), 8.0, if on { Color32::WHITE } else { DIM });
    hover_cursor(resp.on_hover_text(format!("{}: {}", param.name(), if on { "on" } else { "off" })))
}

/// A labelled on/off pill for options inside a module.
fn pill(ui: &mut Ui, setter: &ParamSetter, param: &BoolParam, label: &str, accent: Color32) -> Response {
    let on = param.value();
    let r = chip(ui, label, on, accent);
    if r.clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, !on);
        setter.end_set_parameter(param);
    }
    r.on_hover_text(param.name())
}

fn segmented<T: Enum + PartialEq + 'static>(ui: &mut Ui, setter: &ParamSetter, param: &EnumParam<T>, accent: Color32, width: f32) {
    let names = T::variants();
    let cur = T::to_index(param.value());
    let (rect, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
    ui.painter().rect_filled(rect, 9, WELL);
    let w = (width - 4.0) / names.len() as f32;
    for (i, n) in names.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.left() + 2.0 + i as f32 * w, rect.top() + 2.0), vec2(w, 24.0));
        let resp = hover_cursor(ui.interact(r, ui.id().with((param.name(), i)), Sense::click()));
        if resp.clicked() && i != cur {
            setter.begin_set_parameter(param);
            setter.set_parameter(param, T::from_index(i));
            setter.end_set_parameter(param);
        }
        let p = ui.painter();
        if i == cur {
            p.rect_filled(r, 7, accent);
        } else if resp.hovered() {
            p.rect_filled(r, 7, HOVER);
        }
        let short = n.split(" (").next().unwrap_or(n);
        p.text(r.center(), Align2::CENTER_CENTER, short, semi(11.5), if i == cur { Color32::from_rgb(12, 12, 16) } else { DIM });
    }
}

fn combo<T: Enum + PartialEq + 'static>(ui: &mut Ui, setter: &ParamSetter, param: &EnumParam<T>, width: f32) {
    let cur = T::to_index(param.value());
    let names = T::variants();
    egui::ComboBox::from_id_salt(param.name())
        .width(width)
        .height(320.0)
        .selected_text(egui::RichText::new(names[cur]).font(semi(12.5)))
        .show_ui(ui, |ui| {
            for (i, n) in names.iter().enumerate() {
                if ui.selectable_label(cur == i, *n).clicked() {
                    setter.begin_set_parameter(param);
                    setter.set_parameter(param, T::from_index(i));
                    setter.end_set_parameter(param);
                }
            }
        })
        .response
        .on_hover_text(param.name());
}

fn caption(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(semi(10.5)).color(FAINT));
}

// ---------------------------------------------------------------- header

fn header_bar(ui: &mut Ui, st: &mut EditorState) {
    let r = ui.max_rect();
    let p = ui.painter().clone();
    let sh = st.shared.clone();
    let accent = st.accent;
    let level = sh.in_level.load(Ordering::Relaxed);

    // The orb breathes with the input.
    let c = pos2(r.left() + 18.0, r.center().y);
    let lift = ((gain_to_db(level) + 50.0) / 50.0).clamp(0.0, 1.0);
    glow(&p, c, 15.0, accent, 0.6 + lift * 1.6);
    let colors = st.style_colors();
    gradient_rect(&p, Rect::from_center_size(c, vec2(30.0, 30.0)), 15.0, accent, hex(colors[1]), vec2(1.0, 1.0));
    p.circle_filled(c + vec2(-4.0, -5.0), 4.0, alpha(Color32::WHITE, 0.35));

    let mut x = r.left() + 44.0;
    for ch in "PERSONA".chars() {
        let g = p.layout_no_wrap(ch.to_string(), heavy(19.0), TEXT);
        let w = g.size().x;
        p.galley(pos2(x, r.center().y - g.size().y / 2.0), g, TEXT);
        x += w + 2.6;
    }
    p.text(pos2(x + 10.0, r.center().y + 1.0), Align2::LEFT_CENTER, "rap vocal chains", regular(12.5), FAINT);

    // Right side, laid out from the edge inwards.
    let mut right = r.right();
    let gear_rect = Rect::from_min_size(pos2(right - 32.0, r.center().y - 16.0), vec2(32.0, 32.0));
    let mut g_ui = ui.new_child(egui::UiBuilder::new().max_rect(gear_rect));
    if icon_button(&mut g_ui, 32.0, gear).on_hover_text("Settings and updates").clicked() {
        st.settings_open = !st.settings_open;
    }
    right -= 44.0;

    let badge = Rect::from_min_max(pos2(right - 190.0, r.center().y - 14.0), pos2(right, r.center().y + 14.0));
    let mut b_ui = ui.new_child(egui::UiBuilder::new().max_rect(badge).layout(egui::Layout::right_to_left(egui::Align::Center)));
    update_badge(&mut b_ui, st);
    right -= 200.0;

    let meter = |p: &Painter, x0: f32, label: &str, vals: &[f32]| {
        p.text(pos2(x0, r.center().y), Align2::LEFT_CENTER, label, semi(10.5), FAINT);
        let bx = x0 + 28.0;
        let w = 120.0;
        let n = vals.len() as f32;
        let h = if vals.len() > 1 { 4.0 } else { 6.0 };
        let total = n * h + (n - 1.0) * 3.0;
        for (i, v) in vals.iter().enumerate() {
            let y = r.center().y - total / 2.0 + i as f32 * (h + 3.0);
            let bar = Rect::from_min_size(pos2(bx, y), vec2(w, h));
            p.rect_filled(bar, 2, alpha(Color32::WHITE, 0.07));
            let db = gain_to_db(*v);
            let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            let color = if db > -1.0 {
                HOT
            } else if db > -9.0 {
                WARN
            } else {
                accent
            };
            if t > 0.0 {
                p.rect_filled(Rect::from_min_size(bar.min, vec2(w * t, h)), 2, color);
            }
        }
    };
    let out = [sh.out_level[0].load(Ordering::Relaxed), sh.out_level[1].load(Ordering::Relaxed)];
    meter(&p, right - 150.0, "OUT", &out);
    meter(&p, right - 310.0, "IN", &[level]);
    let latency = sh.latency.load(Ordering::Relaxed) as f32 / sh.sample_rate.load(Ordering::Relaxed).max(1.0) * 1000.0;
    p.text(pos2(right - 330.0, r.center().y), Align2::RIGHT_CENTER, format!("{latency:.0} ms"), regular(11.0), FAINT);
}

fn update_badge(ui: &mut Ui, st: &mut EditorState) {
    let status = st.update.lock().clone();
    let pill = |ui: &mut Ui, text: String, color: Color32| {
        ui.label(egui::RichText::new(text).font(semi(11.5)).color(color));
    };
    match status {
        Status::Available { version, notes } => {
            let mut r = button(ui, &format!("Update to {version}"), vec2(140.0, 26.0), GOOD, Color32::from_rgb(8, 24, 16));
            if !notes.is_empty() {
                r = r.on_hover_text(notes);
            }
            if r.clicked() {
                update::spawn_install(st.update.clone());
            }
        }
        Status::Installing { version } => {
            pill(ui, format!("Installing {version}…"), WARN);
            ui.spinner();
        }
        Status::Installed { version, detail } => {
            ui.label(egui::RichText::new(format!("{version} installed: restart your DAW")).font(semi(11.5)).color(GOOD))
                .on_hover_text(detail);
        }
        Status::Error(e) => {
            ui.label(egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).font(semi(11.5)).color(FAINT))
                .on_hover_text(format!("Couldn't check for updates: {e}"));
        }
        Status::Checking => {
            ui.spinner();
        }
        Status::UpToDate | Status::Idle => pill(ui, concat!("v", env!("CARGO_PKG_VERSION")).to_string(), FAINT),
    }
}

// ---------------------------------------------------------------- style browser

fn sidebar(ui: &mut Ui, setter: &ParamSetter, st: &mut EditorState) {
    let r = ui.max_rect();
    card(ui.painter(), r);
    let accent = st.accent;
    let inner = r.shrink2(vec2(14.0, 14.0));
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Styles").font(semi(16.0)).color(TEXT));
        ui.label(egui::RichText::new(format!("{}", st.entries.len())).font(semi(12.0)).color(FAINT));
    });
    ui.add_space(4.0);

    // Search field.
    let (sr, _) = ui.allocate_exact_size(vec2(inner.width(), 34.0), Sense::hover());
    ui.painter().rect_filled(sr, 10, WELL);
    let lens = pos2(sr.left() + 16.0, sr.center().y);
    ui.painter().circle_stroke(lens - vec2(1.0, 1.0), 4.5, stroke(1.6, FAINT));
    ui.painter().line_segment([lens + vec2(2.5, 2.5), lens + vec2(6.0, 6.0)], stroke(1.8, FAINT));
    let field = Rect::from_min_max(pos2(sr.left() + 30.0, sr.top() + 2.0), pos2(sr.right() - 28.0, sr.bottom() - 2.0));
    let mut f_ui = ui.new_child(egui::UiBuilder::new().max_rect(field).layout(egui::Layout::left_to_right(egui::Align::Center)));
    f_ui.add(
        egui::TextEdit::singleline(&mut st.search)
            .hint_text(egui::RichText::new("Search artists or vibes").color(FAINT))
            .font(regular(13.0))
            .frame(false)
            .desired_width(field.width()),
    );
    if !st.search.is_empty() {
        let x = Rect::from_center_size(pos2(sr.right() - 15.0, sr.center().y), vec2(22.0, 22.0));
        let mut x_ui = ui.new_child(egui::UiBuilder::new().max_rect(x));
        if icon_button(&mut x_ui, 22.0, |p, c, col| {
            p.line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], stroke(1.6, col));
            p.line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], stroke(1.6, col));
        })
        .clicked()
        {
            st.search.clear();
        }
    }
    ui.add_space(8.0);

    // Filter chips.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        let mut pick = |ui: &mut Ui, label: &str, f: Filter| {
            if chip(ui, label, st.filter == f, accent).clicked() {
                st.filter = if st.filter == f && f != Filter::All { Filter::All } else { f };
            }
        };
        pick(ui, "All", Filter::All);
        pick(ui, "Favorites", Filter::Favorites);
        pick(ui, "Mine", Filter::Mine);
        for t in Tag::ALL {
            pick(ui, t.label(), Filter::Tag(t));
        }
    });
    ui.add_space(10.0);

    let list = st.visible();
    let cur = st.current();
    let now = ui.input(|i| i.time);
    let footer_h = 30.0;
    let list_h = (inner.bottom() - ui.cursor().top() - footer_h).max(60.0);
    let mut load: Option<usize> = None;
    let mut fav: Option<String> = None;
    let mut delete: Option<String> = None;
    egui::ScrollArea::vertical().id_salt("styles").max_height(list_h).auto_shrink([false, false]).show(&mut ui, |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        if list.is_empty() {
            ui.add_space(20.0);
            ui.label(egui::RichText::new("Nothing matches. Try another word or filter.").color(FAINT));
        }
        for &i in &list {
            let e = &st.entries[i];
            let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 6.0, 54.0), Sense::click());
            let selected = cur == Some(i);
            let p = ui.painter();
            if selected {
                p.rect_filled(row, 12, RAISED);
                p.rect_filled(Rect::from_min_size(row.min + vec2(0.0, 14.0), vec2(3.0, 26.0)), 2, accent);
            } else if resp.hovered() {
                p.rect_filled(row, 12, alpha(Color32::WHITE, 0.035));
            }
            let sw = Rect::from_min_size(row.min + vec2(10.0, 8.0), vec2(38.0, 38.0));
            gradient_rect(p, sw, 11.0, hex(e.colors[0]), hex(e.colors[1]), vec2(1.0, 1.4));
            p.text(sw.center(), Align2::CENTER_CENTER, initials(&e.name), heavy(14.0), alpha(Color32::WHITE, 0.92));
            let tx = sw.right() + 12.0;
            p.text(pos2(tx, row.top() + 18.0), Align2::LEFT_CENTER, &e.name, semi(13.5), if selected { TEXT } else { mix(TEXT, DIM, 0.25) });
            let sub = if e.user() {
                let base = e.tags.iter().take(2).map(|t| t.label()).collect::<Vec<_>>().join(" · ");
                if base.is_empty() { "Mine".to_string() } else { format!("Mine · {base}") }
            } else {
                e.tags.iter().take(3).map(|t| t.label()).collect::<Vec<_>>().join(" · ")
            };
            p.text(pos2(tx, row.top() + 36.0), Align2::LEFT_CENTER, sub, regular(11.0), FAINT);

            let is_fav = st.favorites.contains(&e.name);
            let star_c = pos2(row.right() - 18.0, row.center().y);
            let star_rect = Rect::from_center_size(star_c, vec2(24.0, 24.0));
            let star_resp = hover_cursor(ui.interact(star_rect, ui.id().with(("fav", i)), Sense::click()));
            if is_fav || resp.hovered() || star_resp.hovered() {
                star(ui.painter(), star_c, 6.5, is_fav, if is_fav { WARN } else if star_resp.hovered() { TEXT } else { FAINT });
            }
            if star_resp.clicked() {
                fav = Some(e.name.clone());
            }
            if e.user() && (resp.hovered() || star_resp.hovered() || st.confirm_delete.as_ref().is_some_and(|(n, _)| *n == e.name)) {
                let del_c = pos2(row.right() - 44.0, row.center().y);
                let del_rect = Rect::from_center_size(del_c, vec2(24.0, 24.0));
                let del_resp = hover_cursor(ui.interact(del_rect, ui.id().with(("del", i)), Sense::click()));
                let armed = st.confirm_delete.as_ref().is_some_and(|(n, t)| *n == e.name && now < *t);
                let col = if armed { HOT } else if del_resp.hovered() { TEXT } else { FAINT };
                let p = ui.painter();
                if armed {
                    p.rect_filled(del_rect, 6, alpha(HOT, 0.18));
                }
                p.line_segment([del_c + vec2(-4.0, -4.0), del_c + vec2(4.0, 4.0)], stroke(1.6, col));
                p.line_segment([del_c + vec2(-4.0, 4.0), del_c + vec2(4.0, -4.0)], stroke(1.6, col));
                let del_resp = del_resp.on_hover_text(if armed { "Click again to delete" } else { "Delete this preset" });
                if del_resp.clicked() {
                    if armed {
                        delete = Some(e.name.clone());
                    } else {
                        st.confirm_delete = Some((e.name.clone(), now + 3.0));
                    }
                }
                continue;
            }
            if hover_cursor(resp).on_hover_text(&e.blurb).clicked() {
                load = Some(i);
            }
        }
    });
    if let Some(i) = load {
        st.load(setter, i);
    }
    if let Some(name) = fav {
        st.toggle_favorite(&name);
    }
    if let Some(name) = delete {
        st.confirm_delete = None;
        st.delete_user(&name);
        st.toast(ui.ctx(), format!("Deleted {name}"));
    }
    let foot = Rect::from_min_max(pos2(inner.left(), inner.bottom() - footer_h + 8.0), inner.max);
    let g = ui.painter().layout(
        "Styles are our take on the sound of each artist's records. Not affiliated with or endorsed by them.".to_string(),
        regular(10.0),
        FAINT,
        foot.width(),
    );
    ui.painter().galley(foot.min, g, FAINT);
}

// ---------------------------------------------------------------- hero

fn hero_card(ui: &mut Ui, setter: &ParamSetter, st: &mut EditorState) {
    let r = ui.max_rect();
    let cur = st.current();
    let (name, blurb, tags, colors, user) = match cur {
        Some(i) => {
            let e = &st.entries[i];
            (e.name.clone(), e.blurb.clone(), e.tags.clone(), e.colors, e.user())
        }
        None => (st.params.style.read().clone(), "A preset that isn't in this browser.".to_string(), Vec::new(), [0x4B5568, 0x1A1E2A], true),
    };
    let p = ui.painter().clone();
    gradient_rect(&p, r, 18.0, hex(colors[0]), hex(colors[1]), vec2(1.0, 0.55));
    // A scrim on the left keeps white text readable on light colors.
    gradient_rect(&p, r, 18.0, Color32::from_rgba_unmultiplied(8, 9, 14, 175), Color32::from_rgba_unmultiplied(8, 9, 14, 20), vec2(1.0, 0.0));
    let clip = p.with_clip_rect(r.shrink(1.0));
    clip.text(pos2(r.right() - 26.0, r.bottom() + 30.0), Align2::RIGHT_BOTTOM, initials(&name), heavy(150.0), alpha(Color32::WHITE, 0.06));

    let x = r.left() + 24.0;
    let fitted = st.params.voice.read().learned && !user;
    let kicker = match (user, fitted) {
        (true, _) => "YOUR PRESET",
        (false, true) => "STYLE  ·  FITTED TO YOUR VOICE",
        (false, false) => "STYLE",
    };
    p.text(pos2(x, r.top() + 22.0), Align2::LEFT_CENTER, kicker, semi(10.5), alpha(Color32::WHITE, 0.62));
    let name_rect = p.text(pos2(x - 1.0, r.top() + 47.0), Align2::LEFT_CENTER, &name, heavy(30.0), Color32::WHITE);
    let g = p.layout(blurb, regular(13.0), alpha(Color32::WHITE, 0.86), 540.0);
    p.galley(pos2(x, name_rect.bottom() + 1.0), g, Color32::WHITE);
    let mut tx = x;
    let ty = r.bottom() - 21.0;
    for t in tags.iter().take(4) {
        let g = p.layout_no_wrap(t.label().to_string(), semi(10.5), Color32::WHITE);
        let w = g.size().x + 16.0;
        let tr = Rect::from_min_size(pos2(tx, ty - 10.0), vec2(w, 20.0));
        p.rect_filled(tr, 10, alpha(Color32::WHITE, 0.14));
        p.galley(tr.center() - g.size() / 2.0, g, Color32::WHITE);
        tx += w + 6.0;
    }

    // Controls, top right.
    let bar = Rect::from_min_size(pos2(r.right() - 238.0, r.top() + 14.0), vec2(224.0, 32.0));
    let mut b = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::right_to_left(egui::Align::Center)));
    b.spacing_mut().item_spacing.x = 4.0;
    if button(&mut b, "Save as…", vec2(84.0, 30.0), alpha(Color32::BLACK, 0.35), Color32::WHITE).on_hover_text("Save the current settings as your own preset").clicked() {
        st.save_name = if user { name.clone() } else { format!("{name} (mine)") };
        st.save_open = true;
    }
    if icon_button(&mut b, 30.0, |p, c, _| chevron(p, c, false, Color32::WHITE)).on_hover_text("Next style").clicked() {
        st.step(setter, 1);
    }
    if icon_button(&mut b, 30.0, |p, c, _| chevron(p, c, true, Color32::WHITE)).on_hover_text("Previous style").clicked() {
        st.step(setter, -1);
    }
    let is_fav = st.favorites.contains(&name);
    if icon_button(&mut b, 30.0, |p, c, _| star(p, c, 7.0, is_fav, if is_fav { WARN } else { Color32::WHITE }))
        .on_hover_text(if is_fav { "Remove from favorites" } else { "Add to favorites" })
        .clicked()
    {
        st.toggle_favorite(&name);
    }

    if st.edited() {
        let er = Rect::from_min_size(pos2(r.right() - 186.0, r.bottom() - 42.0), vec2(172.0, 28.0));
        let mut e_ui = ui.new_child(egui::UiBuilder::new().max_rect(er).layout(egui::Layout::right_to_left(egui::Align::Center)));
        e_ui.spacing_mut().item_spacing.x = 6.0;
        if button(&mut e_ui, "Revert", vec2(70.0, 26.0), Color32::WHITE, Color32::from_rgb(12, 12, 16))
            .on_hover_text("Back to the style as designed")
            .clicked()
        {
            if let Some(i) = cur {
                st.load(setter, i);
            }
        }
        e_ui.label(egui::RichText::new("Edited").font(semi(11.5)).color(alpha(Color32::WHITE, 0.8)));
    }
}

// ---------------------------------------------------------------- vibe knobs

fn vibe_panel(ui: &mut Ui, setter: &ParamSetter, st: &mut EditorState) {
    let r = ui.max_rect();
    card(ui.painter(), r);
    let p = ui.painter().clone();
    let accent = st.accent;
    p.text(r.left_top() + vec2(18.0, 20.0), Align2::LEFT_CENTER, "Vibe", semi(14.0), TEXT);
    p.text(r.left_top() + vec2(58.0, 20.5), Align2::LEFT_CENTER, "50% is the style as designed", regular(11.5), FAINT);
    let params = st.params.clone();
    let v = &params.vibe;
    let s = Settings::from_params(&params, 120.0, 0);
    let knobs: [(&FloatParam, &str, &str, bool); 6] = [
        (&v.tune, "Tune", if s.tune_on { "snap" } else { "tune is off" }, s.tune_on),
        (&v.punch, "Punch", "squeeze", true),
        (&v.grit, "Grit", "heat", true),
        (&v.air, "Air", "shine", true),
        (&v.space, "Space", "depth", true),
        (&v.width, "Width", "spread", true),
    ];
    let slot = (r.width() - 24.0) / 6.0;
    for (i, (param, label, hint, live)) in knobs.into_iter().enumerate() {
        let cx = r.left() + 12.0 + slot * (i as f32 + 0.5);
        let area = Rect::from_center_size(pos2(cx, r.top() + 82.0), vec2(slot - 6.0, 96.0));
        let resp = hover_cursor(ui.interact(area, ui.id().with(("vibe", i)), Sense::click_and_drag()));
        drag_param(ui, setter, param, &resp, 220.0);
        let active = resp.hovered() || resp.dragged();
        let c = pos2(cx, r.top() + 78.0);
        let col = if live { accent } else { FAINT };
        if active && live {
            glow(&p, c, 30.0, accent, 0.8);
        }
        for k in 0..=10 {
            let a = PI * 0.75 + PI * 1.5 * k as f32 / 10.0;
            let (r0, r1) = if k == 5 { (35.0, 40.0) } else { (36.0, 38.5) };
            p.line_segment([c + vec2(a.cos(), a.sin()) * r0, c + vec2(a.cos(), a.sin()) * r1], stroke(1.2, if k == 5 { DIM } else { FAINT }));
        }
        knob_face(&p, c, 32.0, param.modulated_normalized_value(), 0.5, col, active, 5.0);
        let pct = (param.modulated_normalized_value() * 100.0).round() as i32;
        p.text(c, Align2::CENTER_CENTER, format!("{pct}"), semi(15.0), if live { TEXT } else { DIM });
        p.text(pos2(cx, r.top() + 126.0), Align2::CENTER_CENTER, label, semi(13.0), if live { TEXT } else { DIM });
        p.text(pos2(cx, r.top() + 144.0), Align2::CENTER_CENTER, hint, regular(10.5), FAINT);
        resp.on_hover_text(format!("{}: {}%\nDrag, scroll, double-click = back to 50%", param.name(), pct));
    }
}

// ---------------------------------------------------------------- Match My Voice

fn match_panel(ui: &mut Ui, setter: &ParamSetter, st: &mut EditorState) {
    let r = ui.max_rect();
    card(ui.painter(), r);
    let accent = st.accent;
    let p = ui.painter().clone();
    let state = st.shared.learn_state.load(Ordering::Relaxed);
    let progress = st.shared.learn_progress.load(Ordering::Relaxed);
    let learned = st.params.voice.read().learned;

    // Title with a little waveform glyph.
    let g = pos2(r.left() + 24.0, r.top() + 21.0);
    for (k, h) in [4.0, 9.0, 13.0, 7.0, 11.0, 5.0].iter().enumerate() {
        let x = g.x - 8.0 + k as f32 * 3.4;
        p.line_segment([pos2(x, g.y - h / 2.0), pos2(x, g.y + h / 2.0)], stroke(2.0, accent));
    }
    p.text(pos2(r.left() + 42.0, r.top() + 21.0), Align2::LEFT_CENTER, "Match my voice", semi(14.0), TEXT);
    if learned && !st.learn_pending {
        let b = Rect::from_min_size(pos2(r.right() - 74.0, r.top() + 11.0), vec2(60.0, 20.0));
        p.rect_filled(b, 10, alpha(GOOD, 0.16));
        p.text(b.center(), Align2::CENTER_CENTER, "Fitted", semi(10.5), GOOD);
    }

    let body = Rect::from_min_max(pos2(r.left() + 18.0, r.top() + 40.0), pos2(r.right() - 18.0, r.bottom() - 14.0));
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
    ui.spacing_mut().item_spacing.y = 3.0;
    let line = |ui: &mut Ui, text: &str, color: Color32| {
        ui.add(egui::Label::new(egui::RichText::new(text).font(regular(11.5)).color(color)).wrap());
    };

    if st.learn_pending {
        let listening = state == LEARN_LISTENING;
        let (bar, _) = ui.allocate_exact_size(vec2(body.width(), 8.0), Sense::hover());
        ui.painter().rect_filled(bar, 4, WELL);
        if listening {
            ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * progress.max(0.02), 8.0)), 4, accent);
        } else {
            // Waiting for audio: a sliding highlight.
            let t = (ui.input(|i| i.time) as f32 * 0.8).fract();
            let seg = Rect::from_min_size(pos2(bar.left() + (bar.width() - 60.0) * t, bar.top()), vec2(60.0, 8.0));
            ui.painter().rect_filled(seg, 4, alpha(accent, 0.6));
        }
        ui.add_space(6.0);
        if listening {
            ui.label(egui::RichText::new(format!("Listening… {:.0}%", progress * 100.0)).font(semi(13.0)).color(TEXT));
            line(&mut ui, "Keep the vocal going: only the parts with voice count.", DIM);
        } else {
            ui.label(egui::RichText::new("Waiting for audio…").font(semi(13.0)).color(TEXT));
            line(&mut ui, "Press play on your vocal, or sing or rap into the mic with monitoring on.", DIM);
        }
        ui.add_space(6.0);
        if button(&mut ui, "Cancel", vec2(84.0, 28.0), RAISED, TEXT).clicked() {
            st.cancel_learn();
        }
        return;
    }

    if st.learn_failed {
        line(&mut ui, "Didn't hear enough voice in a minute. Play a part with more vocal in it and try again.", WARN);
        ui.add_space(8.0);
        if button(&mut ui, "Try again", vec2(100.0, 30.0), accent, Color32::from_rgb(12, 12, 16)).clicked() {
            st.start_learn();
        }
        return;
    }

    if !st.learn_summary.is_empty() {
        let lines = st.learn_summary.clone();
        for l in lines.iter().take(5) {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("✓").font(semi(11.0)).color(GOOD));
                ui.add(egui::Label::new(egui::RichText::new(l).font(regular(11.0)).color(DIM)).truncate()).on_hover_text(l);
            });
        }
    } else {
        line(
            &mut ui,
            "Play or sing about 8 seconds of your vocal. Persona sets your input level, de-esser, voice range, key and an EQ for your mic, then fits every style to you.",
            DIM,
        );
    }

    let row = Rect::from_min_size(pos2(body.left(), body.bottom() - 30.0), vec2(body.width(), 30.0));
    let mut b = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
    b.spacing_mut().item_spacing.x = 6.0;
    let label = if learned || !st.learn_summary.is_empty() { "Listen again" } else { "Listen" };
    if button(&mut b, label, vec2(110.0, 30.0), accent, Color32::from_rgb(12, 12, 16)).clicked() {
        st.start_learn();
    }
    if st.undo.is_some() {
        if button(&mut b, "Undo", vec2(62.0, 30.0), RAISED, TEXT).on_hover_text("Put back the settings from before").clicked() {
            st.undo_match(setter);
        }
    } else if learned && button(&mut b, "Forget", vec2(68.0, 30.0), RAISED, DIM).on_hover_text("Clear what Match learned").clicked() {
        st.forget_voice(setter);
    }
}

// ---------------------------------------------------------------- chain strip

fn chain_strip(ui: &mut Ui, setter: &ParamSetter, st: &mut EditorState) {
    let r = ui.max_rect();
    let params = st.params.clone();
    let s = Settings::from_params(&params, 120.0, 0);
    let e = Effective::new(&s);
    let sh = st.shared.clone();
    let accent = st.accent;
    let ld = |a: &AtomicF32| a.load(Ordering::Relaxed);
    let gap = 5.0;
    let w = (r.width() - gap * (MODULES.len() as f32 - 1.0)) / MODULES.len() as f32;
    for (i, m) in MODULES.iter().copied().enumerate() {
        let tile = Rect::from_min_size(pos2(r.left() + i as f32 * (w + gap), r.top()), vec2(w, r.height()));
        let resp = hover_cursor(ui.interact(tile, ui.id().with(("tile", i)), Sense::click()));
        let selected = st.module == m;
        let led = module_led(&s, &e, m);
        let p = ui.painter();
        p.rect_filled(tile, 12, if selected { RAISED } else if resp.hovered() { mix(SURFACE, RAISED, 0.6) } else { SURFACE });
        p.rect_stroke(tile, 12, stroke(if selected { 1.5 } else { 1.0 }, if selected { accent } else { alpha(EDGE, 0.7) }), StrokeKind::Inside);
        let dot = pos2(tile.center().x, tile.top() + 14.0);
        match led {
            Led::On => {
                glow(p, dot, 4.0, accent, 1.2);
                p.circle_filled(dot, 3.5, accent);
            }
            Led::Macro => {
                p.circle_stroke(dot, 3.2, stroke(1.5, accent));
            }
            Led::Off => {
                p.circle_filled(dot, 3.0, FAINT);
            }
        }
        p.text(pos2(tile.center().x, tile.top() + 32.0), Align2::CENTER_CENTER, m.label(), semi(11.5), if led == Led::Off && !selected { DIM } else { TEXT });

        let level = match m {
            Module::Gate => ld(&sh.gate_gr) / 20.0,
            Module::Tune => ld(&sh.pitch_correction).abs(),
            Module::Clean => (s.mud_gain.abs() + s.harsh_gain.abs()) / 12.0 + if s.hp_on { 0.15 } else { 0.0 },
            Module::Match => s.match_amount,
            Module::DeEss => ld(&sh.deess_gr) / 10.0,
            Module::Punch => ld(&sh.punch_gr) / 12.0,
            Module::Level => ld(&sh.level_gr) / 10.0,
            Module::Drive => e.sat.map(|(_, d, _)| d).unwrap_or(0.0),
            Module::Tone => (s.low_gain.abs() + e.pres_gain.abs() + e.air_gain.abs()) / 14.0,
            Module::Double => e.dbl_mix,
            Module::Delay | Module::Reverb => ((gain_to_db(ld(&sh.space_level)) + 54.0) / 42.0).max(0.0) * if led == Led::Off { 0.0 } else { 1.0 },
            Module::Output => ((gain_to_db(ld(&sh.out_level[0]).max(ld(&sh.out_level[1]))) + 60.0) / 60.0).max(0.0),
        }
        .clamp(0.0, 1.0);
        let bar = Rect::from_min_size(pos2(tile.left() + 10.0, tile.bottom() - 14.0), vec2(w - 20.0, 4.0));
        p.rect_filled(bar, 2, WELL);
        if level > 0.01 && led != Led::Off {
            let is_gr = matches!(m, Module::Gate | Module::DeEss | Module::Punch | Module::Level);
            p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * level, 4.0)), 2, if is_gr { mix(accent, WARN, 0.5) } else { accent });
        }
        let tip = match (led, module_power(&params, m)) {
            (Led::Macro, _) => format!("{}: off in this style, faded in by a vibe knob. Click to edit.", m.title()),
            (_, Some(_)) => format!("{}. Click to edit, double-click to switch on or off.", m.title()),
            _ => format!("{}. Click to edit.", m.title()),
        };
        let resp = resp.on_hover_text(tip);
        if resp.double_clicked() {
            if let Some(pw) = module_power(&params, m) {
                setter.begin_set_parameter(pw);
                setter.set_parameter(pw, !pw.value());
                setter.end_set_parameter(pw);
            }
        } else if resp.clicked() {
            st.module = m;
        }
    }
}

// ---------------------------------------------------------------- module editor

fn module_panel(ui: &mut Ui, setter: &ParamSetter, st: &mut EditorState) {
    let r = ui.max_rect();
    card(ui.painter(), r);
    let accent = st.accent;
    let m = st.module;
    let params = st.params.clone();
    let p = ui.painter().clone();
    p.text(r.left_top() + vec2(20.0, 24.0), Align2::LEFT_CENTER, m.title(), semi(16.0), TEXT);
    let tw = p.layout_no_wrap(m.title().to_string(), semi(16.0), TEXT).size().x;
    p.text(r.left_top() + vec2(32.0 + tw, 24.5), Align2::LEFT_CENTER, m.blurb(), regular(12.0), FAINT);
    if let Some(pw) = module_power(&params, m) {
        let sw = Rect::from_min_size(pos2(r.right() - 58.0, r.top() + 13.0), vec2(38.0, 22.0));
        let mut s_ui = ui.new_child(egui::UiBuilder::new().max_rect(sw));
        switch(&mut s_ui, setter, pw, accent);
    }

    let body = Rect::from_min_max(pos2(r.left() + 16.0, r.top() + 50.0), pos2(r.right() - 16.0, r.bottom() - 14.0));
    let s = Settings::from_params(&params, st.shared.bpm.load(Ordering::Relaxed), 0);
    let e = Effective::new(&s);
    let enabled = module_power(&params, m).map(|b| b.value()).unwrap_or(true) || module_led(&s, &e, m) == Led::Macro;
    let visual = Rect::from_min_max(pos2(body.right() - 360.0, body.top()), body.max);
    let controls = Rect::from_min_max(body.min, pos2(visual.left() - 16.0, body.bottom()));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(controls).layout(egui::Layout::top_down(egui::Align::Min)));
    if !enabled {
        c.multiply_opacity(0.5);
    }
    c.spacing_mut().item_spacing = vec2(4.0, 6.0);
    let vp = ui.painter().with_clip_rect(visual);
    vp.rect_filled(visual, 12, WELL);
    let fs = st.shared.sample_rate.load(Ordering::Relaxed).max(8000.0);

    match m {
        Module::Gate => {
            let g = &params.gate;
            c.horizontal(|ui| {
                knob(ui, setter, &g.thresh, "Threshold", accent, false);
                knob(ui, setter, &g.range, "Range", accent, false);
            });
            caption(&mut c, "Raise the threshold until the noise between lines drops out.");
            history_view(&vp, visual, &st.history, 0, 30.0, accent, "gain reduction");
        }
        Module::Tune => {
            let t = &params.tune;
            c.horizontal(|ui| {
                ui.vertical(|ui| {
                    caption(ui, "KEY");
                    combo(ui, setter, &t.key, 64.0);
                });
                ui.vertical(|ui| {
                    caption(ui, "SCALE");
                    combo(ui, setter, &t.scale, 130.0);
                });
                ui.vertical(|ui| {
                    caption(ui, "VOICE");
                    combo(ui, setter, &t.voice, 150.0);
                });
            });
            let sh = &st.shared;
            if sh.key_valid.load(Ordering::Relaxed) {
                let root = sh.key_root.load(Ordering::Relaxed);
                let minor = sh.key_minor.load(Ordering::Relaxed);
                let name = format!("{} {}", NOTE_NAMES[root.rem_euclid(12) as usize], if minor { "minor" } else { "major" });
                if chip(&mut c, &format!("Heard: {name}  ·  use it"), false, accent).on_hover_text("Set Key and Scale to what the tuner hears").clicked() {
                    let scale = if minor { 2.0 } else { 1.0 };
                    st.set_plain(setter, &[("t_key", root as f32), ("t_scale", scale)]);
                }
            } else {
                caption(&mut c, "Sing a few lines and Persona will suggest the key.");
            }
            c.horizontal(|ui| {
                knob(ui, setter, &t.speed, "Speed", accent, false);
                knob(ui, setter, &t.amount, "Amount", accent, false);
                knob(ui, setter, &t.humanize, "Humanize", accent, false);
                knob(ui, setter, &t.flex, "Flex", accent, false);
                knob(ui, setter, &t.guard, "Guard", accent, false);
            });
            pitch_view(&vp, visual, &st.history, s.tune_root, s.tune_mask, accent);
        }
        Module::Clean => {
            let cl = &params.clean;
            c.horizontal(|ui| {
                pill(ui, setter, &cl.hp_on, "High-pass", accent);
            });
            c.horizontal(|ui| {
                knob(ui, setter, &cl.hp_freq, "Low cut", accent, false);
                knob(ui, setter, &cl.mud_gain, "Mud", accent, true);
                knob(ui, setter, &cl.mud_freq, "Mud freq", accent, false);
                knob(ui, setter, &cl.harsh_gain, "Harsh", accent, true);
                knob(ui, setter, &cl.harsh_freq, "Harsh freq", accent, false);
            });
            let mine: Vec<Biquad> = {
                let mut v = Vec::new();
                if s.hp_on {
                    v.push(Biquad::new(Kind::HighPass, fs, s.hp_freq, 0.707, 0.0));
                    v.push(Biquad::new(Kind::HighPass, fs, s.hp_freq, 0.707, 0.0));
                }
                v.push(Biquad::new(Kind::Peak, fs, s.mud_freq, 1.2, s.mud_gain));
                v.push(Biquad::new(Kind::Peak, fs, s.harsh_freq, 2.0, s.harsh_gain));
                v
            };
            eq_view(&vp, visual, &eq_filters(&s, fs), &mine, fs, accent);
        }
        Module::Match => {
            let mp = &params.matched;
            c.horizontal(|ui| {
                knob(ui, setter, &mp.amount, "Amount", accent, false);
                ui.add_space(6.0);
                for (i, b) in mp.bands().into_iter().enumerate() {
                    let hz = MATCH_HZ[i];
                    let label = if hz >= 1000.0 { format!("{}k", hz / 1000.0) } else { format!("{hz:.0}") };
                    fader(ui, setter, b, &label, accent, 150.0);
                }
            });
            caption(&mut c, "Set by Match my voice. Amount scales the whole curve; 0% bypasses it.");
            let mine: Vec<Biquad> = MATCH_HZ.iter().enumerate().map(|(i, hz)| Biquad::new(Kind::Peak, fs, *hz, 1.1, s.match_db[i] * s.match_amount)).collect();
            eq_view(&vp, visual, &eq_filters(&s, fs), &mine, fs, accent);
        }
        Module::DeEss => {
            let d = &params.deess;
            c.horizontal(|ui| {
                knob(ui, setter, &d.freq, "Frequency", accent, false);
                knob(ui, setter, &d.thresh, "Threshold", accent, false);
                knob(ui, setter, &d.range, "Range", accent, false);
            });
            caption(&mut c, "Lower the threshold until the S sounds stop jumping out. 3-6 dB is plenty.");
            history_view(&vp, visual, &st.history, 1, 12.0, accent, "S reduction");
        }
        Module::Punch => {
            let k = &params.punch;
            c.horizontal(|ui| {
                knob(ui, setter, &k.thresh, "Threshold", accent, false);
                knob(ui, setter, &k.ratio, "Ratio", accent, false);
                knob(ui, setter, &k.attack, "Attack", accent, false);
                knob(ui, setter, &k.release, "Release", accent, false);
            });
            c.horizontal(|ui| {
                knob(ui, setter, &k.makeup, "Makeup", accent, true);
                knob(ui, setter, &k.color, "Color", accent, false);
            });
            history_view(&vp, visual, &st.history, 2, 18.0, accent, "gain reduction");
        }
        Module::Level => {
            let l = &params.level;
            c.horizontal(|ui| {
                knob(ui, setter, &l.amount, "Amount", accent, false);
                knob(ui, setter, &l.makeup, "Makeup", accent, true);
            });
            caption(&mut c, "Works after Punch: smooths phrase to phrase instead of word to word.");
            history_view(&vp, visual, &st.history, 3, 12.0, accent, "gain reduction");
        }
        Module::Drive => {
            let d = &params.sat;
            segmented(&mut c, setter, &d.mode, accent, controls.width().min(420.0));
            c.add_space(4.0);
            c.horizontal(|ui| {
                knob(ui, setter, &d.drive, "Drive", accent, false);
                knob(ui, setter, &d.mix, "Mix", accent, false);
            });
            if !s.sat_on {
                caption(&mut c, if e.sat.is_some() { "Off in this style: the Grit knob is fading it in." } else { "Off. Switch it on, or turn Grit past 50%." });
            }
            let (mode, drive, mx) = e.sat.unwrap_or((s.sat_mode, s.sat_drive, s.sat_mix));
            drive_view(&vp, visual, mode, drive, mx, accent);
        }
        Module::Tone => {
            let t = &params.tone;
            c.horizontal(|ui| {
                knob(ui, setter, &t.low_gain, "Body", accent, true);
                knob(ui, setter, &t.low_freq, "Body freq", accent, false);
                knob(ui, setter, &t.pres_gain, "Presence", accent, true);
                knob(ui, setter, &t.pres_freq, "Pres freq", accent, false);
            });
            c.horizontal(|ui| {
                knob(ui, setter, &t.air_gain, "Air", accent, true);
                knob(ui, setter, &t.air_freq, "Air freq", accent, false);
            });
            let mine = vec![
                Biquad::new(Kind::LowShelf, fs, s.low_freq, 0.7, s.low_gain),
                Biquad::new(Kind::Peak, fs, s.pres_freq, 0.9, e.pres_gain),
                Biquad::new(Kind::HighShelf, fs, s.air_freq, 0.7, e.air_gain),
            ];
            eq_view(&vp, visual, &eq_filters(&s, fs), &mine, fs, accent);
        }
        Module::Double => {
            let d = &params.doubler;
            c.horizontal(|ui| {
                knob(ui, setter, &d.mix, "Mix", accent, false);
                knob(ui, setter, &d.detune, "Detune", accent, false);
                knob(ui, setter, &d.delay, "Delay", accent, false);
                knob(ui, setter, &d.width, "Width", accent, false);
            });
            if !s.dbl_on {
                caption(&mut c, if e.dbl_mix > 0.0 { "Off in this style: the Width knob is fading it in." } else { "Off. Switch it on, or turn Width past 50%." });
            }
            doubler_view(&vp, visual, e.dbl_mix, e.dbl.width, s.dbl_detune, accent);
        }
        Module::Delay => {
            let d = &params.delay;
            c.horizontal(|ui| {
                ui.vertical(|ui| {
                    caption(ui, "TIME");
                    combo(ui, setter, &d.division, 80.0);
                });
                ui.vertical(|ui| {
                    caption(ui, "MODE");
                    pill(ui, setter, &d.ping_pong, "Ping-pong", accent);
                });
            });
            c.horizontal(|ui| {
                knob(ui, setter, &d.mix, "Mix", accent, false);
                knob(ui, setter, &d.feedback, "Feedback", accent, false);
                knob(ui, setter, &d.duck, "Ducking", accent, false);
                knob(ui, setter, &d.hp, "Low cut", accent, false);
                knob(ui, setter, &d.lp, "High cut", accent, false);
            });
            delay_view(&vp, visual, s.dly_beats, s.bpm, s.dly_feedback, s.dly_ping_pong, accent);
        }
        Module::Reverb => {
            let v = &params.reverb;
            segmented(&mut c, setter, &v.kind, accent, controls.width().min(340.0));
            c.add_space(4.0);
            c.horizontal(|ui| {
                knob(ui, setter, &v.mix, "Mix", accent, false);
                knob(ui, setter, &v.decay, "Decay", accent, false);
                knob(ui, setter, &v.size, "Size", accent, false);
                knob(ui, setter, &v.predelay, "Pre-delay", accent, false);
            });
            c.horizontal(|ui| {
                knob(ui, setter, &v.damping, "Damping", accent, false);
                knob(ui, setter, &v.lowcut, "Low cut", accent, false);
                knob(ui, setter, &v.duck, "Ducking", accent, false);
            });
            if !s.rev_on && e.rev_mix > 0.0 {
                caption(&mut c, "Off in this style: the Space knob is fading it in.");
            }
            reverb_view(&vp, visual, s.rev_predelay, s.rev_decay, s.rev_size, if s.rev_on { e.rev_mix } else { e.rev_mix.max(s.rev_mix) }, accent);
        }
        Module::Output => {
            c.horizontal(|ui| {
                knob(ui, setter, &params.input, "Input", accent, true);
                knob(ui, setter, &params.output, "Output", accent, true);
                knob(ui, setter, &params.mix, "Mix", accent, false);
                knob(ui, setter, &params.trim, "Trim", accent, true)
                    .on_hover_text("Style trim: set by each style so they all come out about as loud as they go in");
                knob(ui, setter, &params.ceiling, "Ceiling", accent, false);
            });
            c.horizontal(|ui| {
                pill(ui, setter, &params.limiter, "Limiter", accent);
            });
            caption(&mut c, "Aim for the IN meter to peak around the yellow. Match my voice sets Input for you.");
            output_view(&vp, visual, &st.shared, &st.history, accent);
        }
    }
}

// ---------------------------------------------------------------- module visuals

fn grid_label(p: &Painter, pos: Pos2, text: &str, align: Align2) {
    p.text(pos, align, text, regular(9.5), FAINT);
}

fn eq_view(p: &Painter, r: Rect, all: &[Biquad], mine: &[Biquad], fs: f32, accent: Color32) {
    let inner = r.shrink2(vec2(14.0, 12.0));
    let (lo, hi) = (20f32.ln(), 20000f32.min(fs * 0.49).ln());
    let x_of = |hz: f32| inner.left() + (hz.ln() - lo) / (hi - lo) * inner.width();
    let range = 15.0;
    let y_of = |db: f32| inner.center().y - db.clamp(-range * 1.4, range * 1.4) / range * inner.height() * 0.5;
    for (hz, label) in [(50.0, "50"), (100.0, "100"), (200.0, ""), (500.0, "500"), (1000.0, "1k"), (2000.0, ""), (5000.0, "5k"), (10000.0, "10k")] {
        let x = x_of(hz);
        p.line_segment([pos2(x, inner.top()), pos2(x, inner.bottom())], stroke(1.0, alpha(Color32::WHITE, 0.04)));
        if !label.is_empty() {
            grid_label(p, pos2(x + 3.0, inner.bottom() - 2.0), label, Align2::LEFT_BOTTOM);
        }
    }
    for db in [-12.0, -6.0, 6.0, 12.0] {
        let y = y_of(db);
        p.line_segment([pos2(inner.left(), y), pos2(inner.right(), y)], stroke(1.0, alpha(Color32::WHITE, 0.04)));
        grid_label(p, pos2(inner.left() + 2.0, y - 1.0), &format!("{db:+.0}"), Align2::LEFT_BOTTOM);
    }
    p.line_segment([pos2(inner.left(), y_of(0.0)), pos2(inner.right(), y_of(0.0))], stroke(1.0, alpha(Color32::WHITE, 0.12)));
    let n = 180;
    let curve = |filters: &[Biquad]| -> Vec<Pos2> {
        (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                let hz = (lo + (hi - lo) * t).exp();
                let db: f32 = filters.iter().map(|b| b.response_db(fs, hz)).sum();
                pos2(inner.left() + t * inner.width(), y_of(db))
            })
            .collect()
    };
    let total = curve(all);
    let zero = y_of(0.0);
    let mut mesh = Mesh::default();
    for (i, q) in total.iter().enumerate() {
        mesh.colored_vertex(*q, alpha(accent, 0.16));
        mesh.colored_vertex(pos2(q.x, zero), alpha(accent, 0.02));
        if i > 0 {
            let k = (i as u32) * 2;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    p.add(Shape::mesh(mesh));
    p.add(Shape::line(curve(mine), stroke(1.5, alpha(Color32::WHITE, 0.55))));
    p.add(Shape::line(total, stroke(2.2, accent)));
    grid_label(p, pos2(inner.right(), inner.top()), "whole tone  ·  this module in white", Align2::RIGHT_TOP);
}

fn history_view(p: &Painter, r: Rect, h: &VecDeque<Frame>, slot: usize, max_db: f32, accent: Color32, label: &str) {
    let inner = r.shrink2(vec2(14.0, 14.0));
    for k in 1..3 {
        let t = k as f32 / 3.0;
        let y = inner.top() + t * inner.height();
        p.line_segment([pos2(inner.left(), y), pos2(inner.right(), y)], stroke(1.0, alpha(Color32::WHITE, 0.04)));
        grid_label(p, pos2(inner.left() + 2.0, y - 1.0), &format!("-{:.0} dB", max_db * t), Align2::LEFT_BOTTOM);
    }
    let n = HISTORY as f32;
    let pts: Vec<Pos2> = h
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let x = inner.right() - (h.len() - 1 - i) as f32 / (n - 1.0) * inner.width();
            pos2(x, inner.top() + (f.gr[slot] / max_db).clamp(0.0, 1.0) * inner.height())
        })
        .collect();
    if pts.len() > 1 {
        let mut mesh = Mesh::default();
        for (i, q) in pts.iter().enumerate() {
            mesh.colored_vertex(pos2(q.x, inner.top()), alpha(accent, 0.04));
            mesh.colored_vertex(*q, alpha(accent, 0.28));
            if i > 0 {
                let k = (i as u32) * 2;
                mesh.add_triangle(k - 2, k - 1, k);
                mesh.add_triangle(k - 1, k, k + 1);
            }
        }
        p.add(Shape::mesh(mesh));
        p.add(Shape::line(pts, stroke(2.0, accent)));
    }
    let now = h.back().map(|f| f.gr[slot]).unwrap_or(0.0);
    p.text(pos2(inner.right(), inner.bottom()), Align2::RIGHT_BOTTOM, format!("{now:.1} dB"), semi(20.0), TEXT);
    grid_label(p, pos2(inner.right(), inner.bottom() - 26.0), label, Align2::RIGHT_BOTTOM);
}

fn pitch_view(p: &Painter, r: Rect, h: &VecDeque<Frame>, root: i32, mask: u16, accent: Color32) {
    let inner = r.shrink2(vec2(14.0, 12.0));
    let voiced: Vec<f32> = h.iter().filter(|f| f.voiced && f.midi > 0.0).map(|f| f.midi).collect();
    let centre = if voiced.is_empty() {
        57.0
    } else {
        let mut v = voiced.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2].round()
    };
    let span = 7.0;
    let y_of = |m: f32| inner.center().y - (m - centre) / span * inner.height() * 0.5;
    let left = inner.left() + 30.0;
    let mut last_label = f32::MAX;
    for k in (centre - span) as i32..=(centre + span) as i32 {
        let y = y_of(k as f32);
        let in_scale = mask >> (k - root).rem_euclid(12) & 1 == 1;
        let is_root = (k - root).rem_euclid(12) == 0;
        p.line_segment([pos2(left, y), pos2(inner.right(), y)], stroke(1.0, alpha(Color32::WHITE, if is_root { 0.12 } else if in_scale { 0.06 } else { 0.015 })));
        if in_scale && (last_label - y).abs() >= 11.0 {
            grid_label(p, pos2(inner.left(), y), &profile::note_name(k as f32), Align2::LEFT_CENTER);
            last_label = y;
        }
    }
    let n = HISTORY as f32;
    let x_of = |i: usize| inner.right() - (h.len() - 1 - i) as f32 / (n - 1.0) * (inner.right() - left);
    let mut seg_in: Vec<Pos2> = Vec::new();
    let mut seg_out: Vec<Pos2> = Vec::new();
    let flush = |seg: &mut Vec<Pos2>, stroke: Stroke| {
        if seg.len() > 1 {
            p.add(Shape::line(std::mem::take(seg), stroke));
        }
        seg.clear();
    };
    for (i, f) in h.iter().enumerate() {
        if f.voiced && f.midi > 0.0 {
            seg_in.push(pos2(x_of(i), y_of(f.midi)));
            seg_out.push(pos2(x_of(i), y_of(f.out_midi)));
        } else {
            flush(&mut seg_in, stroke(1.3, alpha(Color32::WHITE, 0.35)));
            flush(&mut seg_out, stroke(2.4, accent));
        }
    }
    flush(&mut seg_in, stroke(1.3, alpha(Color32::WHITE, 0.35)));
    flush(&mut seg_out, stroke(2.4, accent));
    if let Some(f) = h.back().filter(|f| f.voiced && f.midi > 0.0) {
        let cents = ((f.midi - f.midi.round()) * 100.0).round();
        p.text(pos2(inner.right(), inner.top()), Align2::RIGHT_TOP, profile::note_name(f.midi), semi(22.0), TEXT);
        p.text(pos2(inner.right(), inner.top() + 28.0), Align2::RIGHT_TOP, format!("{cents:+.0} ct in"), regular(11.0), DIM);
    } else if !h.iter().any(|f| f.voiced) {
        p.text(pos2((left + inner.right()) / 2.0, inner.center().y), Align2::CENTER_CENTER, "Sing to see your pitch", semi(12.0), DIM);
    }
    grid_label(p, pos2(inner.right(), inner.bottom()), "gray: you  ·  color: after tune", Align2::RIGHT_BOTTOM);
}

fn drive_view(p: &Painter, r: Rect, mode: SatMode, drive: f32, mx: f32, accent: Color32) {
    let size = r.height() - 28.0;
    let sq = Rect::from_center_size(pos2(r.left() + 14.0 + size / 2.0, r.center().y), vec2(size, size));
    p.line_segment([sq.left_center(), sq.right_center()], stroke(1.0, alpha(Color32::WHITE, 0.06)));
    p.line_segment([sq.center_top(), sq.center_bottom()], stroke(1.0, alpha(Color32::WHITE, 0.06)));
    p.line_segment([sq.left_bottom(), sq.right_top()], stroke(1.0, alpha(Color32::WHITE, 0.08)));
    let n = 120;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let x = -1.0 + 2.0 * i as f32 / n as f32;
            let y = x + (Saturator::curve(mode, drive, x * 0.6) / 0.6 - x) * mx;
            pos2(sq.center().x + x * size / 2.0, sq.center().y - y.clamp(-1.2, 1.2) * size / 2.0)
        })
        .collect();
    p.add(Shape::line(pts, stroke(2.4, accent)));
    // A sine through the curve, to see the shape it gets.
    let wave = Rect::from_min_max(pos2(sq.right() + 18.0, r.top() + 14.0), pos2(r.right() - 14.0, r.bottom() - 14.0));
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let x = (t * 2.0 * std::f32::consts::TAU).sin() * 0.6;
            let y = x + (Saturator::curve(mode, drive, x) - x) * mx;
            pos2(wave.left() + t * wave.width(), wave.center().y - y.clamp(-1.0, 1.0) * wave.height() * 0.42)
        })
        .collect();
    p.add(Shape::line(pts, stroke(1.8, alpha(accent, 0.8))));
    grid_label(p, wave.right_bottom(), &format!("{mode:?}  ·  drive {:.0}%", drive * 100.0), Align2::RIGHT_BOTTOM);
}

fn doubler_view(p: &Painter, r: Rect, mx: f32, width: f32, detune: f32, accent: Color32) {
    let c = r.center();
    let half = r.width() * 0.4;
    p.line_segment([pos2(c.x - half, c.y + 40.0), pos2(c.x + half, c.y + 40.0)], stroke(1.0, alpha(Color32::WHITE, 0.08)));
    grid_label(p, pos2(c.x - half, c.y + 48.0), "L", Align2::CENTER_TOP);
    grid_label(p, pos2(c.x + half, c.y + 48.0), "R", Align2::CENTER_TOP);
    glow(p, c, 16.0, Color32::WHITE, 0.6);
    p.circle_filled(c, 12.0, TEXT);
    grid_label(p, c + vec2(0.0, 22.0), "you", Align2::CENTER_TOP);
    if mx > 0.0 {
        for s in [-1.0, 1.0] {
            let q = pos2(c.x + s * half * width.max(0.05), c.y - 8.0);
            let rad = 6.0 + 10.0 * mx;
            glow(p, q, rad, accent, 0.8);
            p.circle_filled(q, rad, alpha(accent, 0.85));
            grid_label(p, q + vec2(0.0, rad + 6.0), &format!("{}{:.0} ct", if s < 0.0 { "+" } else { "-" }, detune), Align2::CENTER_TOP);
        }
    }
    grid_label(p, pos2(r.right() - 14.0, r.top() + 12.0), &format!("copies at {:.0}%", mx * 100.0), Align2::RIGHT_TOP);
}

fn delay_view(p: &Painter, r: Rect, beats: f32, bpm: f32, feedback: f32, ping: bool, accent: Color32) {
    let inner = r.shrink2(vec2(18.0, 18.0));
    let inner = Rect::from_min_max(pos2(inner.left() + 16.0, inner.top()), inner.max);
    let secs = beats * 60.0 / bpm.clamp(40.0, 300.0);
    let span = 2.4f32.max(secs * 4.0);
    let x_of = |t: f32| inner.left() + t / span * inner.width();
    let mid = inner.center().y;
    p.line_segment([pos2(inner.left(), mid), pos2(inner.right(), mid)], stroke(1.0, alpha(Color32::WHITE, 0.06)));
    p.rect_filled(Rect::from_min_max(pos2(x_of(0.0), mid - inner.height() * 0.45), pos2(x_of(0.0) + 5.0, mid + inner.height() * 0.45)), 2, TEXT);
    let mut g = 1.0f32;
    let mut k = 1;
    while k < 24 {
        let t = secs * k as f32;
        if t > span || g < 0.03 {
            break;
        }
        let h = inner.height() * 0.45 * g;
        let x = x_of(t);
        let (top, bottom) = if ping {
            if k % 2 == 1 { (mid - h, mid) } else { (mid, mid + h) }
        } else {
            (mid - h, mid + h)
        };
        p.rect_filled(Rect::from_min_max(pos2(x, top), pos2(x + 5.0, bottom)), 2, alpha(accent, 0.35 + 0.65 * g));
        g *= feedback.max(0.0);
        k += 1;
    }
    if ping {
        grid_label(p, pos2(inner.left() - 16.0, inner.top()), "L", Align2::LEFT_TOP);
        grid_label(p, pos2(inner.left() - 16.0, inner.bottom()), "R", Align2::LEFT_BOTTOM);
    }
    grid_label(p, inner.right_top(), &format!("{:.0} ms at {:.0} BPM", secs * 1000.0, bpm), Align2::RIGHT_TOP);
}

fn reverb_view(p: &Painter, r: Rect, predelay_ms: f32, decay: f32, size: f32, mx: f32, accent: Color32) {
    let inner = r.shrink2(vec2(18.0, 18.0));
    let span = (decay * 1.4 + predelay_ms * 0.001).max(1.0);
    let x_of = |t: f32| inner.left() + t / span * inner.width();
    let base = inner.bottom();
    let pre = predelay_ms * 0.001;
    let height = inner.height() * (0.35 + 0.65 * (mx / 0.3).min(1.0));
    let n = 140;
    let mut mesh = Mesh::default();
    let mut line = Vec::new();
    for i in 0..=n {
        let t = pre + (span - pre) * i as f32 / n as f32;
        let rise = ((t - pre) / (0.02 + 0.08 * size)).min(1.0);
        let env = rise * (-6.9 * (t - pre) / decay.max(0.05)).exp();
        let q = pos2(x_of(t), base - env * height);
        line.push(q);
        mesh.colored_vertex(q, alpha(accent, 0.3));
        mesh.colored_vertex(pos2(q.x, base), alpha(accent, 0.02));
        if i > 0 {
            let k = (i as u32) * 2;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    p.add(Shape::mesh(mesh));
    p.add(Shape::line(line, stroke(2.0, accent)));
    p.rect_filled(Rect::from_min_max(pos2(x_of(0.0), base - inner.height()), pos2(x_of(0.0) + 4.0, base)), 2, TEXT);
    grid_label(p, inner.right_top(), &format!("{decay:.1} s tail  ·  {predelay_ms:.0} ms pre-delay"), Align2::RIGHT_TOP);
}

fn output_view(p: &Painter, r: Rect, sh: &Shared, h: &VecDeque<Frame>, accent: Color32) {
    let inner = r.shrink2(vec2(22.0, 18.0));
    let ld = |a: &AtomicF32| a.load(Ordering::Relaxed);
    let bars = [("IN", ld(&sh.in_level)), ("L", ld(&sh.out_level[0])), ("R", ld(&sh.out_level[1]))];
    let w = 22.0;
    let top = inner.top() + 6.0;
    let bottom = inner.bottom() - 16.0;
    let y_of = |db: f32| bottom - ((db + 48.0) / 48.0).clamp(0.0, 1.0) * (bottom - top);
    for db in [0.0, -6.0, -12.0, -24.0, -36.0] {
        let y = y_of(db);
        p.line_segment([pos2(inner.left() + 22.0, y), pos2(inner.left() + 22.0 + 3.0 * (w + 14.0), y)], stroke(1.0, alpha(Color32::WHITE, 0.05)));
        grid_label(p, pos2(inner.left(), y), &format!("{db:.0}"), Align2::LEFT_CENTER);
    }
    for (i, (label, v)) in bars.iter().enumerate() {
        let x = inner.left() + 26.0 + i as f32 * (w + 14.0);
        let track = Rect::from_min_max(pos2(x, top), pos2(x + w, bottom));
        p.rect_filled(track, 4, alpha(Color32::WHITE, 0.03));
        let db = gain_to_db(*v);
        let y = y_of(db);
        let color = if db > -1.0 {
            HOT
        } else if db > -9.0 {
            WARN
        } else {
            accent
        };
        if y < bottom {
            p.rect_filled(Rect::from_min_max(pos2(x, y), pos2(x + w, bottom)), 4, color);
        }
        grid_label(p, pos2(x + w / 2.0, inner.bottom()), label, Align2::CENTER_BOTTOM);
    }
    let lim = h.back().map(|f| f.gr[4]).unwrap_or(0.0);
    let lat = sh.latency.load(Ordering::Relaxed) as f32 / ld(&sh.sample_rate).max(1.0) * 1000.0;
    let x = inner.left() + 26.0 + 3.0 * (w + 14.0) + 20.0;
    p.text(pos2(x, top + 4.0), Align2::LEFT_TOP, format!("{lim:.1} dB"), semi(20.0), TEXT);
    grid_label(p, pos2(x, top + 32.0), "limiting", Align2::LEFT_TOP);
    p.text(pos2(x, top + 64.0), Align2::LEFT_TOP, format!("{lat:.1} ms"), semi(20.0), TEXT);
    grid_label(p, pos2(x, top + 92.0), "latency (reported to your DAW)", Align2::LEFT_TOP);
}

// ---------------------------------------------------------------- windows

fn save_window(ctx: &egui::Context, st: &mut EditorState) {
    let mut open = true;
    let mut done = false;
    egui::Window::new(egui::RichText::new("Save preset").font(semi(14.0)))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, -60.0))
        .show(ctx, |ui| {
            ui.label(egui::RichText::new("Saves every sound setting. Input, Match and key stay with the song.").color(DIM));
            ui.add_space(6.0);
            let r = ui.add(egui::TextEdit::singleline(&mut st.save_name).desired_width(300.0).font(regular(14.0)));
            r.request_focus();
            ui.add_space(6.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                if button(ui, "Save", vec2(80.0, 30.0), st.accent, Color32::from_rgb(12, 12, 16)).clicked() || enter {
                    let name = st.save_name.clone();
                    match st.save_user(&name) {
                        Ok(()) => {
                            st.toast(ui.ctx(), format!("Saved {}", name.trim()));
                            done = true;
                        }
                        Err(e) => st.toast(ui.ctx(), e),
                    }
                }
                if button(ui, "Cancel", vec2(80.0, 30.0), RAISED, TEXT).clicked() {
                    done = true;
                }
            });
        });
    if !open || done {
        st.save_open = false;
    }
}

fn settings_window(ctx: &egui::Context, setter: &ParamSetter, st: &mut EditorState) {
    let mut open = true;
    egui::Window::new(egui::RichText::new("Persona").font(semi(14.0)))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .anchor(Align2::RIGHT_TOP, vec2(-14.0, 64.0))
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).font(semi(13.0)));
            ui.hyperlink_to("Release notes and downloads", format!("https://github.com/{}/releases", update::REPO));
            ui.separator();
            if ui.checkbox(&mut st.settings.auto_check, "Check for updates when the plugin opens").changed() {
                update::save_settings(&st.settings);
            }
            let busy = matches!(*st.update.lock(), Status::Checking | Status::Installing { .. });
            if ui.add_enabled(!busy, egui::Button::new("Check now")).clicked() {
                update::spawn_check(st.update.clone());
            }
            match &*st.update.lock() {
                Status::UpToDate => {
                    ui.colored_label(GOOD, "You have the latest version.");
                }
                Status::Error(e) => {
                    ui.colored_label(HOT, e);
                }
                _ => {}
            }
            ui.separator();
            if st.params.voice.read().learned {
                if ui.button("Forget what Match learned").clicked() {
                    st.forget_voice(setter);
                }
            }
            if let Some(d) = user_preset_dir() {
                ui.label(egui::RichText::new(format!("Your presets: {}", d.display())).font(regular(11.0)).color(FAINT));
            }
            ui.separator();
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Styles are an interpretation of the vocal sound on each artist's records, made as starting points. \
                         Persona isn't affiliated with or endorsed by any artist. Pitch engine from HelixTune (MIT). \
                         Inter typeface by Rasmus Andersson (OFL).",
                    )
                    .font(regular(11.0))
                    .color(FAINT),
                )
                .wrap(),
            );
        });
    if !open {
        st.settings_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_read_well() {
        assert_eq!(initials("J. Cole"), "JC");
        assert_eq!(initials("2 Chainz"), "2C");
        assert_eq!(initials("21 Savage"), "21");
        assert_eq!(initials("50 Cent"), "50");
        assert_eq!(initials("A$AP Rocky"), "AR");
        assert_eq!(initials("Tyler, the Creator"), "TC");
        assert_eq!(initials("XXXTentacion"), "XX");
        assert_eq!(initials("Ad-lib Space"), "AS");
    }

    #[test]
    fn accents_are_bright() {
        for s in STYLES {
            let c = accent_of(s.colors[0]);
            let luma = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
            assert!(luma > 110.0, "{}: accent too dark ({luma})", s.name);
        }
    }

    #[test]
    fn every_module_has_a_name_lookup() {
        for m in MODULES {
            assert_eq!(Module::by_name(m.label()), Some(m));
        }
    }
}
