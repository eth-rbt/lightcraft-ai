//! What the agent did, over the loupe: a bar that names the agent step the photo's settings are
//! at and steps Back / Advance through them, and that step drawn on the photo (the masks it
//! made, the crop it set, the sliders it moved). The steps are the engine's agent step log
//! (`lightcraft_engine::agent`); everything on the photo is placed through the view's
//! [`CanvasMap`], so it follows the window size, zoom and pan.

use lightcraft_develop::MaskShape;
use lightcraft_engine::agent::{AgentStep, Position};

use super::masks::{component_pin, frame_long_norm, linear_guide};
use super::{CanvasMap, Color32, LightcraftApp, Point, Pos2, Rect, Sense, Stroke, StrokeKind, Tokens, json, pos2, register, vec2};
use crate::widgets::{AGENT, AGENT_INK};

/// Slider changes named in the bar and listed on the photo before "+N more".
const NAMED_CHANGES: usize = 3;
const LISTED_CHANGES: usize = 5;

/// The steps of the active photo, and which of them its settings are at.
struct Steps {
    own: Vec<AgentStep>,
    position: Position,
}

impl Steps {
    fn of(app: &LightcraftApp) -> Option<Steps> {
        let id = app.session.active()?;
        let own: Vec<AgentStep> = app.session.agent.steps().iter().filter(|s| s.photo == id).cloned().collect();
        if own.is_empty() {
            return None;
        }
        let position = app.session.agent.position(id, &app.session.develop_of(id)?);
        Some(Steps { own, position })
    }

    /// The index of the step the settings are at.
    fn current(&self) -> Option<usize> {
        match self.position {
            Position::After(id) => self.own.iter().position(|s| s.id == id),
            _ => None,
        }
    }

    /// Where Back goes: the step before, or the settings the first step started from.
    fn back(&self) -> Option<(u64, &'static str)> {
        match self.current()? {
            0 => self.own.first().map(|s| (s.id, "before")),
            i => self.own.get(i - 1).map(|s| (s.id, "after")),
        }
    }

    /// Where Advance goes: the next step; from settings edited since, the latest step.
    fn advance(&self) -> Option<u64> {
        match (self.position, self.current()) {
            (Position::Start, _) => self.own.first().map(|s| s.id),
            (_, Some(i)) => self.own.get(i + 1).map(|s| s.id),
            _ => self.own.last().map(|s| s.id),
        }
    }
}

/// The sliders the step of the active photo's current settings moved: control id → the value
/// before. The Edit panel's sliders mark these ([`crate::widgets::set_agent_marks`]).
pub(crate) fn slider_marks(app: &LightcraftApp) -> std::collections::BTreeMap<String, f64> {
    let Some(steps) = Steps::of(app) else { return Default::default() };
    let Some(step) = steps.current().and_then(|i| steps.own.get(i)) else { return Default::default() };
    step.control_changes().into_iter().map(|c| (c.id, c.from)).collect()
}

/// One line saying what a step changed.
fn summary(step: &AgentStep) -> String {
    let tr = crate::i18n::tr;
    let changes = step.control_changes();
    let mut parts: Vec<String> = changes.iter().take(NAMED_CHANGES).map(|c| format!("{} {} → {}", tr(c.label), c.from_text, c.to_text)).collect();
    if changes.len() > NAMED_CHANGES {
        parts.push(format!("+{}", changes.len() - NAMED_CHANGES));
    }
    if step.crop_changed() {
        parts.push(tr("Crop").to_string());
    }
    let masks = step.mask_changes();
    for (ids, what) in [(&masks.added, "Mask added"), (&masks.changed, "Mask changed"), (&masks.removed, "Mask removed")] {
        if !ids.is_empty() {
            parts.push(tr(what).to_string());
        }
    }
    if parts.is_empty() || step.other_changes() {
        parts.push(tr("Other settings").to_string());
    }
    parts.join("  ·  ")
}

/// A painted button of the bar. Returns whether it was clicked.
fn bar_button(ui: &mut egui::Ui, r: Rect, key: &str, label: &str, enabled: bool, primary: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let resp = ui.interact(r, egui::Id::unique(key), if enabled { Sense::click() } else { Sense::hover() });
    crate::widgets::announce(&resp, label);
    register(ui, key, r);
    let p = ui.painter();
    let (fill, text) = match (enabled, primary) {
        (false, _) => (Color32::from_white_alpha(14), Color32::from_gray(110)),
        (true, true) => (if resp.hovered() { AGENT.gamma_multiply(1.1) } else { AGENT }, AGENT_INK),
        (true, false) => (Color32::from_white_alpha(if resp.hovered() { 60 } else { 34 }), Color32::from_gray(235)),
    };
    p.rect_filled(r, 5.0, fill);
    p.text(r.center(), egui::Align2::CENTER_CENTER, label, t.font(12.5), text);
    enabled && resp.clicked()
}

/// The bar at the foot of the canvas. Returns its rectangle.
fn bar(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, steps: &Steps) -> Rect {
    let t = Tokens::get(ui.ctx());
    let tr = crate::i18n::tr;
    let w = (canvas.width() - 32.0).clamp(120.0, 660.0);
    let r = Rect::from_min_size(pos2(canvas.center().x - w / 2.0, canvas.bottom() - 14.0 - 62.0), vec2(w, 62.0));
    // the bar takes the pointer from the loupe under it: a click on it must not zoom the photo
    let resp = ui.interact(r, egui::Id::unique("agent-bar"), Sense::click());
    register(ui, "agent:bar", r);
    let p = ui.painter_at(canvas);
    p.rect_filled(r, 8.0, Color32::from_black_alpha(215));
    p.rect_stroke(r, 8.0, Stroke::new(1.0, AGENT.gamma_multiply(0.55)), StrokeKind::Inside);

    let current = steps.current().and_then(|i| steps.own.get(i).map(|s| (i, s)));
    let (head, line, note) = match (current, steps.position) {
        (Some((i, step)), _) => {
            let head = format!("{} {} / {}", tr("Agent step"), i + 1, steps.own.len()).to_uppercase();
            (format!("{head}  ·  {}", step.tool), summary(step), step.note.clone().unwrap_or_default())
        }
        (None, Position::Start) => {
            (tr("Before the agent's edits").to_uppercase(), format!("{} {}", steps.own.len(), tr("agent steps")), String::new())
        }
        _ => (tr("Edited since the agent's steps").to_uppercase(), format!("{} {}", steps.own.len(), tr("agent steps")), String::new()),
    };
    let (button_w, close_w, gap) = (72.0, 22.0, 6.0);
    let buttons_left = r.right() - 10.0 - close_w - gap - 2.0 * button_w - gap;
    let text_clip = Rect::from_min_max(r.min, pos2(buttons_left - 8.0, r.max.y));
    let text = p.with_clip_rect(text_clip);
    let left = r.left() + 12.0;
    text.text(pos2(left, r.top() + 13.0), egui::Align2::LEFT_CENTER, &head, t.font(10.5), AGENT);
    text.text(pos2(left, r.top() + 31.0), egui::Align2::LEFT_CENTER, &line, t.font(13.0), Color32::from_gray(240));
    text.text(pos2(left, r.top() + 48.0), egui::Align2::LEFT_CENTER, &note, t.font(12.0), Color32::from_gray(175));
    if !note.is_empty() || !line.is_empty() {
        resp.on_hover_text(if note.is_empty() { line } else { format!("{line}\n{note}") });
    }

    let button = |i: f32| Rect::from_min_size(pos2(buttons_left + i * (button_w + gap), r.center().y - 14.0), vec2(button_w, 28.0));
    let (back, advance) = (steps.back(), steps.advance());
    if bar_button(ui, button(0.0), "agent:back", tr("Back"), back.is_some(), false)
        && let Some((step, side)) = back
    {
        let _ = app.run("agent.steps.goto", json!({"step": step, "side": side}));
    }
    if bar_button(ui, button(1.0), "agent:next", tr("Advance"), advance.is_some(), true)
        && let Some(step) = advance
    {
        let _ = app.run("agent.steps.goto", json!({"step": step}));
    }
    let close = Rect::from_center_size(pos2(r.right() - 10.0 - close_w / 2.0, r.center().y), vec2(close_w, close_w));
    let close_resp = ui.interact(close, egui::Id::unique("agent:close"), Sense::click());
    crate::widgets::announce(&close_resp, tr("Hide agent steps"));
    register(ui, "agent:close", close);
    let p = ui.painter_at(canvas);
    let cross = Stroke::new(1.3, if close_resp.hovered() { Color32::WHITE } else { Color32::from_gray(170) });
    let (c, m) = (close.center(), 3.5);
    p.line_segment([c - vec2(m, m), c + vec2(m, m)], cross);
    p.line_segment([c - vec2(m, -m), c + vec2(m, -m)], cross);
    if close_resp.on_hover_text(tr("Hide the agent's steps (the edits stay)")).clicked() {
        let _ = app.run("agent.steps.clear", json!({}));
    }
    r
}

/// A small filled label.
fn chip(p: &egui::Painter, t: &Tokens, at: Pos2, lines: &[String], within: Rect) -> Rect {
    let galleys: Vec<_> = lines.iter().map(|l| p.layout_no_wrap(l.clone(), t.font(12.0), AGENT_INK)).collect();
    let w = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + 14.0;
    let h = 17.0 * galleys.len() as f32 + 8.0;
    // kept on screen: a pin near the photo's edge still shows its whole label
    let min = pos2(at.x.min(within.right() - w).max(within.left()), at.y.min(within.bottom() - h).max(within.top()));
    let r = Rect::from_min_size(min, vec2(w, h));
    p.rect_filled(r, 4.0, AGENT);
    for (i, g) in galleys.into_iter().enumerate() {
        p.galley(pos2(r.left() + 7.0, r.top() + 4.0 + 17.0 * i as f32), g, AGENT_INK);
    }
    r
}

/// A line with a dark keyline under it, so it reads over any photo.
fn keyed_line(p: &egui::Painter, pts: Vec<Pos2>, closed: bool, dashed: bool) {
    let shape = |stroke: Stroke| match (closed, dashed) {
        (true, _) => vec![egui::Shape::closed_line(pts.clone(), stroke)],
        (false, true) => egui::Shape::dashed_line(&pts, stroke, 7.0, 5.0),
        (false, false) => vec![egui::Shape::line(pts.clone(), stroke)],
    };
    p.extend(shape(Stroke::new(4.0, Color32::from_black_alpha(150))));
    p.extend(shape(Stroke::new(2.0, AGENT)));
}

/// The masks a step added or changed: each component's outline (gradient lines, the ellipse of a
/// radial) or a pin where it has no simple outline, with the mask's name.
fn masks(ui: &egui::Ui, p: &egui::Painter, t: &Tokens, map: &CanvasMap, step: &AgentStep, visible: Rect) {
    let changes = step.mask_changes();
    for mask in step.after.masks.iter().filter(|m| changes.added.contains(&m.id) || changes.changed.contains(&m.id)) {
        let map = map.for_mask(mask.on_photo);
        let mut label_at = None;
        for component in &mask.components {
            match &component.shape {
                MaskShape::Linear { start, end } => {
                    for at in [*start, *end] {
                        for run in linear_guide(&map, *start, *end, at) {
                            keyed_line(p, run, false, true);
                        }
                    }
                    keyed_line(p, vec![map.screen(*start), map.screen(*end)], false, false);
                }
                MaskShape::Radial { center, rx, ry, angle, .. } => {
                    let l = frame_long_norm(&map);
                    let (s, co) = angle.to_radians().sin_cos();
                    let pts = (0..64)
                        .map(|i| {
                            let a = f64::from(i) / 64.0 * std::f64::consts::TAU;
                            let (x, y) = (rx * a.cos(), ry * a.sin());
                            map.screen(Point::new(center.x + (x * co - y * s) * l.0, center.y + (x * s + y * co) * l.1))
                        })
                        .collect();
                    keyed_line(p, pts, true, false);
                }
                _ => {}
            }
            if let Some(pin) = component_pin(&component.shape).map(|n| map.screen(n)) {
                p.circle_filled(pin, 6.5, AGENT);
                p.circle_stroke(pin, 6.5, Stroke::new(2.0, AGENT_INK));
                label_at.get_or_insert(pin);
            }
        }
        let what = if changes.added.contains(&mask.id) { "added" } else { "changed" };
        let at = label_at.map_or(visible.center(), |pin| pin + vec2(12.0, -11.0));
        let r = chip(p, t, at, &[format!("{} · {}", mask.name, crate::i18n::tr(what))], visible);
        register(ui, format!("agent:mask:{}", mask.id), r);
    }
}

/// A crop is shown as a diagram, not on the photo: the loupe shows the photo already cropped, so
/// the frame the step set is the picture's own edge. The diagram is the whole frame with the crop
/// before (dashed) and after (filled) inside it.
fn crop(ui: &egui::Ui, p: &egui::Painter, t: &Tokens, step: &AgentStep, img_rect: Rect, visible: Rect) {
    let (from, to) = (step.before.crop.geometry, step.after.crop.geometry);
    let (rw, rh) = (to.rect.width().max(1e-6) as f32, to.rect.height().max(1e-6) as f32);
    // the whole frame's proportions, from the cropped picture on screen
    let aspect = (img_rect.width() / img_rect.height().max(1.0) * rh / rw).clamp(0.2, 5.0);
    let long = 132.0_f32.min(visible.width() * 0.4).max(40.0);
    let size = if aspect >= 1.0 { vec2(long, long / aspect) } else { vec2(long * aspect, long) };
    let frame = Rect::from_min_size(pos2(visible.right() - size.x - 14.0, visible.top() + 14.0), size);
    let back = frame.expand(7.0);
    p.rect_filled(back, 6.0, Color32::from_black_alpha(205));
    p.rect_stroke(frame, 0.0, Stroke::new(1.0, Color32::from_gray(150)), StrokeKind::Inside);
    let inside = |r: lightcraft_geom::Rect| {
        let at =
            |x: f64, y: f64| pos2(frame.left() + x.clamp(0.0, 1.0) as f32 * frame.width(), frame.top() + y.clamp(0.0, 1.0) as f32 * frame.height());
        Rect::from_two_pos(at(r.x0, r.y0), at(r.x1, r.y1))
    };
    let was = inside(from.rect);
    p.extend(egui::Shape::dashed_line(
        &[was.left_top(), was.right_top(), was.right_bottom(), was.left_bottom(), was.left_top()],
        Stroke::new(1.0, Color32::from_gray(210)),
        4.0,
        3.0,
    ));
    let now = inside(to.rect);
    p.rect_filled(now, 0.0, AGENT.gamma_multiply(0.3));
    p.rect_stroke(now, 0.0, Stroke::new(2.0, AGENT), StrokeKind::Inside);
    let mut label = crate::i18n::tr("New crop").to_string();
    if (from.angle - to.angle).abs() > 1e-6 {
        label.push_str(&format!("  ·  {:+.1}°", to.angle));
    }
    chip(p, t, pos2(back.left(), back.bottom() + 4.0), &[label], visible);
    register(ui, "agent:crop", back);
}

/// The sliders a step moved, listed at the photo's top left.
fn changes(ui: &egui::Ui, p: &egui::Painter, t: &Tokens, step: &AgentStep, visible: Rect) {
    let all = step.control_changes();
    if all.is_empty() {
        return;
    }
    let mut lines: Vec<String> =
        all.iter().take(LISTED_CHANGES).map(|c| format!("{}  {} → {}", crate::i18n::tr(c.label), c.from_text, c.to_text)).collect();
    if all.len() > LISTED_CHANGES {
        lines.push(format!("+{}", all.len() - LISTED_CHANGES));
    }
    let r = chip(p, t, visible.min + vec2(12.0, 12.0), &lines, visible);
    register(ui, "agent:changes", r);
}

/// The bar, and the step the photo's settings are at drawn over the photo.
pub(super) fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, map: &CanvasMap, canvas: Rect, img_rect: Rect) {
    let Some(steps) = Steps::of(app) else { return };
    let t = Tokens::get(ui.ctx());
    let bar_rect = bar(app, ui, canvas, &steps);
    let Some(step) = steps.current().and_then(|i| steps.own.get(i)) else { return };
    // the part of the photo on screen, clear of the bar
    let visible = img_rect.intersect(Rect::from_min_max(canvas.min, pos2(canvas.right(), bar_rect.top() - 6.0)));
    if !visible.is_positive() {
        return;
    }
    let p = ui.painter_at(visible);
    masks(ui, &p, &t, map, step, visible);
    changes(ui, &p, &t, step, visible);
    if step.crop_changed() {
        crop(ui, &p, &t, step, img_rect, visible);
    }
}
