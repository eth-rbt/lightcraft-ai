//! A spoken creative workspace above the editor. The host supplies audio/networking; this
//! module owns photo-bound ink and applies visible, interruptible edits through normal commands.

mod ink;
mod ui;
mod vision;

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, SyncSender},
};

use egui::Rect;
use lightcraft_develop::controls;
use serde_json::{Value, json};

use crate::{
    LightcraftApp,
    state::{RightPanel, ViewMode},
};
pub use ink::{Ink, Mark};
pub use ui::{draw_editor, logo, overlay, toolbar};

pub enum VoiceInput {
    Frame { data_url: String, revision: u64, context: String },
    Text(String),
    ToolResult { call_id: String, result: Value },
}

pub enum VoiceEvent {
    Status(String),
    Transcript(String),
    Interrupt,
    Annotate { call_id: String, revision: u64, operations: Value },
    Edit { call_id: String, revision: u64, control: String, value: f64 },
    Error(String),
}

pub struct VoiceConnection {
    pub commands: SyncSender<VoiceInput>,
    pub events: Receiver<VoiceEvent>,
    pub stop: Arc<AtomicBool>,
}

impl Drop for VoiceConnection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

pub type StartVoice = Box<dyn Fn(egui::Context) -> Result<VoiceConnection, String>>;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Interact,
    Pen,
    Highlighter,
    Eraser,
    Text,
}

pub const EDIT_CONTROLS: &[&str] = &[
    "light.exposure",
    "light.contrast",
    "light.highlights",
    "light.shadows",
    "light.whites",
    "light.blacks",
    "color.vibrance",
    "color.saturation",
    "effects.texture",
    "effects.clarity",
    "effects.dehaze",
    "wb.temp",
    "wb.tint",
];

struct Edit {
    auto_sync: bool,
    call_id: String,
    photo: (u64, u64),
    control: String,
    from: f64,
    target: f64,
    elapsed: f64,
    last: f64,
}

/// Session-local ink is keyed by both library generation and photo id. Nothing is written to
/// originals, exported photos, or the user's normal develop undo history by drawing.
pub struct Workspace {
    pub expanded: bool,
    pub tool: Tool,
    pub color: [u8; 4],
    pub width: f32,
    pub note: String,
    pub text: String,
    pub status: String,
    pub error: Option<String>,
    pub transcript: String,
    pub action: String,
    pub revision: u64,
    pub photo: Option<(u64, u64)>,
    pub documents: BTreeMap<(u64, u64), Ink>,
    pub pending: Option<Mark>,
    pub editor_rect: Option<Rect>,
    pub photo_rect: Option<Rect>,
    pub voice: Option<VoiceConnection>,
    voice_wanted: bool,
    attempted: bool,
    edit: Option<Edit>,
    guard: Option<lightcraft_engine::activity::TaskGuard>,
    capture: Option<vision::Capture>,
    captured_at: f64,
    catalog_revision: u64,
    barrier: bool,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            expanded: false,
            tool: Tool::Interact,
            color: [57, 123, 119, 255],
            width: 4.0,
            note: "Note".into(),
            text: String::new(),
            status: "Voice off".into(),
            error: None,
            transcript: String::new(),
            action: String::new(),
            revision: 0,
            photo: None,
            documents: BTreeMap::new(),
            pending: None,
            editor_rect: None,
            photo_rect: None,
            voice: None,
            voice_wanted: false,
            attempted: false,
            edit: None,
            guard: None,
            capture: None,
            captured_at: f64::NEG_INFINITY,
            catalog_revision: 0,
            barrier: false,
        }
    }
}

impl Workspace {
    pub fn ink(&self) -> Option<&Ink> {
        self.photo.and_then(|key| self.documents.get(&key))
    }
    pub fn ink_mut(&mut self) -> Result<&mut Ink, String> {
        let key = self.photo.ok_or("Open a photo to annotate it")?;
        if !self.documents.contains_key(&key) && self.documents.len() >= 128 {
            return Err("The annotation workspace is full for this session".into());
        }
        Ok(self.documents.entry(key).or_default())
    }
    pub fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }
}

fn identity(app: &LightcraftApp) -> Option<(u64, u64)> {
    app.session.active().map(|id| (app.session.library_generation(), id.0))
}

pub fn visible(app: &LightcraftApp) -> bool {
    app.assistant.expanded && app.ui.view == ViewMode::Detail && !app.ui.fullscreen && app.session.active().is_some()
}

fn blocked(app: &LightcraftApp) -> bool {
    app.ui.dialog.is_some()
        || crate::panels::notices::blocks_input(app)
        || crate::panels::library_problem::blocks_dialog_input(app)
        || crate::dialogs::updates_ui::blocks_input(app)
}

pub fn drawing(app: &LightcraftApp) -> bool {
    visible(app) && !blocked(app) && app.assistant.tool != Tool::Interact
}

fn reply(app: &mut LightcraftApp, call_id: String, result: Value) {
    if let Some(voice) = &app.assistant.voice
        && voice.commands.try_send(VoiceInput::ToolResult { call_id, result }).is_err()
    {
        app.assistant.error = Some("Voice connection is busy. Reconnect before requesting another edit.".into());
        stop_voice(app);
    }
}

fn finish_edit(app: &mut LightcraftApp, stopped: bool) {
    let Some(edit) = app.assistant.edit.take() else { return };
    // A replaced library owns a different interaction and Auto Sync preference, even when
    // its photo IDs happen to match. Never commit or restore state into that new library.
    if app.session.library_generation() != edit.photo.0 {
        app.ui.dragging_control = None;
        return;
    }
    let result = app.run("develop.endInteraction", json!({}));
    app.session.auto_sync = edit.auto_sync;
    app.ui.dragging_control = None;
    let current = app.session.develop_of(lightcraft_catalog::PhotoId(edit.photo.1)).and_then(|d| controls::get(&d, &edit.control));
    app.assistant.action = if stopped {
        format!("Stopped · {} at {}", edit.control, current.unwrap_or(edit.from))
    } else {
        format!("Applied · {} = {}", edit.control, edit.target)
    };
    reply(
        app,
        edit.call_id,
        json!({"ok": !stopped && result.is_ok(), "interrupted": stopped,
        "control": edit.control, "value": current, "revision": app.assistant.revision}),
    );
}

pub fn interrupt(app: &mut LightcraftApp) {
    finish_edit(app, true);
    app.assistant.pending = None;
    app.assistant.changed();
}

fn stop_voice(app: &mut LightcraftApp) {
    finish_edit(app, true);
    app.assistant.voice = None;
    app.assistant.capture = None;
    app.assistant.guard = None;
    app.assistant.status = "Voice off".into();
}

fn start_voice(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.assistant.voice.is_some() || app.assistant.attempted || !visible(app) || blocked(app) {
        return;
    }
    app.assistant.attempted = true;
    let Some(start) = &app.services.start_voice else {
        app.assistant.error = Some("Voice is available in the Windows desktop app. Drawing still works here.".into());
        return;
    };
    match start(ctx.clone()) {
        Ok(connection) => {
            app.assistant.guard = Some(app.session.activity.start(
                "voice",
                "Creative voice assistant",
                lightcraft_engine::activity::Cancel::Flag(connection.stop.clone()),
            ));
            app.assistant.voice = Some(connection);
            app.assistant.status = "Connecting".into();
            app.assistant.error = None;
            app.assistant.captured_at = f64::NEG_INFINITY;
        }
        Err(error) => app.assistant.error = Some(error),
    }
}

/// The public UI/control entry point. The logo is the workspace switch, not a microphone switch.
pub fn command(app: &mut LightcraftApp, id: &str, p: &Value) -> Result<Value, String> {
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    match id {
        "assistant.toggle" => {
            if app.assistant.expanded {
                stop_voice(app);
                app.assistant.expanded = false;
                app.assistant.voice_wanted = false;
                app.assistant.tool = Tool::Interact;
                app.assistant.pending = None;
            } else {
                if app.session.active().is_none() {
                    return Err("Open a photo first".into());
                }
                app.ui.view = ViewMode::Detail;
                app.assistant.expanded = true;
                app.assistant.voice_wanted = true;
                app.assistant.attempted = false;
                app.assistant.photo = identity(app);
                start_voice(app, &ctx);
            }
            app.assistant.changed();
            ctx.request_discard("creative workspace changed its layout");
        }
        "assistant.voice" => {
            if app.assistant.voice.is_some() {
                stop_voice(app);
                app.assistant.voice_wanted = false;
            } else {
                app.assistant.voice_wanted = true;
                app.assistant.attempted = false;
                start_voice(app, &ctx);
            }
        }
        "assistant.tool" => {
            let tool = match p.get("tool").and_then(Value::as_str) {
                Some("interact") => Tool::Interact,
                Some("pen") => Tool::Pen,
                Some("highlighter") => Tool::Highlighter,
                Some("eraser") => Tool::Eraser,
                Some("text") => Tool::Text,
                _ => return Err("Unknown annotation tool".into()),
            };
            interrupt(app);
            let _ = app.session.end_interaction();
            app.gesture = None;
            app.space_hand = None;
            app.ui.dragging_control = None;
            app.assistant.tool = tool;
            ctx.stop_dragging();
            ctx.request_discard("annotation input ownership changed");
        }
        "assistant.undo" | "assistant.redo" | "assistant.clear" => {
            app.assistant.pending = None;
            let ink = app.assistant.ink_mut()?;
            match id {
                "assistant.undo" => ink.undo(),
                "assistant.redo" => ink.redo(),
                _ => ink.clear(),
            }
            app.assistant.changed();
        }
        "assistant.stop" => {
            // Closing the transport cancels any server response as well as the local animation.
            stop_voice(app);
            app.assistant.voice_wanted = false;
        }
        "assistant.send" => {
            let text = p.get("text").and_then(Value::as_str).unwrap_or(&app.assistant.text).trim().to_string();
            if text.is_empty() || text.len() > 4000 {
                return Err("Enter a message of up to 4,000 characters".into());
            }
            interrupt(app);
            let voice = app.assistant.voice.as_ref().ok_or("Voice is not connected. Reconnect to send feedback.")?;
            voice.commands.try_send(VoiceInput::Text(text)).map_err(|_| "Voice is busy. Try sending again.")?;
            app.assistant.text.clear();
        }
        _ => return Err("Unknown creative workspace command".into()),
    }
    ctx.request_repaint();
    Ok(json!({"expanded": app.assistant.expanded, "revision": app.assistant.revision}))
}

fn valid_request(app: &LightcraftApp, revision: u64) -> Result<(), String> {
    if !visible(app) || blocked(app) || identity(app) != app.assistant.photo {
        return Err("The photo or workspace changed. The action was cancelled.".into());
    }
    if revision != app.assistant.revision {
        return Err("The picture changed. Look at the latest image before editing.".into());
    }
    if app.assistant.pending.is_some() {
        return Err("The user is drawing. Wait for the stroke to finish.".into());
    }
    Ok(())
}

fn annotate(app: &mut LightcraftApp, revision: u64, operations: Value) -> Result<Value, String> {
    valid_request(app, revision)?;
    if operations.as_array().is_none_or(|ops| ops.len() > 32) {
        return Err("Too many annotation operations".into());
    }
    let mut marks: Vec<Mark> = serde_json::from_value(operations).map_err(|_| "Invalid annotation operations")?;
    let editor = app.assistant.editor_rect.ok_or("Editor is not visible")?;
    let photo = app.assistant.photo_rect.ok_or("Photo is not visible")?;
    let convert = |p: &mut [f32; 2]| -> Result<(), String> {
        if p.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
            return Err("Annotation coordinates are outside the shared image".into());
        }
        *p = ink::normalized(ink::screen(*p, editor), photo);
        Ok(())
    };
    for mark in &mut marks {
        match mark {
            Mark::Stroke { points, width, .. } => {
                if !width.is_finite() || !(0.5..=40.0).contains(width) {
                    return Err("Invalid stroke width".into());
                }
                for p in points {
                    convert(p)?;
                }
                *width /= photo.width().max(1.0);
            }
            Mark::Text { position, size, .. } => {
                if !size.is_finite() || !(8.0..=96.0).contains(size) {
                    return Err("Invalid annotation size".into());
                }
                convert(position)?;
                *size /= photo.width().max(1.0);
            }
        }
    }
    let count = marks.len();
    app.assistant.ink_mut()?.append(marks)?;
    app.assistant.changed();
    Ok(json!({"ok": true, "added": count, "revision": app.assistant.revision}))
}

fn begin_edit(app: &mut LightcraftApp, call_id: String, revision: u64, control: String, target: f64, now: f64) -> Result<(), String> {
    valid_request(app, revision)?;
    if app.assistant.edit.is_some() || app.session.interaction.is_some() {
        return Err("An adjustment is already in progress".into());
    }
    if !EDIT_CONTROLS.contains(&control.as_str()) {
        return Err("This adjustment is not supported by the assistant".into());
    }
    let spec = controls::find(&control).ok_or("Unknown adjustment")?;
    if !target.is_finite() || !(spec.min..=spec.max).contains(&target) {
        return Err("Adjustment value is out of range".into());
    }
    let photo = identity(app).ok_or("Open a photo first")?;
    let from =
        app.session.develop_of(lightcraft_catalog::PhotoId(photo.1)).and_then(|d| controls::get(&d, &control)).ok_or("Cannot read adjustment")?;
    let section = match spec.section {
        lightcraft_develop::Section::Light => "light",
        lightcraft_develop::Section::Color => "color",
        lightcraft_develop::Section::Effects => "effects",
        _ => return Err("Unsupported adjustment section".into()),
    };
    if app.ui.right != RightPanel::Edit || !crate::docking::exposed(app, crate::docking::Panel::Tools) {
        app.run("panel.edit", json!({}))?;
    }
    app.ui.right = RightPanel::Edit;
    if !app.ui.open_sections.iter().any(|s| s == section) {
        app.ui.open_sections.push(section.into());
    }
    app.run("develop.beginInteraction", json!({"label": format!("Assistant · {}", spec.label)}))?;
    let auto_sync = app.session.auto_sync;
    app.session.auto_sync = false;
    app.ui.dragging_control = Some(control.clone());
    app.assistant.tool = Tool::Interact;
    app.assistant.action = format!("{}  {} → {}", spec.label, spec.format(from), spec.format(target));
    app.assistant.edit = Some(Edit { auto_sync, call_id, photo, control, from, target, elapsed: 0.0, last: now });
    Ok(())
}

fn advance_edit(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(mut edit) = app.assistant.edit.take() else { return };
    if identity(app) != Some(edit.photo) || blocked(app) || !visible(app) {
        app.assistant.edit = Some(edit);
        finish_edit(app, true);
        return;
    }
    let now = ctx.input(|i| i.time);
    edit.elapsed += (now - edit.last).clamp(0.0, 0.08);
    edit.last = now;
    let fraction = (edit.elapsed / 1.2).clamp(0.0, 1.0);
    let value = edit.from + (edit.target - edit.from) * fraction;
    // Voice actions target this photo even if the user has enabled multi-photo Auto Sync.
    app.session.auto_sync = false;
    let result = app.run("develop.set", json!({"control": edit.control, "value": value}));
    app.assistant.edit = Some(edit);
    if result.is_err() || fraction >= 1.0 {
        finish_edit(app, result.is_err());
    }
    ctx.request_repaint();
}

/// Run once per visible frame before keyboard processing. No microphone starts on app launch.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    let photo = identity(app);
    if photo != app.assistant.photo {
        stop_voice(app);
        app.assistant.photo = photo;
        app.assistant.pending = None;
        app.assistant.photo_rect = None;
        app.assistant.attempted = false;
        app.assistant.transcript.clear();
        app.assistant.action.clear();
        app.assistant.changed();
    }
    let barrier = blocked(app) || !visible(app);
    if barrier {
        if app.assistant.voice.is_some() {
            stop_voice(app);
        }
        app.assistant.pending = None;
    } else if app.assistant.barrier {
        app.assistant.attempted = false;
    }
    app.assistant.barrier = barrier;
    if app.assistant.voice_wanted && !barrier {
        start_voice(app, ctx);
    }
    if app.assistant.catalog_revision != app.session.catalog.revision {
        app.assistant.catalog_revision = app.session.catalog.revision;
        app.assistant.changed();
    }
    let mut events: Vec<_> = app.assistant.voice.as_ref().map(|v| v.events.try_iter().take(32).collect()).unwrap_or_default();
    if app.assistant.voice.as_ref().is_some_and(|v| v.stop.load(Ordering::Acquire)) {
        // Preserve the worker's actionable error while discarding every queued tool.
        if let Some(error) = events.iter().find_map(|event| match event {
            VoiceEvent::Error(error) => Some(error.clone()),
            _ => None,
        }) {
            app.assistant.error = Some(error);
        }
        stop_voice(app);
        events.clear();
    }
    for event in events {
        // Audio errors and cancellation can race with queued tool calls. Cancellation wins.
        if app.assistant.voice.as_ref().is_none_or(|v| v.stop.load(Ordering::Acquire)) {
            stop_voice(app);
            break;
        }
        match event {
            VoiceEvent::Status(status) => app.assistant.status = status,
            VoiceEvent::Transcript(text) => app.assistant.transcript = text.chars().take(600).collect(),
            VoiceEvent::Interrupt => interrupt(app),
            VoiceEvent::Error(error) => {
                stop_voice(app);
                app.assistant.error = Some(error);
                break;
            }
            VoiceEvent::Annotate { call_id, revision, operations } => {
                let result = annotate(app, revision, operations).unwrap_or_else(|e| json!({"ok": false, "error": e}));
                reply(app, call_id, result);
            }
            VoiceEvent::Edit { call_id, revision, control, value } => {
                if let Err(error) = begin_edit(app, call_id.clone(), revision, control, value, ctx.input(|i| i.time)) {
                    reply(app, call_id, json!({"ok": false, "error": error}));
                }
            }
        }
    }
    if app.assistant.voice.as_ref().is_some_and(|v| v.stop.load(Ordering::Acquire)) {
        stop_voice(app);
    }
    // Physical interaction wins over an assistant adjustment immediately.
    if app.assistant.edit.is_some()
        && ctx.input(|i| {
            i.pointer.any_pressed()
                || i.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Key { pressed: true, .. } | egui::Event::MouseWheel { .. }
                    )
                })
        })
    {
        interrupt(app);
    }
    advance_edit(app, ctx);
    if app.assistant.voice.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(40));
    }
}

pub fn shortcuts(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    if !drawing(app) || ctx.egui_wants_keyboard_input() {
        return false;
    }
    let command = ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) {
            Some(("assistant.tool", json!({"tool": "interact"})))
        } else if i.modifiers.command && i.key_pressed(egui::Key::Z) {
            Some((if i.modifiers.shift { "assistant.redo" } else { "assistant.undo" }, json!({})))
        } else {
            None
        }
    });
    if let Some((id, p)) = command {
        let _ = app.run(id, p);
    }
    true
}

/// Called where the actual slider is laid out, so an assistant adjustment is visible even if
/// its section had been scrolled offscreen. The label and thumb are highlighted together.
pub fn show_control(app: &LightcraftApp, ui: &mut egui::Ui, control: &str, row: Rect) {
    let shown = match control {
        "wb.tempRel" => "wb.temp",
        "wb.tintRel" => "wb.tint",
        other => other,
    };
    if app.assistant.edit.as_ref().is_some_and(|edit| edit.control == shown) {
        ui.scroll_to_rect(row, Some(egui::Align::Center));
        ui.painter().rect_stroke(row.shrink(3.0), 4.0, egui::Stroke::new(1.5, egui::Color32::from_rgb(57, 123, 119)), egui::StrokeKind::Inside);
    }
}

#[cfg(test)]
mod tests;
