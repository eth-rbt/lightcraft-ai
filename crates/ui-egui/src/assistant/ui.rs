use egui::{Color32, Rect, RichText, Sense, Stroke, vec2};
use serde_json::json;

use super::{Mark, Tool, drawing, ink, visible};
use crate::{LightcraftApp, widgets::register};

const PAPER: Color32 = Color32::from_rgb(247, 242, 233);
const INK: Color32 = Color32::from_rgb(47, 47, 47);
const LINE: Color32 = Color32::from_rgb(217, 208, 193);
const SELECTED: Color32 = Color32::from_rgb(220, 210, 195);
const TEAL: Color32 = Color32::from_rgb(57, 123, 119);

fn button(ui: &mut egui::Ui, id: &str, text: &str, selected: bool, enabled: bool) -> bool {
    let r = ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).color(INK).size(13.0))
            .fill(if selected { SELECTED } else { Color32::TRANSPARENT })
            .min_size(vec2(28.0, 32.0)),
    );
    register(ui, id, r.rect);
    r.clicked()
}

/// The small workspace logo stays in the regular top bar when the extra toolbar is collapsed.
pub fn logo(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let expanded = visible(app);
    let tip = if expanded { "Close creative tools and stop voice" } else { "Open creative tools and voice" };
    let response = ui
        .add_enabled(
            app.session.active().is_some(),
            egui::Button::new(RichText::new("Lc").strong().size(16.0).color(if expanded { PAPER } else { TEAL }))
                .fill(if expanded { TEAL } else { Color32::TRANSPARENT })
                .min_size(vec2(34.0, 32.0)),
        )
        .on_hover_text(tip);
    register(ui, "assistant:logo", response.rect);
    crate::widgets::announce(&response, tip);
    if response.clicked() {
        let _ = app.run("assistant.toggle", json!({}));
    }
}

pub fn toolbar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if !visible(app) {
        app.assistant.editor_rect = None;
        return;
    }
    egui::Panel::top("creative-toolbar")
        .resizable(false)
        .frame(egui::Frame::NONE.fill(PAPER).stroke(Stroke::new(1.0, LINE)).inner_margin(egui::Margin::symmetric(12, 8)))
        .show(ui, |ui| {
            *ui.visuals_mut() = egui::Visuals::light();
            ui.visuals_mut().override_text_color = Some(INK);
            ui.spacing_mut().item_spacing = vec2(7.0, 6.0);
            ui.horizontal(|ui| {
                logo(app, ui);
                egui::ScrollArea::horizontal().id_salt("creative-tools-scroll").show(ui, |ui| {
                    ui.horizontal(|ui| tools(app, ui));
                });
            });
            ui.horizontal(|ui| {
                let r = crate::text_field::TextField::singleline("assistant:feedback", &mut app.assistant.text)
                    .label("Feedback for the assistant")
                    .hint("Speak naturally, or type feedback while the assistant edits…")
                    .width((ui.available_width() - 178.0).max(80.0))
                    .text_color(INK)
                    .show(ui);
                let enter = r.ending == Some(crate::text_field::Ending::Return);
                let send = button(ui, "assistant:send", "Send", false, app.assistant.voice.is_some() && !app.assistant.text.trim().is_empty());
                if (enter || send)
                    && let Err(error) = app.run("assistant.send", json!({}))
                {
                    app.assistant.error = Some(error);
                }
                let connected = app.assistant.voice.is_some();
                if button(ui, "assistant:voice", if connected { "Pause voice" } else { "Reconnect" }, connected, true) {
                    let _ = app.run("assistant.voice", json!({}));
                }
            });
            if app.assistant.tool == Tool::Text {
                ui.horizontal(|ui| {
                    ui.label("Label");
                    crate::text_field::TextField::singleline("assistant:label", &mut app.assistant.note)
                        .label("Annotation label")
                        .hint("Text to place on the photo")
                        .width(320.0)
                        .text_color(INK)
                        .show(ui);
                    ui.small("Click to place");
                });
            }
            let info = app.assistant.error.as_deref().unwrap_or_else(|| {
                if app.assistant.editing() {
                    &app.assistant.action
                } else if !app.assistant.transcript.is_empty() {
                    &app.assistant.transcript
                } else if !app.assistant.action.is_empty() {
                    &app.assistant.action
                } else if app.assistant.voice.is_some() {
                    "Microphone and this editor are shared · Speak or type to interrupt · Marks stay with this photo"
                } else {
                    "Marks stay with this photo · Drawing does not change the photo export"
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(&app.assistant.status).color(TEAL).size(11.0));
                ui.add(
                    egui::Label::new(
                        RichText::new(info).color(if app.assistant.error.is_some() { Color32::from_rgb(165, 66, 45) } else { INK }).size(11.0),
                    )
                    .truncate(),
                )
                .on_hover_text(info);
            });
        });
    app.assistant.editor_rect = Some(ui.available_rect_before_wrap());
}

fn tools(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    for (tool, id, label) in [
        (Tool::Interact, "interact", "Edit photo"),
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
        let (rect, response) = ui.allocate_exact_size(vec2(25.0, 28.0), Sense::click());
        let ink = Color32::from_rgb(color[0], color[1], color[2]);
        ui.painter().circle_filled(rect.center(), 8.0, ink);
        if app.assistant.color == color {
            ui.painter().circle_stroke(rect.center(), 11.0, Stroke::new(1.0, INK));
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
    for (id, label, enabled) in [("undo", "Undo ink", undo), ("redo", "Redo ink", redo), ("clear", "Clear ink", clear)] {
        if button(ui, &format!("assistant:{id}"), label, false, enabled) {
            let _ = app.run(&format!("assistant.{id}"), json!({}));
        }
    }
}

/// Suppress BOTH widget hit tests and raw pointer/wheel/key handlers in the normal editor while
/// drawing. The original InputState is restored for the overlay and subsequent modal layers.
pub fn draw_editor(app: &mut LightcraftApp, ui: &mut egui::Ui, draw: impl FnOnce(&mut LightcraftApp, &mut egui::Ui)) {
    if !drawing(app) {
        draw(app, ui);
        return;
    }
    let ctx = ui.ctx().clone();
    let saved = ctx.input(Clone::clone);
    let mut raw = saved.raw.clone();
    raw.events.clear();
    raw.focused = false;
    raw.screen_rect = Some(ctx.viewport_rect());
    let quiet = egui::InputState::default().begin_pass(raw, false, saved.pixels_per_point, egui::InputOptions::default());
    ctx.input_mut(|input| *input = quiet);
    ui.scope(|ui| {
        ui.visuals_mut().disabled_alpha = 1.0;
        ui.disable();
        draw(app, ui);
    });
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
    let (Some(editor), Some(photo)) = (app.assistant.editor_rect, app.image_rect.filter(|r| r.is_positive())) else { return };
    if app.assistant.photo_rect != Some(photo) {
        app.assistant.pending = None;
        app.assistant.photo_rect = Some(photo);
        app.assistant.changed();
    }
    if drawing(app) {
        egui::Area::new(egui::Id::unique("creative-annotation-input"))
            .order(egui::Order::Foreground)
            .fixed_pos(editor.min)
            .movable(false)
            .constrain(false)
            .show(ctx, |ui| {
                ui.set_min_size(editor.size());
                let response = ui.allocate_rect(editor, Sense::click_and_drag());
                register(ui, "assistant:overlay", editor);
                response.on_hover_cursor(egui::CursorIcon::Crosshair);
                input(app, ctx, editor, photo);
                paint(app, &ui.painter().with_clip_rect(editor), photo);
            });
    } else {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("creative-ink"))).with_clip_rect(editor);
        paint(app, &painter, photo);
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

fn input(app: &mut LightcraftApp, ctx: &egui::Context, editor: Rect, photo: Rect) {
    let pointer = ctx.input(|i| i.pointer.clone());
    let Some(pos) = pointer.interact_pos() else {
        app.assistant.pending = None;
        return;
    };
    let p = ink::normalized(editor.clamp(pos), photo);
    if pointer.primary_pressed() && editor.contains(pos) {
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
            _ => {}
        }
    }
    if app.assistant.tool == Tool::Eraser
        && pointer.primary_down()
        && editor.contains(pos)
        && app.assistant.ink_mut().is_ok_and(|ink| ink.erase(pos, photo))
    {
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
