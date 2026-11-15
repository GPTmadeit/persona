//! What both editors share: the palette, the type, the drawing helpers and the widgets.

use nih_plug::prelude::*;
use nih_plug_egui::egui::{
    self, epaint::Mesh, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Painter, Pos2, Rect,
    Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};
use std::f32::consts::PI;
use std::sync::Arc;

// ---------------------------------------------------------------- palette and type

pub(crate) const BG: Color32 = Color32::from_rgb(11, 12, 17);
pub(crate) const SURFACE: Color32 = Color32::from_rgb(19, 21, 29);
pub(crate) const RAISED: Color32 = Color32::from_rgb(28, 31, 42);
pub(crate) const HOVER: Color32 = Color32::from_rgb(37, 41, 55);
pub(crate) const WELL: Color32 = Color32::from_rgb(13, 14, 20);
pub(crate) const EDGE: Color32 = Color32::from_rgb(42, 46, 61);
pub(crate) const TEXT: Color32 = Color32::from_rgb(237, 239, 246);
pub(crate) const DIM: Color32 = Color32::from_rgb(148, 154, 174);
pub(crate) const FAINT: Color32 = Color32::from_rgb(86, 92, 112);
pub(crate) const GOOD: Color32 = Color32::from_rgb(84, 222, 154);
pub(crate) const WARN: Color32 = Color32::from_rgb(255, 192, 84);
pub(crate) const HOT: Color32 = Color32::from_rgb(255, 94, 112);

pub(crate) fn stroke(width: f32, color: Color32) -> Stroke {
    Stroke::new(width, color)
}

pub(crate) fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub(crate) fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub(crate) fn heavy(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("black".into()))
}

pub(crate) fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub(crate) fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub(crate) fn alpha(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// A style color made bright enough to work as an accent on the dark surfaces.
pub(crate) fn accent_of(c: u32) -> Color32 {
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

pub(crate) fn setup_fonts(ctx: &egui::Context) {
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

pub(crate) fn child(ui: &mut Ui, rect: Rect) -> Ui {
    ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)))
}

// ---------------------------------------------------------------- drawing helpers

pub(crate) fn card(p: &Painter, r: Rect) {
    p.rect_filled(r, 14, SURFACE);
    p.rect_stroke(r, 14, stroke(1.0, alpha(EDGE, 0.7)), StrokeKind::Inside);
}

/// A rounded rectangle filled with a linear gradient along `dir`.
pub(crate) fn gradient_rect(p: &Painter, r: Rect, radius: f32, c0: Color32, c1: Color32, dir: Vec2) {
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
pub(crate) fn glow(p: &Painter, c: Pos2, r: f32, color: Color32, strength: f32) {
    for i in 0..6 {
        let t = i as f32 / 6.0;
        p.circle_filled(c, r * (1.0 + t * 0.9), alpha(color, strength * 0.09 * (1.0 - t)));
    }
}

pub(crate) fn arc(p: &Painter, c: Pos2, r: f32, a: f32, b: f32, stroke: Stroke) {
    let n = ((b - a).abs() * r / 3.0).ceil().clamp(2.0, 96.0) as usize;
    let pts: Vec<Pos2> = (0..=n).map(|i| a + (b - a) * i as f32 / n as f32).map(|t| c + vec2(t.cos(), t.sin()) * r).collect();
    p.add(Shape::line(pts, stroke));
}

pub(crate) fn star(p: &Painter, c: Pos2, r: f32, filled: bool, color: Color32) {
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

pub(crate) fn chevron(p: &Painter, c: Pos2, left: bool, color: Color32) {
    let s = if left { -1.0 } else { 1.0 };
    p.add(Shape::line(vec![c + vec2(-3.0 * s, -5.0), c + vec2(3.0 * s, 0.0), c + vec2(-3.0 * s, 5.0)], stroke(2.0, color)));
}

pub(crate) fn gear(p: &Painter, c: Pos2, color: Color32) {
    for i in 0..8 {
        let a = i as f32 * PI / 4.0;
        p.line_segment([c + vec2(a.cos(), a.sin()) * 4.5, c + vec2(a.cos(), a.sin()) * 8.0], stroke(2.6, color));
    }
    p.circle_stroke(c, 5.0, stroke(2.2, color));
}

// ---------------------------------------------------------------- widgets

pub(crate) fn hover_cursor(r: Response) -> Response {
    r.on_hover_cursor(CursorIcon::PointingHand)
}

/// A flat button: `fill` behind, lighter on hover.
pub(crate) fn button(ui: &mut Ui, text: &str, size: Vec2, fill: Color32, color: Color32) -> Response {
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

pub(crate) fn icon_button(ui: &mut Ui, size: f32, draw: impl FnOnce(&Painter, Pos2, Color32)) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 8, alpha(Color32::WHITE, 0.08));
    }
    draw(p, r.center(), if resp.hovered() { TEXT } else { DIM });
    hover_cursor(resp)
}

pub(crate) fn chip(ui: &mut Ui, text: &str, selected: bool, accent: Color32) -> Response {
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
pub(crate) fn drag_param(ui: &Ui, setter: &ParamSetter, param: &FloatParam, resp: &Response, travel: f32) {
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

pub(crate) fn knob_face(p: &Painter, c: Pos2, r: f32, value: f32, from: f32, accent: Color32, active: bool, track: f32) {
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
pub(crate) fn knob(ui: &mut Ui, setter: &ParamSetter, param: &FloatParam, label: &str, accent: Color32, bipolar: bool) -> Response {
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
pub(crate) fn fader(ui: &mut Ui, setter: &ParamSetter, param: &FloatParam, label: &str, accent: Color32, height: f32) -> Response {
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
pub(crate) fn switch(ui: &mut Ui, setter: &ParamSetter, param: &BoolParam, accent: Color32) -> Response {
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
pub(crate) fn pill(ui: &mut Ui, setter: &ParamSetter, param: &BoolParam, label: &str, accent: Color32) -> Response {
    let on = param.value();
    let r = chip(ui, label, on, accent);
    if r.clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, !on);
        setter.end_set_parameter(param);
    }
    r.on_hover_text(param.name())
}

pub(crate) fn segmented<T: Enum + PartialEq + 'static>(ui: &mut Ui, setter: &ParamSetter, param: &EnumParam<T>, accent: Color32, width: f32) {
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

pub(crate) fn combo<T: Enum + PartialEq + 'static>(ui: &mut Ui, setter: &ParamSetter, param: &EnumParam<T>, width: f32) {
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

pub(crate) fn caption(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(semi(10.5)).color(FAINT));
}

pub(crate) fn grid_label(p: &Painter, pos: Pos2, text: &str, align: Align2) {
    p.text(pos, align, text, regular(9.5), FAINT);
}

/// The dark theme both windows use. `accent` colours selections.
pub(crate) fn apply_theme(ctx: &egui::Context, accent: Color32) {
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
    style.visuals.selection.bg_fill = alpha(accent, 0.35);
    style.visuals.selection.stroke = stroke(1.0, accent);
    style.spacing.item_spacing = vec2(8.0, 6.0);
    style.spacing.button_padding = vec2(10.0, 4.0);
    style.spacing.interact_size.y = 26.0;
    style.text_styles.insert(egui::TextStyle::Body, regular(13.0));
    style.text_styles.insert(egui::TextStyle::Button, semi(12.5));
    style.text_styles.insert(egui::TextStyle::Small, regular(11.0));
    style.text_styles.insert(egui::TextStyle::Heading, semi(17.0));
    ctx.set_style(style);
}
