//! Bounded, in-memory capture of this window's editor only, with the island floating over it.
use base64::Engine as _;
use egui::{ColorImage, Rect};
use lightcraft_develop::controls;
use serde_json::json;

use super::{EDIT_CONTROLS, VoiceInput};
use crate::LightcraftApp;

pub(super) struct Capture {
    slot: craft_ui::capture::CaptureSlot,
    revision: u64,
    rect: Rect,
    viewport: Rect,
    photo: Rect,
    identity: Option<(u64, u64)>,
    catalog_revision: u64,
    at: f64,
}

pub(super) fn capture(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.assistant.voice.is_none() || ctx.current_pass_index() != 0 || ctx.will_discard() {
        return;
    }
    let now = ctx.input(|i| i.time);
    if let Some(capture) = app.assistant.capture.take() {
        if capture.revision != app.assistant.revision
            || now - capture.at > 3.0
            || capture.identity != super::identity(app)
            || capture.catalog_revision != app.session.catalog.revision
        {
            // Never send the previous photo or stale coordinates after a switch/resize/edit.
        } else if let Some(result) = capture.slot.take() {
            match result {
                Ok(image) => send(app, &image, &capture),
                Err(_) => app.assistant.error = Some("Editor sharing failed. Reconnect voice to try again.".into()),
            }
        } else {
            app.assistant.capture = Some(capture);
        }
    }
    if app.assistant.capture.is_some() || now - app.assistant.captured_at < 0.8 {
        return;
    }
    let (Some(rect), Some(photo)) = (app.assistant.editor_rect, app.assistant.photo_rect) else { return };
    let (slot, callback) = craft_ui::capture::CaptureSlot::new(ctx, egui::ViewportId::ROOT, 64 * 1024 * 1024);
    app.assistant.capture = Some(Capture {
        slot,
        revision: app.assistant.revision,
        rect,
        viewport: ctx.viewport_rect(),
        photo,
        at: now,
        identity: super::identity(app),
        catalog_revision: app.session.catalog.revision,
    });
    app.assistant.captured_at = now;
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(callback));
}

fn send(app: &mut LightcraftApp, image: &ColorImage, capture: &Capture) {
    if capture.identity != super::identity(app) || capture.identity != app.assistant.photo || capture.catalog_revision != app.session.catalog.revision
    {
        return;
    }
    let Some(png) = &app.services.png else { return };
    let [width, height] = image.size;
    if width == 0 || height == 0 || !capture.viewport.is_positive() {
        return;
    }
    let sx = width as f32 / capture.viewport.width();
    let sy = height as f32 / capture.viewport.height();
    let bounds = capture.rect.intersect(capture.viewport);
    let x = ((bounds.left() - capture.viewport.left()) * sx).round().max(0.0) as usize;
    let y = ((bounds.top() - capture.viewport.top()) * sy).round().max(0.0) as usize;
    let w = (bounds.width() * sx).round().max(1.0) as usize;
    let h = (bounds.height() * sy).round().max(1.0) as usize;
    // Base64 expands the payload by 4/3; keep the entire data URL below the transport's 4 MiB cap.
    // Detailed/noisy photos are downscaled again rather than silently losing visual context.
    let mut bytes = Vec::new();
    for edge in [1600.0, 1200.0, 900.0, 640.0] {
        let scale = (edge / w.max(h) as f32).min(1.0);
        let ow = ((w as f32 * scale).round() as usize).max(1);
        let oh = ((h as f32 * scale).round() as usize).max(1);
        let pixels = (0..oh).flat_map(|row| {
            (0..ow).map(move |column| {
                let py = (y + row * h / oh).min(height - 1);
                let px = (x + column * w / ow).min(width - 1);
                image.pixels.get(py * width + px).map(|p| p.to_array()).unwrap_or([0, 0, 0, 255])
            })
        });
        let Some(raster) = lightcraft_raster::Rgba8::from_pixels(ow, oh, pixels) else { return };
        bytes = png(&raster);
        if !bytes.is_empty() && bytes.len() <= 2_900_000 {
            break;
        }
    }
    if bytes.is_empty() || bytes.len() > 2_900_000 {
        app.assistant.error = Some("Could not share the current editor image. Reconnect voice to try again.".into());
        return;
    }
    let photo = app.session.active().and_then(|id| app.session.catalog.photo(id));
    let develop = app.session.active().and_then(|id| app.session.develop_of(id));
    let values: serde_json::Map<String, serde_json::Value> =
        EDIT_CONTROLS.iter().filter_map(|id| develop.as_ref().and_then(|d| controls::get(d, id)).map(|v| ((*id).into(), json!(v)))).collect();
    let context = json!({
        "photo": photo.map(|p| &p.file_name), "controls": values,
        "photoBounds": [super::ink::normalized(capture.photo.min, capture.rect), super::ink::normalized(capture.photo.max, capture.rect)],
        "annotations": app.assistant.ink().map(|ink| ink.marks.len()).unwrap_or(0),
        "instruction": "This image is the current editor. Annotation coordinates are relative to the whole shared image, from 0 to 1. The listed controls are the current values. Only change the current photo when asked."
    }).to_string();
    let data_url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
    if let Some(voice) = &app.assistant.voice {
        let _ = voice.commands.try_send(VoiceInput::Frame { data_url, revision: capture.revision, context });
    }
}
