//! Persona Master's window.
//!
//! Left: Auto Master and the presets. Middle: the analyser with the EQ drawn over it, the
//! chain in signal order and the controls of the selected module. Right: loudness, true peak,
//! limiting and the stereo picture.

use nih_plug::prelude::*;
use nih_plug_egui::create_egui_editor;
use nih_plug_egui::egui::{self, epaint::Mesh, pos2, vec2, Align2, Color32, Painter, Pos2, Rect, Sense, Shape, StrokeKind, Ui};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::auto;
use super::engine::{balance_filter, MasterSettings};
use super::params::*;
use super::presets::{KEEP_ON_PRESET, PRESETS};
use super::shared::*;
use super::spectrum::{hz_of, Analyser, POINTS};
use crate::dsp::filters::{gain_to_db, Biquad};
use crate::ui::*;
use crate::update::{self, SharedStatus, Status};

pub const WIDTH: u32 = 1240;
pub const HEIGHT: u32 = 780;

const ACCENT: Color32 = Color32::from_rgb(70, 214, 190);
const INK: Color32 = Color32::from_rgb(8, 20, 18);
const BAND_COLORS: [Color32; MB_BANDS] =
    [Color32::from_rgb(255, 122, 122), Color32::from_rgb(255, 196, 92), Color32::from_rgb(110, 220, 150), Color32::from_rgb(110, 180, 255)];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Module {
    Eq,
    Balance,
    Glue,
    Dynamics,
    Exciter,
    Imager,
    Maximizer,
    Output,
}

const MODULES: [Module; 8] =
    [Module::Eq, Module::Balance, Module::Glue, Module::Dynamics, Module::Exciter, Module::Imager, Module::Maximizer, Module::Output];

impl Module {
    fn label(self) -> &'static str {
        match self {
            Module::Eq => "EQ",
            Module::Balance => "Balance",
            Module::Glue => "Glue",
            Module::Dynamics => "Dynamics",
            Module::Exciter => "Exciter",
            Module::Imager => "Imager",
            Module::Maximizer => "Maximizer",
            Module::Output => "Output",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Module::Eq => "Six bands, each on the whole mix, the middle or the sides. Drag the dots above.",
            Module::Balance => "The broad tonal correction Auto Master sets. Amount scales all of it.",
            Module::Glue => "Bus compression that makes the mix move as one.",
            Module::Dynamics => "Four bands of compression. Tames a boomy low end or a spiky top.",
            Module::Exciter => "Harmonics per band: warmth low down, shine up high.",
            Module::Imager => "Stereo width per band. Keep the bass in the middle.",
            Module::Maximizer => "Loudness: soft clip the peaks, then a true-peak limiter at the ceiling.",
            Module::Output => "Input and output level, and dither for 16 or 24-bit files.",
        }
    }

    #[cfg_attr(not(feature = "standalone"), allow(dead_code))]
    fn by_name(name: &str) -> Option<Module> {
        MODULES.iter().copied().find(|m| m.label().eq_ignore_ascii_case(name))
    }
}

fn module_power(p: &MasterParams, m: Module) -> Option<&BoolParam> {
    match m {
        Module::Eq => Some(&p.eq_on),
        Module::Glue => Some(&p.glue_on),
        Module::Dynamics => Some(&p.mb_on),
        Module::Exciter => Some(&p.exc_on),
        Module::Imager => Some(&p.img_on),
        Module::Maximizer => Some(&p.max_on),
        _ => None,
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct UserPreset {
    name: String,
    values: Vec<(String, f32)>,
}

struct State {
    params: Arc<MasterParams>,
    shared: Arc<MasterShared>,
    ids: Vec<String>,
    ptrs: HashMap<String, ParamPtr>,
    module: Module,
    eq_sel: usize,
    band_sel: usize,
    an_in: Analyser,
    an_out: Analyser,
    gr: VecDeque<f32>,
    user: Vec<UserPreset>,
    learn_pending: bool,
    learn_failed: bool,
    refines: u8,
    undo: Option<Vec<(ParamPtr, f32)>>,
    save_open: bool,
    save_name: String,
    update: SharedStatus,
    settings: update::Settings,
    settings_open: bool,
    toast: Option<(String, f64)>,
    fonts_ready: bool,
    startup_preset: Option<String>,
}

fn preset_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Persona").join("Master Presets"))
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn load_user() -> Vec<UserPreset> {
    let Some(rd) = preset_dir().and_then(|d| std::fs::read_dir(d).ok()) else {
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

impl State {
    fn ptr(&self, id: &str) -> Option<ParamPtr> {
        self.ptrs.get(id).copied()
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

    fn set_plain(&self, setter: &ParamSetter, values: &[(String, f32)]) {
        let t: Vec<(ParamPtr, f32)> = values.iter().filter_map(|(id, v)| self.ptr(id).map(|p| (p, unsafe { p.preview_normalized(*v) }))).collect();
        self.apply(setter, &t);
    }

    fn snapshot(&self) -> Vec<(ParamPtr, f32)> {
        self.ids.iter().map(|id| (self.ptrs[id], unsafe { self.ptrs[id].unmodulated_normalized_value() })).collect()
    }

    /// Loads a preset: every sound parameter goes to the preset's value or its default.
    fn load(&self, setter: &ParamSetter, name: &str, values: &[(String, f32)]) {
        let t: Vec<(ParamPtr, f32)> = self
            .ids
            .iter()
            .filter(|id| !KEEP_ON_PRESET.contains(&id.as_str()))
            .map(|id| {
                let ptr = self.ptrs[id];
                let n = match values.iter().find(|(k, _)| k == id) {
                    Some((_, v)) => unsafe { ptr.preview_normalized(*v) },
                    None => unsafe { ptr.default_normalized_value() },
                };
                (ptr, n)
            })
            .collect();
        self.apply(setter, &t);
        *self.params.preset.write() = name.to_string();
    }

    fn load_named(&self, setter: &ParamSetter, name: &str) {
        if let Some(p) = PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name)) {
            let v: Vec<(String, f32)> = p.values.iter().map(|(k, v)| (k.to_string(), *v)).collect();
            self.load(setter, p.name, &v);
        } else if let Some(u) = self.user.iter().find(|u| u.name == name) {
            self.load(setter, &u.name, &u.values);
        }
    }

    fn save_user(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("give the preset a name first".into());
        }
        if PRESETS.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
            return Err("that name belongs to a built-in preset".into());
        }
        let values = self
            .ids
            .iter()
            .filter(|id| !KEEP_ON_PRESET.contains(&id.as_str()))
            .map(|id| (id.clone(), unsafe { self.ptrs[id].unmodulated_plain_value() }))
            .collect();
        let dir = preset_dir().ok_or("no config folder on this system")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&UserPreset { name: name.to_string(), values }).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(format!("{}.json", sanitize(name))), json).map_err(|e| e.to_string())?;
        *self.params.preset.write() = name.to_string();
        self.user = load_user();
        Ok(())
    }

    fn toast(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ctx.input(|i| i.time) + 2.6));
    }

    fn start_learn(&mut self) {
        let sh = &self.shared;
        sh.learn_state.store(LEARN_IDLE, Ordering::Relaxed);
        sh.learn_progress.store(0.0, Ordering::Relaxed);
        sh.learn_cancel.store(false, Ordering::Relaxed);
        sh.learn_request.store(REQ_LISTEN, Ordering::Relaxed);
        self.learn_pending = true;
        self.learn_failed = false;
        self.refines = 0;
    }

    fn cancel_learn(&mut self) {
        self.shared.learn_request.store(REQ_NONE, Ordering::Relaxed);
        self.shared.learn_cancel.store(true, Ordering::Relaxed);
        self.learn_pending = false;
    }

    fn poll_learn(&mut self, setter: &ParamSetter) {
        if !self.learn_pending {
            return;
        }
        let sh = self.shared.clone();
        let target = self.params.target.value();
        match sh.learn_state.load(Ordering::Relaxed) {
            LEARN_HEARD => {
                let out = auto::decide(&auto::Heard::read(&sh), self.params.genre.value(), target);
                self.undo = Some(self.snapshot());
                self.set_plain(setter, &out.values);
                *self.params.report.write() = out.report;
                *self.params.preset.write() = "Auto Master".to_string();
                sh.learn_state.store(LEARN_IDLE, Ordering::Relaxed);
                sh.learn_request.store(REQ_REFINE, Ordering::Relaxed);
                self.refines = 1;
            }
            LEARN_REFINED => {
                let out_lufs = sh.heard.out_lufs.load(Ordering::Relaxed);
                let (gain, line) = auto::refine(out_lufs, target, self.params.max_gain.value());
                self.set_plain(setter, &[("max_gain".to_string(), gain)]);
                let mut rep = self.params.report.write();
                rep.retain(|l| !l.starts_with("Fine-tuned"));
                rep.push(line);
                drop(rep);
                sh.learn_state.store(LEARN_IDLE, Ordering::Relaxed);
                if self.refines < 2 && (target - out_lufs).abs() > 0.7 {
                    self.refines += 1;
                    sh.learn_request.store(REQ_REFINE, Ordering::Relaxed);
                } else {
                    self.learn_pending = false;
                    sh.reset_meters.store(true, Ordering::Relaxed);
                }
            }
            LEARN_FAILED => {
                self.learn_pending = false;
                self.learn_failed = true;
            }
            _ => {}
        }
    }
}

pub fn create(params: Arc<MasterParams>, shared: Arc<MasterShared>) -> Option<Box<dyn Editor>> {
    let map = params.param_map();
    #[allow(unused_mut)]
    let mut state = State {
        ids: map.iter().map(|(id, _, _)| id.clone()).collect(),
        ptrs: map.iter().map(|(id, p, _)| (id.clone(), *p)).collect(),
        params: params.clone(),
        shared,
        module: Module::Maximizer,
        eq_sel: 2,
        band_sel: 0,
        an_in: Analyser::new(),
        an_out: Analyser::new(),
        gr: VecDeque::with_capacity(240),
        user: load_user(),
        learn_pending: false,
        learn_failed: false,
        refines: 0,
        undo: None,
        save_open: false,
        save_name: String::new(),
        update: Arc::new(parking_lot::Mutex::new(Status::Idle)),
        settings: update::load_settings(),
        settings_open: false,
        toast: None,
        fonts_ready: false,
        startup_preset: None,
    };
    #[cfg(feature = "standalone")]
    {
        state.startup_preset = std::env::var("PERSONA_PRESET").ok();
        if let Some(m) = std::env::var("PERSONA_MODULE").ok().as_deref().and_then(Module::by_name) {
            state.module = m;
        }
    }

    create_egui_editor(
        params.editor_state.clone(),
        state,
        |ctx, state| {
            setup_fonts(ctx);
            apply_theme(ctx, ACCENT);
            if state.settings.auto_check && *state.update.lock() == Status::Idle {
                update::spawn_check(state.update.clone());
            }
        },
        |ctx, setter, st| {
            if !st.fonts_ready {
                st.fonts_ready = true;
                ctx.request_repaint();
                return;
            }
            if let Some(name) = st.startup_preset.take() {
                st.load_named(setter, &name);
            }
            st.poll_learn(setter);
            let fs = st.shared.sample_rate.load(Ordering::Relaxed).max(8000.0);
            let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.1);
            st.an_in.update(&st.shared.spec_in, fs, dt);
            st.an_out.update(&st.shared.spec_out, fs, dt);
            if st.gr.len() >= 240 {
                st.gr.pop_front();
            }
            st.gr.push_back(st.shared.max_gr.load(Ordering::Relaxed));

            egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ctx, |ui| {
                let full = ui.max_rect();
                let m = 14.0;
                let header = Rect::from_min_size(full.min + vec2(m, 12.0), vec2(full.width() - 2.0 * m, 44.0));
                let top = header.bottom() + 10.0;
                let bottom = full.bottom() - m;
                let side = Rect::from_min_max(pos2(full.left() + m, top), pos2(full.left() + m + 250.0, bottom));
                let meters = Rect::from_min_max(pos2(full.right() - m - 210.0, top), pos2(full.right() - m, bottom));
                let mid = Rect::from_min_max(pos2(side.right() + 12.0, top), pos2(meters.left() - 12.0, bottom));
                let auto_r = Rect::from_min_size(side.min, vec2(side.width(), 372.0));
                let presets_r = Rect::from_min_max(pos2(side.left(), auto_r.bottom() + 12.0), side.max);
                let graph = Rect::from_min_size(mid.min, vec2(mid.width(), 290.0));
                let strip = Rect::from_min_size(pos2(mid.left(), graph.bottom() + 12.0), vec2(mid.width(), 62.0));
                let module = Rect::from_min_max(pos2(mid.left(), strip.bottom() + 12.0), mid.max);

                header_bar(&mut child(ui, header), setter, st);
                auto_panel(&mut child(ui, auto_r), setter, st);
                presets_panel(&mut child(ui, presets_r), setter, st);
                graph_panel(&mut child(ui, graph), setter, st, fs);
                chain_strip(&mut child(ui, strip), setter, st);
                module_panel(&mut child(ui, module), setter, st);
                meters_panel(&mut child(ui, meters), st);
            });

            if st.settings_open {
                settings_window(ctx, st);
            }
            if st.save_open {
                save_window(ctx, st);
            }
            if let Some((msg, until)) = st.toast.clone() {
                if ctx.input(|i| i.time) > until {
                    st.toast = None;
                } else {
                    egui::Area::new(egui::Id::new("toast")).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -28.0)).show(ctx, |ui| {
                        egui::Frame::new().fill(RAISED).stroke(stroke(1.0, ACCENT)).corner_radius(10).inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                            ui.label(egui::RichText::new(msg).font(semi(12.5)).color(TEXT))
                        });
                    });
                }
            }
            ctx.request_repaint();
        },
    )
}

fn ld(a: &AtomicF32) -> f32 {
    a.load(Ordering::Relaxed)
}

fn toggle(setter: &ParamSetter, p: &BoolParam) {
    setter.begin_set_parameter(p);
    setter.set_parameter(p, !p.value());
    setter.end_set_parameter(p);
}

// ---------------------------------------------------------------- header

fn header_bar(ui: &mut Ui, setter: &ParamSetter, st: &mut State) {
    let r = ui.max_rect();
    let p = ui.painter().clone();
    let level = ld(&st.shared.out_peak[0]).max(ld(&st.shared.out_peak[1]));
    let c = pos2(r.left() + 18.0, r.center().y);
    let lift = ((gain_to_db(level) + 50.0) / 50.0).clamp(0.0, 1.0);
    glow(&p, c, 15.0, ACCENT, 0.6 + lift * 1.6);
    gradient_rect(&p, Rect::from_center_size(c, vec2(30.0, 30.0)), 15.0, ACCENT, Color32::from_rgb(20, 60, 110), vec2(1.0, 1.0));
    p.circle_filled(c + vec2(-4.0, -5.0), 4.0, alpha(Color32::WHITE, 0.35));
    let mut x = r.left() + 44.0;
    for (word, color) in [("PERSONA", TEXT), ("MASTER", ACCENT)] {
        for ch in word.chars() {
            let g = p.layout_no_wrap(ch.to_string(), heavy(19.0), color);
            let w = g.size().x;
            p.galley(pos2(x, r.center().y - g.size().y / 2.0), g, color);
            x += w + 2.6;
        }
        x += 8.0;
    }
    p.text(pos2(x + 4.0, r.center().y + 1.0), Align2::LEFT_CENTER, "mastering chain", regular(12.5), FAINT);

    let bar = Rect::from_min_max(pos2(r.right() - 560.0, r.center().y - 16.0), pos2(r.right(), r.center().y + 16.0));
    let mut b = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::right_to_left(egui::Align::Center)));
    b.spacing_mut().item_spacing.x = 8.0;
    if icon_button(&mut b, 32.0, gear).on_hover_text("Settings and updates").clicked() {
        st.settings_open = !st.settings_open;
    }
    crate::editor::update_badge(&mut b, &st.update);
    b.add_space(6.0);
    let bypass = st.params.bypass.value();
    if button(&mut b, if bypass { "Bypassed" } else { "Bypass" }, vec2(84.0, 28.0), if bypass { WARN } else { RAISED }, if bypass { INK } else { TEXT })
        .on_hover_text("Hear the mix without the master, in time with it")
        .clicked()
    {
        toggle(setter, &st.params.bypass);
    }
    let gm = st.shared.gain_match.load(Ordering::Relaxed);
    let label = if gm { format!("Gain match {:+.1} dB", ld(&st.shared.match_db)) } else { "Gain match".to_string() };
    if button(&mut b, &label, vec2(if gm { 156.0 } else { 100.0 }, 28.0), if gm { HOT } else { RAISED }, if gm { Color32::WHITE } else { TEXT })
        .on_hover_text("Turns the master down to the loudness of the mix, so Bypass compares sound and not volume.\nFor listening only: switch it off before you export.")
        .clicked()
    {
        st.shared.gain_match.store(!gm, Ordering::Relaxed);
    }
    let latency = st.shared.latency.load(Ordering::Relaxed) as f32 / ld(&st.shared.sample_rate).max(1.0) * 1000.0;
    b.label(egui::RichText::new(format!("{latency:.1} ms")).font(regular(11.0)).color(FAINT)).on_hover_text("Latency, reported to your DAW");
}

// ---------------------------------------------------------------- Auto Master

fn auto_panel(ui: &mut Ui, setter: &ParamSetter, st: &mut State) {
    let r = ui.max_rect();
    card(ui.painter(), r);
    let p = ui.painter().clone();
    let g = pos2(r.left() + 24.0, r.top() + 23.0);
    for (k, h) in [5.0, 10.0, 14.0, 8.0, 12.0, 6.0].iter().enumerate() {
        let x = g.x - 8.0 + k as f32 * 3.4;
        p.line_segment([pos2(x, g.y - h / 2.0), pos2(x, g.y + h / 2.0)], stroke(2.0, ACCENT));
    }
    p.text(pos2(r.left() + 42.0, r.top() + 23.0), Align2::LEFT_CENTER, "Auto Master", semi(15.0), TEXT);
    let body = Rect::from_min_max(pos2(r.left() + 16.0, r.top() + 44.0), pos2(r.right() - 16.0, r.bottom() - 14.0));
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
    ui.spacing_mut().item_spacing.y = 4.0;
    let params = st.params.clone();
    let state = ld_u(&st.shared.learn_state);
    let progress = ld(&st.shared.learn_progress);

    if st.learn_pending {
        let (title, hint) = match state {
            LEARN_LISTENING => (format!("Listening… {:.0}%", progress * 100.0), "Keep the loudest part of the song playing."),
            LEARN_REFINING => (format!("Fine-tuning loudness… {:.0}%", progress * 100.0), "Checking the master against your target. Keep it playing."),
            _ => ("Waiting for audio…".to_string(), "Press play on the loudest part of your song."),
        };
        ui.add_space(6.0);
        let (bar, _) = ui.allocate_exact_size(vec2(body.width(), 8.0), Sense::hover());
        ui.painter().rect_filled(bar, 4, WELL);
        if matches!(state, LEARN_LISTENING | LEARN_REFINING) {
            ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * progress.max(0.02), 8.0)), 4, ACCENT);
        } else {
            let t = (ui.input(|i| i.time) as f32 * 0.8).fract();
            ui.painter().rect_filled(Rect::from_min_size(pos2(bar.left() + (bar.width() - 60.0) * t, bar.top()), vec2(60.0, 8.0)), 4, alpha(ACCENT, 0.6));
        }
        ui.add_space(8.0);
        ui.label(egui::RichText::new(title).font(semi(13.0)).color(TEXT));
        ui.add(egui::Label::new(egui::RichText::new(hint).font(regular(11.5)).color(DIM)).wrap());
        ui.add_space(8.0);
        if button(&mut ui, "Cancel", vec2(84.0, 28.0), RAISED, TEXT).clicked() {
            st.cancel_learn();
        }
        return;
    }

    caption(&mut ui, "GENRE");
    combo(&mut ui, setter, &params.genre, body.width() - 8.0);
    ui.add_space(4.0);
    caption(&mut ui, "TARGET LOUDNESS");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let cur = params.target.value();
        for (label, v, tip) in [
            ("Stream -14", -14.0, "What streaming services play everything back at. The most dynamic."),
            ("Loud -10", -10.0, "Where most rap and pop releases sit."),
            ("Club -8", -8.0, "As loud as commercial trap and EDM. Costs some punch."),
        ] {
            if chip(ui, label, (cur - v).abs() < 0.26, ACCENT).on_hover_text(tip).clicked() {
                setter.begin_set_parameter(&params.target);
                setter.set_parameter(&params.target, v);
                setter.end_set_parameter(&params.target);
            }
        }
    });
    ui.add_space(6.0);
    let report = params.report.read().clone();
    let footer = 34.0;
    let text_h = (body.bottom() - ui.cursor().top() - footer).max(40.0);
    egui::ScrollArea::vertical().id_salt("report").max_height(text_h).auto_shrink([false, false]).show(&mut ui, |ui| {
        if st.learn_failed {
            ui.add(egui::Label::new(egui::RichText::new("Didn't hear enough music in 90 seconds. Press play and try again.").font(regular(11.5)).color(WARN)).wrap());
        } else if report.is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Play the loudest part of your song and press Listen. Auto Master measures the mix for about 12 seconds, \
                         sets the tonal balance, compression, width and loudness for the genre, then checks the result against your target.",
                    )
                    .font(regular(11.5))
                    .color(DIM),
                )
                .wrap(),
            );
        } else {
            for l in &report {
                ui.horizontal_top(|ui| {
                    ui.label(egui::RichText::new("✓").font(semi(11.0)).color(GOOD));
                    ui.add(egui::Label::new(egui::RichText::new(l).font(regular(11.0)).color(DIM)).wrap());
                });
            }
        }
    });
    let row = Rect::from_min_size(pos2(body.left(), body.bottom() - 30.0), vec2(body.width(), 30.0));
    let mut b = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
    b.spacing_mut().item_spacing.x = 6.0;
    if button(&mut b, if report.is_empty() { "Listen" } else { "Listen again" }, vec2(118.0, 30.0), ACCENT, INK).clicked() {
        st.start_learn();
    }
    if st.undo.is_some() && button(&mut b, "Undo", vec2(62.0, 30.0), RAISED, TEXT).on_hover_text("Put back the settings from before").clicked() {
        if let Some(u) = st.undo.take() {
            st.apply(setter, &u);
            st.params.report.write().clear();
        }
    }
}

fn ld_u(a: &std::sync::atomic::AtomicU32) -> u32 {
    a.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------- presets

fn presets_panel(ui: &mut Ui, setter: &ParamSetter, st: &mut State) {
    let r = ui.max_rect();
    card(ui.painter(), r);
    let inner = r.shrink2(vec2(12.0, 12.0));
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Presets").font(semi(15.0)).color(TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if button(ui, "Save as…", vec2(78.0, 24.0), RAISED, TEXT).clicked() {
                st.save_name = String::new();
                st.save_open = true;
            }
        });
    });
    ui.add_space(4.0);
    let current = st.params.preset.read().clone();
    let mut pick: Option<String> = None;
    let mut delete: Option<String> = None;
    egui::ScrollArea::vertical().id_salt("presets").auto_shrink([false, false]).show(&mut ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let rows: Vec<(String, String, bool)> = PRESETS
            .iter()
            .map(|p| (p.name.to_string(), p.blurb.to_string(), false))
            .chain(st.user.iter().map(|u| (u.name.clone(), "Your preset. Right-click to delete.".to_string(), true)))
            .collect();
        for (name, blurb, user) in rows {
            let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 6.0, 30.0), Sense::click());
            let selected = current == name;
            let p = ui.painter();
            if selected {
                p.rect_filled(row, 9, RAISED);
                p.rect_filled(Rect::from_min_size(row.min + vec2(0.0, 8.0), vec2(3.0, 14.0)), 2, ACCENT);
            } else if resp.hovered() {
                p.rect_filled(row, 9, alpha(Color32::WHITE, 0.035));
            }
            p.text(pos2(row.left() + 12.0, row.center().y), Align2::LEFT_CENTER, &name, semi(12.5), if selected { TEXT } else { mix(TEXT, DIM, 0.35) });
            if user {
                p.text(pos2(row.right() - 8.0, row.center().y), Align2::RIGHT_CENTER, "mine", regular(10.0), FAINT);
            }
            let resp = hover_cursor(resp).on_hover_text(blurb);
            if resp.clicked() {
                pick = Some(name.clone());
            }
            if user && resp.secondary_clicked() {
                delete = Some(name);
            }
        }
    });
    if let Some(name) = pick {
        st.load_named(setter, &name);
    }
    if let Some(name) = delete {
        if let Some(d) = preset_dir() {
            let _ = std::fs::remove_file(d.join(format!("{}.json", sanitize(&name))));
        }
        st.user = load_user();
        st.toast(ui.ctx(), format!("Deleted {name}"));
    }
}

// ---------------------------------------------------------------- analyser and EQ

fn graph_panel(ui: &mut Ui, setter: &ParamSetter, st: &mut State, fs: f32) {
    let r = ui.max_rect();
    card(ui.painter(), r);
    let inner = Rect::from_min_max(pos2(r.left() + 34.0, r.top() + 12.0), pos2(r.right() - 12.0, r.bottom() - 20.0));
    let p = ui.painter().with_clip_rect(r.shrink(1.0));
    p.rect_filled(inner, 8, WELL);
    let (lo, hi) = (20f32.ln(), 20000f32.ln());
    let x_of = |hz: f32| inner.left() + (hz.max(20.0).ln() - lo) / (hi - lo) * inner.width();
    let hz_at = |x: f32| (lo + (x - inner.left()) / inner.width() * (hi - lo)).exp();
    let range = 15.0;
    let y_of = |db: f32| inner.center().y - db.clamp(-range, range) / range * inner.height() * 0.5;
    let db_at = |y: f32| (inner.center().y - y) / (inner.height() * 0.5) * range;
    // The spectrum has its own scale: -72 dB at the bottom, +6 at the top.
    let sy = |db: f32| inner.bottom() - ((db + 72.0) / 78.0).clamp(0.0, 1.0) * inner.height();

    for (hz, label) in [(50.0, "50"), (100.0, "100"), (200.0, "200"), (500.0, "500"), (1000.0, "1k"), (2000.0, "2k"), (5000.0, "5k"), (10000.0, "10k")] {
        let x = x_of(hz);
        p.line_segment([pos2(x, inner.top()), pos2(x, inner.bottom())], stroke(1.0, alpha(Color32::WHITE, 0.04)));
        grid_label(&p, pos2(x, inner.bottom() + 3.0), label, Align2::CENTER_TOP);
    }
    for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
        let y = y_of(db);
        p.line_segment([pos2(inner.left(), y), pos2(inner.right(), y)], stroke(1.0, alpha(Color32::WHITE, if db == 0.0 { 0.12 } else { 0.04 })));
        grid_label(&p, pos2(inner.left() - 5.0, y), &format!("{db:+.0}"), Align2::RIGHT_CENTER);
    }

    let params = st.params.clone();
    let s = MasterSettings::from_params(&params, 0);
    let pc = p.with_clip_rect(inner);

    // ---- where the genre wants the balance, placed on the level of the music
    let out = &st.an_out.db;
    let mids: Vec<usize> = (0..POINTS).filter(|i| (300.0..4000.0).contains(&hz_of(*i))).collect();
    let level = mids.iter().map(|i| out[*i]).sum::<f32>() / mids.len() as f32;
    let genre = params.genre.value();
    if level > -70.0 {
        let tm = mids.iter().map(|i| auto::curve_at(genre, hz_of(*i))).sum::<f32>() / mids.len() as f32;
        let mut mesh = Mesh::default();
        for i in 0..POINTS {
            let t = level + auto::curve_at(genre, hz_of(i)) - tm;
            mesh.colored_vertex(pos2(x_of(hz_of(i)), sy(t + 3.0)), alpha(Color32::WHITE, 0.055));
            mesh.colored_vertex(pos2(x_of(hz_of(i)), sy(t - 3.0)), alpha(Color32::WHITE, 0.055));
            if i > 0 {
                let k = (i as u32) * 2;
                mesh.add_triangle(k - 2, k - 1, k);
                mesh.add_triangle(k - 1, k, k + 1);
            }
        }
        pc.add(Shape::mesh(mesh));
    }

    // ---- spectra: the mix behind, the master in front
    let line = |db: &[f32; POINTS]| -> Vec<Pos2> { (0..POINTS).map(|i| pos2(x_of(hz_of(i)), sy(db[i]))).collect() };
    let fill = |pts: &[Pos2], color: Color32, a: f32| {
        let mut mesh = Mesh::default();
        for (i, q) in pts.iter().enumerate() {
            mesh.colored_vertex(*q, alpha(color, a));
            mesh.colored_vertex(pos2(q.x, inner.bottom()), alpha(color, 0.0));
            if i > 0 {
                let k = (i as u32) * 2;
                mesh.add_triangle(k - 2, k - 1, k);
                mesh.add_triangle(k - 1, k, k + 1);
            }
        }
        pc.add(Shape::mesh(mesh));
    };
    let pin = line(&st.an_in.db);
    fill(&pin, Color32::WHITE, 0.07);
    pc.add(Shape::line(pin, stroke(1.0, alpha(Color32::WHITE, 0.22))));
    let pout = line(&st.an_out.db);
    fill(&pout, ACCENT, 0.22);
    pc.add(Shape::line(pout, stroke(1.6, alpha(ACCENT, 0.9))));

    // ---- the EQ and Balance curves
    let mut filters: Vec<Biquad> = Vec::new();
    if s.eq_on {
        filters.extend(s.eq.iter().filter(|b| b.active()).map(|b| b.filter(fs)));
    }
    if s.bal_amount > 0.001 {
        filters.extend((0..BAL_BANDS).filter(|i| s.bal_db[*i].abs() > 0.01).map(|i| balance_filter(i, fs, s.bal_db[i] * s.bal_amount)));
    }
    let n = 220;
    let curve: Vec<Pos2> = (0..=n)
        .map(|i| {
            let hz = (lo + (hi - lo) * i as f32 / n as f32).exp().min(fs * 0.49);
            pos2(inner.left() + inner.width() * i as f32 / n as f32, y_of(filters.iter().map(|b| b.response_db(fs, hz)).sum()))
        })
        .collect();
    pc.add(Shape::line(curve, stroke(2.2, TEXT)));

    // ---- the six dots
    for i in 0..EQ_BANDS {
        let band = &params.eq[i];
        let cut = matches!(band.kind.value(), EqType::LowCut | EqType::HighCut);
        let c = pos2(x_of(band.freq.value()), y_of(if cut { 0.0 } else { band.gain.value() }));
        let resp = hover_cursor(ui.interact(Rect::from_center_size(c, vec2(26.0, 26.0)), ui.id().with(("eq", i)), Sense::click_and_drag()));
        let on = band.on.value() && s.eq_on;
        if resp.drag_started() {
            setter.begin_set_parameter(&band.freq);
            setter.begin_set_parameter(&band.gain);
            st.eq_sel = i;
            st.module = Module::Eq;
        }
        if resp.dragged() {
            if let Some(m) = resp.interact_pointer_pos() {
                setter.set_parameter(&band.freq, hz_at(m.x.clamp(inner.left(), inner.right())).clamp(20.0, 20000.0));
                if !cut {
                    setter.set_parameter(&band.gain, db_at(m.y).clamp(-12.0, 12.0));
                }
            }
        }
        if resp.drag_stopped() {
            setter.end_set_parameter(&band.freq);
            setter.end_set_parameter(&band.gain);
        }
        if resp.double_clicked() {
            toggle(setter, &band.on);
        } else if resp.clicked() {
            st.eq_sel = i;
            st.module = Module::Eq;
        }
        if resp.hovered() && !resp.dragged() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                let v = (band.q.unmodulated_normalized_value() + scroll.signum() * 0.04).clamp(0.0, 1.0);
                setter.begin_set_parameter(&band.q);
                setter.set_parameter_normalized(&band.q, v);
                setter.end_set_parameter(&band.q);
            }
        }
        let selected = st.eq_sel == i && st.module == Module::Eq;
        let col = match band.channel.value() {
            Channel::Stereo => ACCENT,
            Channel::Mid => WARN,
            Channel::Side => Color32::from_rgb(150, 160, 255),
        };
        let p = ui.painter();
        if selected || resp.hovered() {
            glow(p, c, 10.0, col, 1.0);
        }
        p.circle_filled(c, 9.0, if on { col } else { RAISED });
        p.circle_stroke(c, 9.0, stroke(if selected { 2.0 } else { 1.0 }, if selected { Color32::WHITE } else { alpha(Color32::WHITE, 0.3) }));
        p.text(c, Align2::CENTER_CENTER, format!("{}", i + 1), semi(10.5), if on { INK } else { DIM });
        resp.on_hover_text(format!(
            "EQ {}: {} at {}, {:+.1} dB\nDrag to move, scroll for width, double-click to switch on or off",
            i + 1,
            EqType::variants()[EqType::to_index(band.kind.value())],
            band.freq,
            band.gain.value()
        ));
    }
    grid_label(&p, pos2(inner.right() - 6.0, inner.top() + 5.0), "white: your mix  ·  colour: the master  ·  band: where the genre sits", Align2::RIGHT_TOP);
}

// ---------------------------------------------------------------- chain strip

fn chain_strip(ui: &mut Ui, setter: &ParamSetter, st: &mut State) {
    let r = ui.max_rect();
    let params = st.params.clone();
    let s = MasterSettings::from_params(&params, 0);
    let sh = st.shared.clone();
    let gap = 6.0;
    let w = (r.width() - gap * (MODULES.len() as f32 - 1.0)) / MODULES.len() as f32;
    for (i, m) in MODULES.iter().copied().enumerate() {
        let tile = Rect::from_min_size(pos2(r.left() + i as f32 * (w + gap), r.top()), vec2(w, r.height()));
        let resp = hover_cursor(ui.interact(tile, ui.id().with(("tile", i)), Sense::click()));
        let selected = st.module == m;
        let on = match m {
            Module::Eq => s.eq_on && s.eq.iter().any(|b| b.active()),
            Module::Balance => s.bal_amount > 0.001 && s.bal_db.iter().any(|g| g.abs() > 0.05),
            Module::Glue => s.glue_on,
            Module::Dynamics => s.mb_on,
            Module::Exciter => s.exc_on && s.band.iter().any(|b| b.excite > 0.005),
            Module::Imager => s.imaging(),
            Module::Maximizer => s.max_on,
            Module::Output => true,
        };
        let p = ui.painter();
        p.rect_filled(tile, 12, if selected { RAISED } else if resp.hovered() { mix(SURFACE, RAISED, 0.6) } else { SURFACE });
        p.rect_stroke(tile, 12, stroke(if selected { 1.5 } else { 1.0 }, if selected { ACCENT } else { alpha(EDGE, 0.7) }), StrokeKind::Inside);
        let dot = pos2(tile.center().x, tile.top() + 14.0);
        if on {
            glow(p, dot, 4.0, ACCENT, 1.2);
            p.circle_filled(dot, 3.5, ACCENT);
        } else {
            p.circle_filled(dot, 3.0, FAINT);
        }
        p.text(pos2(tile.center().x, tile.top() + 32.0), Align2::CENTER_CENTER, m.label(), semi(11.5), if on || selected { TEXT } else { DIM });
        let level = match m {
            Module::Glue => ld(&sh.glue_gr) / 6.0,
            Module::Dynamics => sh.mb_gr.iter().map(ld).fold(0.0, f32::max) / 6.0,
            Module::Maximizer => ld(&sh.max_gr) / 8.0,
            Module::Exciter => s.band.iter().map(|b| b.excite).fold(0.0, f32::max),
            Module::Imager => s.band.iter().map(|b| (b.width - 1.0).abs()).fold(0.0, f32::max),
            Module::Balance => s.bal_db.iter().map(|g| g.abs()).fold(0.0, f32::max) * s.bal_amount / 5.0,
            Module::Eq => s.eq.iter().filter(|b| b.active()).map(|b| b.gain.abs()).fold(0.0, f32::max) / 8.0,
            Module::Output => ((gain_to_db(ld(&sh.out_peak[0]).max(ld(&sh.out_peak[1]))) + 60.0) / 60.0).max(0.0),
        }
        .clamp(0.0, 1.0);
        let bar = Rect::from_min_size(pos2(tile.left() + 10.0, tile.bottom() - 14.0), vec2(w - 20.0, 4.0));
        p.rect_filled(bar, 2, WELL);
        if level > 0.01 && on {
            let gr = matches!(m, Module::Glue | Module::Dynamics | Module::Maximizer);
            p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * level, 4.0)), 2, if gr { mix(ACCENT, WARN, 0.6) } else { ACCENT });
        }
        let resp = resp.on_hover_text(if module_power(&params, m).is_some() { "Click to edit, double-click to switch on or off" } else { "Click to edit" });
        if resp.double_clicked() {
            if let Some(pw) = module_power(&params, m) {
                toggle(setter, pw);
            }
        } else if resp.clicked() {
            st.module = m;
        }
    }
}

// ---------------------------------------------------------------- module editor

/// Horizontal gain-reduction bars in the module's side view.
fn gr_bars(p: &Painter, r: Rect, rows: &[(&str, f32, Color32)], max_db: f32) {
    let inner = r.shrink2(vec2(14.0, 14.0));
    let h = (inner.height() / rows.len() as f32).min(34.0);
    for (i, (label, db, col)) in rows.iter().enumerate() {
        let y = inner.top() + i as f32 * h;
        p.text(pos2(inner.left(), y + 7.0), Align2::LEFT_CENTER, *label, semi(10.5), DIM);
        p.text(pos2(inner.right(), y + 7.0), Align2::RIGHT_CENTER, format!("{:.1} dB", if *db < 0.05 { 0.0 } else { -*db }), semi(10.5), TEXT);
        let bar = Rect::from_min_size(pos2(inner.left(), y + 16.0), vec2(inner.width(), 6.0));
        p.rect_filled(bar, 3, alpha(Color32::WHITE, 0.06));
        let t = (db / max_db).clamp(0.0, 1.0);
        if t > 0.0 {
            p.rect_filled(Rect::from_min_max(pos2(bar.right() - bar.width() * t, bar.top()), bar.max), 3, *col);
        }
    }
}

fn module_panel(ui: &mut Ui, setter: &ParamSetter, st: &mut State) {
    let r = ui.max_rect();
    card(ui.painter(), r);
    let m = st.module;
    let params = st.params.clone();
    let sh = st.shared.clone();
    let p = ui.painter().clone();
    p.text(r.left_top() + vec2(20.0, 24.0), Align2::LEFT_CENTER, m.label(), semi(16.0), TEXT);
    let tw = p.layout_no_wrap(m.label().to_string(), semi(16.0), TEXT).size().x;
    p.text(r.left_top() + vec2(32.0 + tw, 24.5), Align2::LEFT_CENTER, m.blurb(), regular(12.0), FAINT);
    if let Some(pw) = module_power(&params, m) {
        let sw = Rect::from_min_size(pos2(r.right() - 58.0, r.top() + 13.0), vec2(38.0, 22.0));
        switch(&mut child(ui, sw), setter, pw, ACCENT);
    }
    let body = Rect::from_min_max(pos2(r.left() + 16.0, r.top() + 50.0), pos2(r.right() - 16.0, r.bottom() - 14.0));
    let visual = Rect::from_min_max(pos2(body.right() - 210.0, body.top()), body.max);
    let controls = Rect::from_min_max(body.min, pos2(visual.left() - 14.0, body.bottom()));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(controls).layout(egui::Layout::top_down(egui::Align::Min)));
    if module_power(&params, m).is_some_and(|b| !b.value()) {
        c.multiply_opacity(0.5);
    }
    c.spacing_mut().item_spacing = vec2(4.0, 6.0);
    let vp = ui.painter().with_clip_rect(visual);
    vp.rect_filled(visual, 12, WELL);
    let note = |text: &str| {
        let g = vp.layout(text.to_string(), regular(11.0), DIM, visual.width() - 28.0);
        vp.galley(visual.min + vec2(14.0, 14.0), g, DIM);
    };

    match m {
        Module::Eq => {
            c.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                for i in 0..EQ_BANDS {
                    if chip(ui, &format!("{}", i + 1), st.eq_sel == i, ACCENT).clicked() {
                        st.eq_sel = i;
                    }
                }
                ui.add_space(8.0);
                pill(ui, setter, &params.eq[st.eq_sel].on, "Band on", ACCENT);
            });
            let b = &params.eq[st.eq_sel];
            c.horizontal(|ui| {
                ui.vertical(|ui| {
                    caption(ui, "SHAPE");
                    combo(ui, setter, &b.kind, 110.0);
                });
                ui.vertical(|ui| {
                    caption(ui, "WORKS ON");
                    segmented(ui, setter, &b.channel, ACCENT, 190.0);
                });
            });
            c.horizontal(|ui| {
                knob(ui, setter, &b.freq, "Frequency", ACCENT, false);
                knob(ui, setter, &b.gain, "Gain", ACCENT, true);
                knob(ui, setter, &b.q, "Width (Q)", ACCENT, false);
            });
            note("Mid is what both speakers share: kick, bass, lead vocal. Side is what differs: width, reverb, wide synths.\n\nTry a low cut on the Side below 100 Hz, or a high shelf on the Side for air.\n\nThe curves follow analog EQ shapes right up to 20 kHz.");
        }
        Module::Balance => {
            c.horizontal(|ui| {
                knob(ui, setter, &params.bal_amount, "Amount", ACCENT, false);
                ui.add_space(6.0);
                for (i, b) in params.bal().into_iter().enumerate() {
                    let hz = BAL_HZ[i];
                    let label = if hz >= 1000.0 { format!("{}k", hz / 1000.0) } else { format!("{hz:.0}") };
                    fader(ui, setter, b, &label, ACCENT, 170.0);
                }
            });
            note("Set by Auto Master from how your mix compares with the genre. The shaded band in the display above is the target.\n\nAmount at 0% takes it out; drag any fader to taste.");
        }
        Module::Glue => {
            c.horizontal(|ui| {
                knob(ui, setter, &params.glue_thresh, "Threshold", ACCENT, false);
                knob(ui, setter, &params.glue_ratio, "Ratio", ACCENT, false);
                knob(ui, setter, &params.glue_attack, "Attack", ACCENT, false);
                knob(ui, setter, &params.glue_release, "Release", ACCENT, false);
            });
            c.horizontal(|ui| {
                knob(ui, setter, &params.glue_makeup, "Makeup", ACCENT, false);
                knob(ui, setter, &params.glue_hp, "SC low cut", ACCENT, false).on_hover_text("Keeps the kick and 808 from pumping the whole mix");
                knob(ui, setter, &params.glue_mix, "Mix", ACCENT, false);
                ui.vertical(|ui| {
                    ui.add_space(12.0);
                    pill(ui, setter, &params.glue_auto, "Auto release", ACCENT);
                });
            });
            gr_bars(&vp, visual, &[("Glue", ld(&sh.glue_gr), ACCENT)], 8.0);
            vp.text(pos2(visual.left() + 14.0, visual.bottom() - 14.0), Align2::LEFT_BOTTOM, "1-2 dB on the loud parts is plenty.", regular(11.0), DIM);
        }
        Module::Dynamics => {
            c.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                for (i, name) in MB_NAMES.iter().enumerate() {
                    if chip(ui, name, st.band_sel == i, BAND_COLORS[i]).clicked() {
                        st.band_sel = i;
                    }
                }
            });
            let b = &params.band[st.band_sel];
            let col = BAND_COLORS[st.band_sel];
            c.horizontal(|ui| {
                knob(ui, setter, &b.thresh, "Threshold", col, false);
                knob(ui, setter, &b.ratio, "Ratio", col, false);
                knob(ui, setter, &b.attack, "Attack", col, false);
                knob(ui, setter, &b.release, "Release", col, false);
                knob(ui, setter, &b.gain, "Gain", col, true);
            });
            caption(&mut c, "CROSSOVERS (SHARED WITH EXCITER AND IMAGER)");
            c.horizontal(|ui| {
                knob(ui, setter, &params.x_lo, "Low", ACCENT, false);
                knob(ui, setter, &params.x_mid, "Mid", ACCENT, false);
                knob(ui, setter, &params.x_hi, "High", ACCENT, false);
            });
            let rows: Vec<(&str, f32, Color32)> = (0..MB_BANDS).map(|i| (MB_NAMES[i], ld(&sh.mb_gr[i]), BAND_COLORS[i])).collect();
            gr_bars(&vp, visual, &rows, 8.0);
        }
        Module::Exciter => {
            segmented(&mut c, setter, &params.exc_mode, ACCENT, 260.0);
            c.add_space(4.0);
            c.horizontal(|ui| {
                for i in 0..MB_BANDS {
                    knob(ui, setter, &params.band[i].excite, MB_NAMES[i], BAND_COLORS[i], false);
                }
            });
            note("Tube: thick and warm, good low down.\nTape: glue and weight everywhere.\nCrisp: edge and shine for the top band.\n\nA little goes a long way: 10-25%. Runs oversampled, so the harmonics don't fold back as fizz.");
        }
        Module::Imager => {
            c.horizontal(|ui| {
                for i in 0..MB_BANDS {
                    knob(ui, setter, &params.band[i].width, MB_NAMES[i], BAND_COLORS[i], false);
                }
            });
            c.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add_space(12.0);
                    pill(ui, setter, &params.mono_bass, "Mono bass", ACCENT);
                });
                knob(ui, setter, &params.mono_freq, "Below", ACCENT, false);
            });
            note("100% leaves a band as it is, 0% makes it mono, above 100% widens it.\n\nWiden the top, never the bottom: bass that differs left and right disappears on club systems and phones. Watch the correlation meter on the right stay above zero.");
        }
        Module::Maximizer => {
            c.horizontal(|ui| {
                knob(ui, setter, &params.max_gain, "Gain", ACCENT, false);
                knob(ui, setter, &params.ceiling, "Ceiling", ACCENT, false);
                knob(ui, setter, &params.clip, "Soft clip", ACCENT, false).on_hover_text("Rounds off drum peaks before the limiter: louder with more punch. 30-60% for rap.");
                knob(ui, setter, &params.max_release, "Release", ACCENT, false);
                knob(ui, setter, &params.max_character, "Character", ACCENT, false).on_hover_text("Low: fast and transparent. High: slower on dense passages, thicker and smoother.");
            });
            c.horizontal(|ui| {
                pill(ui, setter, &params.true_peak, "True peak", ACCENT).on_hover_text("Catches the peaks between samples, which is what streaming services measure");
            });
            caption(&mut c, "Raise Gain until the loudness on the right reaches your target. More than 4-5 dB of steady limiting starts to cost punch.");
            gr_bars(&vp, visual, &[("Limiter", ld(&sh.max_gr), ACCENT), ("Soft clip", ld(&sh.clip_gr), WARN)], 10.0);
            let target = params.target.value();
            let now = ld(&sh.short_term);
            if now > -60.0 {
                let d = now - target;
                let text = if d.abs() < 0.8 { "On target".to_string() } else { format!("{:.1} LU {} target", d.abs(), if d > 0.0 { "over" } else { "under" }) };
                vp.text(pos2(visual.left() + 14.0, visual.bottom() - 14.0), Align2::LEFT_BOTTOM, text, semi(12.0), if d.abs() < 0.8 { GOOD } else { DIM });
            }
        }
        Module::Output => {
            c.horizontal(|ui| {
                knob(ui, setter, &params.input, "Input", ACCENT, true);
                knob(ui, setter, &params.output, "Output", ACCENT, false);
                ui.vertical(|ui| {
                    caption(ui, "DITHER");
                    combo(ui, setter, &params.dither, 90.0);
                    pill(ui, setter, &params.dither_shape, "Noise shaping", ACCENT);
                });
            });
            caption(&mut c, "Dither only when you export to 16 or 24-bit, and only once, here at the very end.");
            note("Put Persona Master last on the master channel.\n\nLeave headroom going in: peaks around -6 dBFS are ideal. The maximizer makes up the level.");
        }
    }
}

// ---------------------------------------------------------------- meters

fn meters_panel(ui: &mut Ui, st: &mut State) {
    let r = ui.max_rect();
    let sh = st.shared.clone();
    let target = st.params.target.value();
    let ceiling = st.params.ceiling.value();
    let loud = Rect::from_min_size(r.min, vec2(r.width(), 300.0));
    let gr = Rect::from_min_size(pos2(r.left(), loud.bottom() + 12.0), vec2(r.width(), 118.0));
    let scope = Rect::from_min_max(pos2(r.left(), gr.bottom() + 12.0), r.max);
    let p = ui.painter().clone();

    // ---- loudness
    card(&p, loud);
    p.text(loud.left_top() + vec2(16.0, 20.0), Align2::LEFT_CENTER, "Loudness", semi(14.0), TEXT);
    let reset = Rect::from_min_size(pos2(loud.right() - 62.0, loud.top() + 9.0), vec2(50.0, 22.0));
    if button(&mut child(ui, reset), "Reset", vec2(50.0, 22.0), RAISED, DIM).on_hover_text("Start the integrated loudness and the peak hold over").clicked() {
        sh.reset_meters.store(true, Ordering::Relaxed);
    }
    let int = ld(&sh.integrated);
    let fmt = |v: f32| if v < -70.0 { "--.-".to_string() } else { format!("{v:.1}") };
    let near = (int - target).abs() < 1.0;
    p.text(pos2(loud.left() + 16.0, loud.top() + 62.0), Align2::LEFT_CENTER, fmt(int), heavy(34.0), if int < -70.0 { DIM } else if near { GOOD } else { TEXT });
    p.text(pos2(loud.right() - 16.0, loud.top() + 54.0), Align2::RIGHT_CENTER, "LUFS", semi(11.0), DIM);
    p.text(pos2(loud.right() - 16.0, loud.top() + 70.0), Align2::RIGHT_CENTER, "integrated", regular(10.5), FAINT);
    let top = loud.top() + 96.0;
    let bottom = loud.bottom() - 62.0;
    let y_of = |l: f32| bottom - ((l + 36.0) / 33.0).clamp(0.0, 1.0) * (bottom - top);
    for l in [-6.0, -10.0, -14.0, -20.0, -30.0] {
        let y = y_of(l);
        p.line_segment([pos2(loud.left() + 40.0, y), pos2(loud.right() - 16.0, y)], stroke(1.0, alpha(Color32::WHITE, 0.05)));
        grid_label(&p, pos2(loud.left() + 34.0, y), &format!("{l:.0}"), Align2::RIGHT_CENTER);
    }
    let bw = (loud.width() - 56.0 - 12.0) / 2.0;
    for (i, (label, v)) in [("Short", ld(&sh.short_term)), ("Moment", ld(&sh.momentary))].iter().enumerate() {
        let x = loud.left() + 40.0 + i as f32 * (bw + 12.0);
        let track = Rect::from_min_max(pos2(x, top), pos2(x + bw, bottom));
        p.rect_filled(track, 5, alpha(Color32::WHITE, 0.03));
        let y = y_of(*v);
        if *v > -36.0 {
            let col = if *v > target + 1.5 { WARN } else { ACCENT };
            p.rect_filled(Rect::from_min_max(pos2(x, y), pos2(x + bw, bottom)), 5, col);
        }
        p.text(pos2(x + bw / 2.0, bottom + 10.0), Align2::CENTER_CENTER, format!("{label} {}", fmt(*v)), regular(10.0), DIM);
    }
    let ty = y_of(target);
    p.line_segment([pos2(loud.left() + 36.0, ty), pos2(loud.right() - 12.0, ty)], stroke(1.5, Color32::WHITE));
    grid_label(&p, pos2(loud.right() - 14.0, ty - 2.0), "target", Align2::RIGHT_BOTTOM);
    let tp = ld(&sh.true_peak);
    let y = loud.bottom() - 28.0;
    p.text(pos2(loud.left() + 16.0, y), Align2::LEFT_CENTER, "True peak", regular(11.0), DIM);
    p.text(pos2(loud.right() - 16.0, y), Align2::RIGHT_CENTER, format!("{} dBTP", fmt(tp)), semi(12.0), if tp > ceiling + 0.3 { HOT } else { TEXT });
    p.text(pos2(loud.left() + 16.0, y + 16.0), Align2::LEFT_CENTER, "Range", regular(11.0), DIM);
    p.text(pos2(loud.right() - 16.0, y + 16.0), Align2::RIGHT_CENTER, format!("{:.1} LU", ld(&sh.range)), semi(12.0), TEXT);

    // ---- limiting, scrolling
    card(&p, gr);
    p.text(gr.left_top() + vec2(16.0, 20.0), Align2::LEFT_CENTER, "Limiting", semi(14.0), TEXT);
    let now = st.gr.back().copied().unwrap_or(0.0);
    p.text(pos2(gr.right() - 16.0, gr.top() + 20.0), Align2::RIGHT_CENTER, format!("{:.1} dB", if now < 0.05 { 0.0 } else { -now }), semi(12.5), if now > 6.0 { WARN } else { TEXT });
    let plot = Rect::from_min_max(pos2(gr.left() + 14.0, gr.top() + 38.0), pos2(gr.right() - 14.0, gr.bottom() - 12.0));
    p.rect_filled(plot, 6, WELL);
    let pts: Vec<Pos2> = st
        .gr
        .iter()
        .enumerate()
        .map(|(i, v)| pos2(plot.right() - (st.gr.len() - 1 - i) as f32 / 239.0 * plot.width(), plot.top() + (v / 10.0).clamp(0.0, 1.0) * plot.height()))
        .collect();
    if pts.len() > 1 {
        let mut mesh = Mesh::default();
        for (i, q) in pts.iter().enumerate() {
            mesh.colored_vertex(pos2(q.x, plot.top()), alpha(ACCENT, 0.05));
            mesh.colored_vertex(*q, alpha(ACCENT, 0.35));
            if i > 0 {
                let k = (i as u32) * 2;
                mesh.add_triangle(k - 2, k - 1, k);
                mesh.add_triangle(k - 1, k, k + 1);
            }
        }
        p.add(Shape::mesh(mesh));
        p.add(Shape::line(pts, stroke(1.6, ACCENT)));
    }

    // ---- stereo
    card(&p, scope);
    p.text(scope.left_top() + vec2(16.0, 20.0), Align2::LEFT_CENTER, "Stereo", semi(14.0), TEXT);
    let corr = ld(&sh.correlation);
    let cb = Rect::from_min_max(pos2(scope.left() + 16.0, scope.bottom() - 26.0), pos2(scope.right() - 16.0, scope.bottom() - 20.0));
    p.rect_filled(cb, 3, alpha(Color32::WHITE, 0.07));
    let cx = cb.center().x + corr * cb.width() * 0.5;
    p.rect_filled(Rect::from_x_y_ranges(cb.center().x.min(cx)..=cb.center().x.max(cx), cb.y_range()), 3, if corr < 0.0 { HOT } else { ACCENT });
    p.line_segment([pos2(cb.center().x, cb.top() - 3.0), pos2(cb.center().x, cb.bottom() + 3.0)], stroke(1.0, FAINT));
    grid_label(&p, pos2(cb.left(), cb.bottom() + 3.0), "-1 out of phase", Align2::LEFT_TOP);
    grid_label(&p, pos2(cb.right(), cb.bottom() + 3.0), "mono +1", Align2::RIGHT_TOP);
    let area = Rect::from_min_max(pos2(scope.left() + 14.0, scope.top() + 38.0), pos2(scope.right() - 14.0, cb.top() - 10.0));
    p.rect_filled(area, 6, WELL);
    let c = area.center();
    let rad = area.width().min(area.height()) * 0.46;
    p.line_segment([c - vec2(rad, rad), c + vec2(rad, rad)], stroke(1.0, alpha(Color32::WHITE, 0.05)));
    p.line_segment([c + vec2(-rad, rad), c + vec2(rad, -rad)], stroke(1.0, alpha(Color32::WHITE, 0.05)));
    p.line_segment([c - vec2(0.0, rad), c + vec2(0.0, rad)], stroke(1.0, alpha(Color32::WHITE, 0.08)));
    let mut l = [0.0f32; 512];
    let mut rr = [0.0f32; 512];
    sh.scope[0].read(&mut l);
    sh.scope[1].read(&mut rr);
    let peak = l.iter().chain(rr.iter()).fold(0.05f32, |m, v| m.max(v.abs()));
    let pc = p.with_clip_rect(area);
    for (a, b) in l.iter().zip(rr.iter()) {
        // Mid up, side across.
        let (x, y) = ((a - b) * 0.707 / peak, (a + b) * 0.707 / peak);
        pc.circle_filled(c + vec2(x * rad, -y * rad), 1.0, alpha(ACCENT, 0.5));
    }
}

// ---------------------------------------------------------------- windows

fn save_window(ctx: &egui::Context, st: &mut State) {
    let mut open = true;
    let mut done = false;
    egui::Window::new(egui::RichText::new("Save preset").font(semi(14.0))).open(&mut open).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, vec2(0.0, -60.0)).show(ctx, |ui| {
        ui.label(egui::RichText::new("Saves every setting except input level, genre, target and dither.").color(DIM));
        ui.add_space(6.0);
        ui.add(egui::TextEdit::singleline(&mut st.save_name).desired_width(300.0).font(regular(14.0))).request_focus();
        ui.add_space(6.0);
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.horizontal(|ui| {
            if button(ui, "Save", vec2(80.0, 30.0), ACCENT, INK).clicked() || enter {
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

fn settings_window(ctx: &egui::Context, st: &mut State) {
    let mut open = true;
    egui::Window::new(egui::RichText::new("Persona Master").font(semi(14.0))).open(&mut open).collapsible(false).resizable(false).default_width(360.0).anchor(Align2::RIGHT_TOP, vec2(-14.0, 64.0)).show(ctx, |ui| {
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
        if let Some(d) = preset_dir() {
            ui.label(egui::RichText::new(format!("Your presets: {}", d.display())).font(regular(11.0)).color(FAINT));
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "Loudness is measured to ITU-R BS.1770-4 / EBU R128. The genre targets are our own reading of finished records, \
                     as starting points. Persona and Persona Master update together. Inter typeface by Rasmus Andersson (OFL).",
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
    fn every_module_has_a_name_lookup() {
        for m in MODULES {
            assert_eq!(Module::by_name(m.label()), Some(m));
        }
    }

    #[test]
    fn presets_only_name_real_parameters() {
        let p = MasterParams::default();
        let ids: Vec<String> = p.param_map().into_iter().map(|(id, _, _)| id).collect();
        for preset in PRESETS {
            for (id, _) in preset.values {
                assert!(ids.iter().any(|k| k == id), "{}: {id}", preset.name);
            }
        }
        for id in KEEP_ON_PRESET {
            assert!(ids.iter().any(|k| k == id), "{id}");
        }
    }
}
