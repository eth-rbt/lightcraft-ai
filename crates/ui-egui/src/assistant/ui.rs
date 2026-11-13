use egui::{Color32, Rect, RichText, Sense, Stroke, vec2};
use serde_json::json;

use super::{Mark, Tool, drawing, ink, visible};
use crate::{LightcraftApp, widgets::register};

const PAPER: Color32 = Color32::from_rgb(247, 242, 233);
const DIM: Color32 = Color32::from_rgb(168, 163, 154);
const ISLAND: Color32 = Color32::from_rgb(17, 17, 19);
const EDGE: Color32 = Color32::from_rgb(52, 52, 56);
const SELECTED: Color32 = Color32::from_rgb(62, 62, 68);
const TEAL: Color32 = Color32::from_rgb(57, 123, 119);
const TEAL_LIGHT: Color32 = Color32::from_rgb(108, 196, 188);
const ERROR: Color32 = Color32::from_rgb(238, 128, 104);

fn button(ui: &mut egui::Ui, id: &str, text: &str, selected: bool, enabled: bool) -> bool {
    let r = ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).color(PAPER).size(13.0))
            .fill(if selected { SELECTED } else { Color32::TRANSPARENT })
            .corner_radius(14)
            .min_size(vec2(28.0, 28.0)),
    );
    register(ui, id, r.rect);
    r.clicked()
}

/// The workspace switch: in the regular top bar while the island is away, and the first thing in
/// the island while it is down.
pub fn logo(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let expanded = visible(app);
    let tip = if expanded { "Close creative tools and stop voice" } else { "Open creative tools and voice" };
    let response = ui
        .add_enabled(
            app.session.active().is_some(),
            egui::Button::new(RichText::new("Lc").strong().size(if expanded { 14.0 } else { 16.0 }).color(if expanded { PAPER } else { TEAL }))
                .fill(if expanded { TEAL } else { Color32::TRANSPARENT })
                .corner_radius(if expanded { 14 } else { 4 })
                .min_size(if expanded { vec2(30.0, 28.0) } else { vec2(34.0, 32.0) }),
        )
        .on_hover_text(tip);
    register(ui, "assistant:logo", response.rect);
    crate::widgets::announce(&response, tip);
    if response.clicked() {
        let _ = app.run("assistant.toggle", json!({}));
    }
}

/// Run before the editor is laid out. The island floats over the editor and takes no room from
/// it, so the editor (and the image shared with a voice) is everything the window has left.
pub fn begin(app: &mut LightcraftApp, ui: &egui::Ui) {
    app.assistant.editor_rect = visible(app).then(|| ui.available_rect_before_wrap());
}

fn island_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("creative-island"))
}

fn ink_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("creative-annotation-input"))
}

/// The pill that drops down at the top middle of the window: the drawing tools, and under them
/// what the voice is doing and what it said.
fn island(app: &mut LightcraftApp, ctx: &egui::Context) {
    let room = (ctx.viewport_rect().width() - 64.0).max(120.0);
    egui::Area::new(island_layer().id)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, vec2(0.0, 8.0))
        .default_width(room + 40.0)
        .show(ctx, |ui| {
            *ui.visuals_mut() = egui::Visuals::dark();
            ui.visuals_mut().override_text_color = Some(PAPER);
            egui::Frame::NONE
                .fill(ISLAND)
                .stroke(Stroke::new(1.0, EDGE))
                .corner_radius(26)
                .inner_margin(egui::Margin::symmetric(18, 8))
                .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(96) })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(5.0, 5.0);
                    let row = egui::ScrollArea::horizontal()
                        .id_salt("creative-tools-scroll")
                        .max_width(room)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                logo(app, ui);
                                tools(app, ui);
                            })
                            .response
                            .rect
                            .width()
                        })
                        .inner
                        .min(room);
                    ui.set_max_width(row);
                    if app.assistant.tool == Tool::Text {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Label").size(12.0));
                            crate::text_field::TextField::singleline("assistant:label", &mut app.assistant.note)
                                .label("Annotation label")
                                .hint("Text to place on the photo")
                                .width((row - 150.0).clamp(80.0, 320.0))
                                .show(ui);
                            ui.label(RichText::new("Click to place").color(DIM).size(11.0));
                        });
                    }
                    said(app, ui);
                });
        });
    // The ink layer comes to the front of its order when it is drawn on; the island stays above it.
    ctx.move_to_top(island_layer());
}

/// The line that shows what the voice is doing and the words it heard or spoke back.
fn said(app: &LightcraftApp, ui: &mut egui::Ui) {
    let info = app.assistant.error.as_deref().unwrap_or_else(|| {
        if app.assistant.editing() {
            &app.assistant.action
        } else if !app.assistant.transcript.is_empty() {
            &app.assistant.transcript
        } else if !app.assistant.action.is_empty() {
            &app.assistant.action
        } else {
            "Draw on the photo · Marks stay with this photo and are not exported"
        }
    });
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(RichText::new(&app.assistant.status).color(TEAL_LIGHT).size(11.0));
        ui.add(egui::Label::new(RichText::new(info).color(if app.assistant.error.is_some() { ERROR } else { PAPER }).size(11.0)).truncate())
            .on_hover_text(info);
    });
}

fn tools(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    for (tool, id, label) in [
        (Tool::Pen, "pen", "Pencil"),
        (Tool::Highlighter, "highlighter", "Highlight"),
        (Tool::Eraser, "eraser", "Eraser"),
        (Tool::Text, "text", "Text"),
    ] {
        if button(ui, &format!("assistant:{id}"), label, app.assistant.tool == tool, true) {
            let _ = app.run("assistant.tool", json!({"tool": id}));
        }
    }
    ui.separator();
    for (name, color) in
        [("Graphite", [41, 41, 41, 255]), ("Teal", [57, 123, 119, 255]), ("Terracotta", [180, 94, 70, 255]), ("Indigo", [89, 102, 160, 255])]
    {
        let (rect, response) = ui.allocate_exact_size(vec2(24.0, 28.0), Sense::click());
        let ink = Color32::from_rgb(color[0], color[1], color[2]);
        ui.painter().circle_filled(rect.center(), 7.5, ink);
        // the ring keeps the graphite ink visible on the dark island
        ui.painter().circle_stroke(rect.center(), 7.5, Stroke::new(1.0, EDGE.gamma_multiply(1.6)));
        if app.assistant.color == color {
            ui.painter().circle_stroke(rect.center(), 10.5, Stroke::new(1.5, PAPER));
        }
        register(ui, format!("assistant:color:{name}"), rect);
        crate::widgets::announce(&response, name);
        if response.on_hover_text(name).clicked() {
            app.assistant.color = color;
        }
    }
    for width in [2.0, 4.0, 6.0, 8.0] {
        if button(ui, &format!("assistant:width:{width}"), &format!("{width}"), app.assistant.width == width, true) {
            app.assistant.width = width;
        }
    }
    ui.separator();
    let (undo, redo, clear) = app.assistant.ink().map(|i| (i.can_undo(), i.can_redo(), !i.marks.is_empty())).unwrap_or_default();
    for (id, label, enabled) in [("undo", "Undo", undo), ("redo", "Redo", redo), ("clear", "Clear", clear)] {
        if button(ui, &format!("assistant:{id}"), label, false, enabled) {
            let _ = app.run(&format!("assistant.{id}"), json!({}));
        }
    }
    // Only a build with its own voice transport has a connection to pause; an outside voice
    // (tools/lightcraft-chat) is held by its talk key.
    if app.services.start_voice.is_some() {
        ui.separator();
        let connected = app.assistant.voice.is_some();
        if button(ui, "assistant:voice", if connected { "Pause voice" } else { "Reconnect" }, connected, true) {
            let _ = app.run("assistant.voice", json!({}));
        }
    }
}

/// The pencil owns the pointer: a stroke is being drawn, or the pointer is on the ink layer over
/// the photo (not on the island, a menu or a popup above it, and not mid-drag from a slider).
pub(super) fn capturing(app: &LightcraftApp, ctx: &egui::Context) -> bool {
    if !drawing(app) {
        return false;
    }
    if app.assistant.pending.is_some() {
        return true;
    }
    if ctx.dragged_id().is_some_and(|id| Some(id) != app.assistant.overlay_id) {
        return false;
    }
    ctx.input(|i| i.pointer.interact_pos()).is_some_and(|pos| ctx.layer_id_at(pos) == Some(ink_layer()))
}

/// While the pencil owns the pointer, the editor under it must not see the pointer or the wheel:
/// the loupe reads them raw to pan and zoom, so a stroke would also move the photo. Keys still
/// reach it, and the panels around the photo keep working whenever the pointer is on them.
/// The original InputState is restored for the overlay and subsequent modal layers.
pub fn draw_editor(app: &mut LightcraftApp, ui: &mut egui::Ui, draw: impl FnOnce(&mut LightcraftApp, &mut egui::Ui)) {
    let ctx = ui.ctx().clone();
    if !capturing(app, &ctx) {
        draw(app, ui);
        return;
    }
    let saved = ctx.input(Clone::clone);
    let mut raw = saved.raw.clone();
    raw.events.retain(|event| {
        !matches!(
            event,
            egui::Event::PointerMoved(_)
                | egui::Event::MouseMoved(_)
                | egui::Event::PointerButton { .. }
                | egui::Event::PointerGone
                | egui::Event::MouseWheel { .. }
                | egui::Event::Zoom(_)
                | egui::Event::Touch { .. }
        )
    });
    raw.screen_rect = Some(ctx.viewport_rect());
    let quiet = egui::InputState::default().begin_pass(raw, false, saved.pixels_per_point, egui::InputOptions::default());
    ctx.input_mut(|input| *input = quiet);
    draw(app, ui);
    ctx.input_mut(|input| *input = saved);
}

pub fn overlay(app: &mut LightcraftApp, ctx: &egui::Context) {
    // A filmstrip click can select a different photo after the frame's initial tick.
    // Do not draw old marks or deliver an old capture under that new identity.
    if super::identity(app) != app.assistant.photo {
        super::stop_voice(app);
        app.assistant.pending = None;
        app.assistant.photo_rect = None;
        app.assistant.capture = None;
        return;
    }
    if !visible(app) || super::blocked(app) {
        return;
    }
    island(app, ctx);
    let (Some(editor), Some(photo)) = (app.assistant.editor_rect, app.image_rect.filter(|r| r.is_positive())) else { return };
    if app.assistant.photo_rect != Some(photo) {
        app.assistant.pending = None;
        app.assistant.photo_rect = Some(photo);
        app.assistant.changed();
    }
    // Ink is drawn on the photo's own view: the panels around it stay the editor's.
    let canvas = app.canvas_rect.map_or(editor, |canvas| canvas.intersect(editor));
    if canvas.is_positive() {
        egui::Area::new(ink_layer().id).order(egui::Order::Foreground).fixed_pos(canvas.min).movable(false).constrain(false).show(ctx, |ui| {
            ui.set_min_size(canvas.size());
            let response = ui.allocate_rect(canvas, Sense::click_and_drag());
            register(ui, "assistant:overlay", canvas);
            app.assistant.overlay_id = Some(response.id);
            let hit = response.contains_pointer();
            response.on_hover_cursor(egui::CursorIcon::Crosshair);
            input(app, ctx, canvas, photo, hit);
            paint(app, &ui.painter().with_clip_rect(canvas), photo);
        });
    }
    super::vision::capture(app, ctx);
}

fn paint(app: &LightcraftApp, painter: &egui::Painter, photo: Rect) {
    if let Some(ink) = app.assistant.ink() {
        for mark in &ink.marks {
            mark.paint(painter, photo);
        }
    }
    if let Some(mark) = &app.assistant.pending {
        mark.paint(painter, photo);
    }
}

/// `hit`: the pointer is on the ink layer itself, not on the island or a popup above it.
fn input(app: &mut LightcraftApp, ctx: &egui::Context, editor: Rect, photo: Rect, hit: bool) {
    let pointer = ctx.input(|i| i.pointer.clone());
    let Some(pos) = pointer.interact_pos() else {
        app.assistant.pending = None;
        return;
    };
    let p = ink::normalized(editor.clamp(pos), photo);
    if pointer.primary_pressed() && hit {
        match app.assistant.tool {
            Tool::Pen | Tool::Highlighter => {
                let (color, width) =
                    if app.assistant.tool == Tool::Highlighter { ([242, 219, 71, 115], 12.0) } else { (app.assistant.color, app.assistant.width) };
                app.assistant.pending = Some(Mark::Stroke { points: vec![p], color, width: width / photo.width().max(1.0) });
            }
            Tool::Text => {
                let mark =
                    Mark::Text { position: p, text: app.assistant.note.clone(), color: app.assistant.color, size: 20.0 / photo.width().max(1.0) };
                let result = app.assistant.ink_mut().and_then(|i| i.append(vec![mark]));
                if let Err(e) = result {
                    app.assistant.error = Some(e);
                } else {
                    app.assistant.changed();
                }
            }
            Tool::Eraser => {}
        }
    }
    if app.assistant.tool == Tool::Eraser && pointer.primary_down() && hit && app.assistant.ink_mut().is_ok_and(|ink| ink.erase(pos, photo)) {
        app.assistant.changed();
    }
    if let Some(Mark::Stroke { points, .. }) = &mut app.assistant.pending
        && (pointer.primary_down() || pointer.primary_released())
        && points.len() < 20_000
        && points.last().is_none_or(|last| ink::screen(*last, photo).distance(pos) > 1.0)
    {
        points.push(p);
    }
    if pointer.primary_released() {
        if let Some(mark) = app.assistant.pending.take() {
            let result = app.assistant.ink_mut().and_then(|i| i.append(vec![mark]));
            if let Err(e) = result {
                app.assistant.error = Some(e);
            } else {
                app.assistant.changed();
            }
        }
    } else if !pointer.primary_down() {
        app.assistant.pending = None;
    }
}
