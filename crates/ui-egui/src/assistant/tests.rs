use super::*;
use crate::headless::Headless;
use crate::tests_fixtures::SyntheticLibrary;
use std::time::Duration;

fn demo() -> (SyntheticLibrary, Headless) {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let tag = format!("assistant-{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let fixture = SyntheticLibrary::with_photos(&tag, 2);
    let mut app = fixture.app();
    // These tests exercise UI input and command semantics, not GPU rendering/driver startup.
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    for _ in 0..8 {
        h.step();
    }
    (fixture, h)
}

fn request(h: &mut Headless, id: &str, p: Value) {
    h.app.run(id, p).unwrap();
    for _ in 0..4 {
        h.step();
    }
}

fn capture_example(h: &Headless, name: &str) {
    let Ok(directory) = std::env::var("LIGHTCRAFT_ASSISTANT_ARTIFACTS") else { return };
    let image = h.paint();
    let raster = lightcraft_raster::Rgba8::from_pixels(image.size[0], image.size[1], image.pixels.iter().map(|p| p.to_array())).unwrap();
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&raster), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(std::path::Path::new(&directory).join(name), png).unwrap();
}

#[test]
fn logo_opens_tools_and_collapse_keeps_ink_but_restores_editor() {
    let (_fixture, mut h) = demo();
    assert!(!h.app.assistant.expanded);
    let t = Duration::from_secs(20);
    let result = h.request("ui.clickWidget", json!({"id": "assistant:logo"}), t);
    assert_eq!(result["ok"], true, "{result}");
    assert!(visible(&h.app));
    request(&mut h, "assistant.tool", json!({"tool": "pen"}));
    let rect = h.app.image_rect.unwrap();
    let before = h.app.ui.zoom;
    let undo = h.app.session.undo.len();
    let result = h.request(
        "ui.drag",
        json!({"x": rect.center().x - 40.0, "y": rect.center().y,
        "toX": rect.center().x + 40.0, "toY": rect.center().y + 25.0, "steps": 8}),
        t,
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(h.app.assistant.ink().unwrap().marks.len(), 1);
    assert_eq!(h.app.session.undo.len(), undo, "drawing must not edit the photograph");
    assert_eq!(h.app.ui.zoom, before);
    capture_example(&h, "creative-tools.png");
    let result = h.request("ui.clickWidget", json!({"id": "assistant:logo"}), t);
    assert_eq!(result["ok"], true, "{result}");
    assert!(!h.app.assistant.expanded);
    assert_eq!(h.app.assistant.ink().unwrap().marks.len(), 1);
    assert!(!drawing(&h.app));
    assert!(h.app.assistant.voice.is_none());
    capture_example(&h, "regular-editor.png");
}

#[test]
fn annotations_belong_to_their_photo_and_history_is_independent() {
    let (_fixture, mut h) = demo();
    request(&mut h, "assistant.toggle", json!({}));
    let first = h.app.session.active().unwrap();
    let next = h.app.session.catalog.photos().find(|p| p.id != first).unwrap().id;
    let mark = Mark::Text { position: [0.5, 0.5], text: "Keep this sky".into(), color: [57, 123, 119, 255], size: 0.025 };
    h.app.assistant.ink_mut().unwrap().append(vec![mark]).unwrap();
    request(&mut h, "library.select", json!({"ids": [next.0]}));
    assert!(h.app.assistant.ink().is_none());
    request(&mut h, "library.select", json!({"ids": [first.0]}));
    assert_eq!(h.app.assistant.ink().unwrap().marks.len(), 1);
    request(&mut h, "assistant.clear", json!({}));
    assert!(h.app.assistant.ink().unwrap().marks.is_empty());
    request(&mut h, "assistant.undo", json!({}));
    assert_eq!(h.app.assistant.ink().unwrap().marks.len(), 1);
}

#[test]
fn model_changes_real_exposure_progressively_and_feedback_interrupts() {
    let (_fixture, mut h) = demo();
    request(&mut h, "assistant.toggle", json!({}));
    let photo = h.app.session.active().unwrap();
    let initial = h.app.session.develop_of(photo).unwrap().light.exposure;
    let undo = h.app.session.undo.len();
    let revision = h.app.assistant.revision;
    let now = h.view.ctx.input(|i| i.time);
    begin_edit(&mut h.app, "test".into(), revision, "light.exposure".into(), initial + 1.0, now).unwrap();
    for _ in 0..20 {
        h.step();
    }
    let midway = h.app.session.develop_of(photo).unwrap().light.exposure;
    assert!(midway > initial && midway < initial + 1.0, "{midway}");
    capture_example(&h, "visible-exposure-edit.png");
    interrupt(&mut h.app);
    for _ in 0..90 {
        h.step();
    }
    assert_eq!(h.app.session.develop_of(photo).unwrap().light.exposure, midway);
    assert_eq!(h.app.session.undo.len(), undo + 1, "one assistant action is one undo step");
    assert!(!h.app.assistant.editing());
    assert!(h.app.session.interaction.is_none());
}

#[test]
fn stale_model_actions_and_out_of_range_edits_are_rejected() {
    let (_fixture, mut h) = demo();
    request(&mut h, "assistant.toggle", json!({}));
    let revision = h.app.assistant.revision;
    assert!(begin_edit(&mut h.app, "bad".into(), revision, "light.exposure".into(), 99.0, 0.0).is_err());
    assert!(begin_edit(&mut h.app, "bad".into(), revision, "photo.delete".into(), 1.0, 0.0).is_err());
    h.app.assistant.changed();
    assert!(begin_edit(&mut h.app, "old".into(), revision, "light.exposure".into(), 1.0, 0.0).is_err());
    assert!(h.app.session.interaction.is_none());
}

#[test]
fn switching_photos_cancels_an_edit_before_it_can_touch_the_next_photo() {
    let (_fixture, mut h) = demo();
    request(&mut h, "assistant.toggle", json!({}));
    let first = h.app.session.active().unwrap();
    let next = h.app.session.catalog.photos().find(|p| p.id != first).unwrap().id;
    let next_before = h.app.session.develop_of(next).unwrap().light.exposure;
    let revision = h.app.assistant.revision;
    let now = h.view.ctx.input(|i| i.time);
    begin_edit(&mut h.app, "edit".into(), revision, "light.exposure".into(), 1.0, now).unwrap();
    for _ in 0..12 {
        h.step();
    }
    request(&mut h, "library.select", json!({"ids": [next.0]}));
    for _ in 0..90 {
        h.step();
    }
    assert_eq!(h.app.session.develop_of(next).unwrap().light.exposure, next_before);
    assert!(!h.app.assistant.editing());
}

#[test]
fn assistant_edit_never_auto_syncs_other_selected_photos() {
    for switch in [false, true] {
        let (_fixture, mut h) = demo();
        request(&mut h, "assistant.toggle", json!({}));
        let first = h.app.session.active().unwrap();
        let other = h.app.session.catalog.photos().find(|p| p.id != first).unwrap().id;
        request(&mut h, "library.select", json!({"ids": [first.0, other.0], "active": first.0}));
        h.app.session.auto_sync = true;
        let before = h.app.session.develop_of(other).unwrap().light.exposure;
        let revision = h.app.assistant.revision;
        let now = h.view.ctx.input(|i| i.time);
        begin_edit(&mut h.app, "edit".into(), revision, "light.exposure".into(), 1.0, now).unwrap();
        for _ in 0..15 {
            h.step();
        }
        if switch {
            request(&mut h, "library.select", json!({"ids": [other.0]}));
        }
        for _ in 0..100 {
            h.step();
        }
        assert_eq!(h.app.session.develop_of(other).unwrap().light.exposure, before, "Auto Sync leaked, switch={switch}");
        assert!(h.app.session.auto_sync, "restore the user's Auto Sync setting");
    }
}

fn mock_voice(h: &mut Headless) -> (SyncSender<VoiceEvent>, Receiver<VoiceInput>, Arc<AtomicBool>) {
    let (commands, input) = std::sync::mpsc::sync_channel(32);
    let (output, events) = std::sync::mpsc::sync_channel(32);
    let stop = Arc::new(AtomicBool::new(false));
    let connection = std::sync::Mutex::new(Some(VoiceConnection { commands, events, stop: stop.clone() }));
    h.app.services.start_voice = Some(Box::new(move |_| connection.lock().unwrap().take().ok_or("Already started".into())));
    (output, input, stop)
}

#[test]
fn logo_starts_voice_and_speech_and_text_stop_visible_edits() {
    let (_fixture, mut h) = demo();
    let (events, input, stop) = mock_voice(&mut h);
    request(&mut h, "assistant.toggle", json!({}));
    assert!(h.app.assistant.voice.is_some(), "opening the logo starts voice automatically");
    for speech in [false, true] {
        events
            .send(VoiceEvent::Edit { call_id: "edit".into(), revision: h.app.assistant.revision, control: "light.exposure".into(), value: 2.0 })
            .unwrap();
        for _ in 0..12 {
            h.step();
        }
        assert!(h.app.assistant.editing());
        let photo = h.app.session.active().unwrap();
        let midway = h.app.session.develop_of(photo).unwrap().light.exposure;
        if speech {
            events.send(VoiceEvent::Interrupt).unwrap();
            h.step();
        } else {
            request(&mut h, "assistant.send", json!({"text": "Stop, keep it darker"}));
            assert!(input.try_iter().any(|event| matches!(event, VoiceInput::Text(text) if text == "Stop, keep it darker")));
        }
        assert!(!h.app.assistant.editing());
        for _ in 0..90 {
            h.step();
        }
        assert_eq!(h.app.session.develop_of(photo).unwrap().light.exposure, midway);
    }
    request(&mut h, "assistant.toggle", json!({}));
    assert!(stop.load(Ordering::Acquire));
    assert!(h.app.assistant.voice.is_none());
}

#[test]
fn cancelled_voice_discards_tools_already_in_the_event_queue() {
    for cancellation_flag in [false, true] {
        let (_fixture, mut h) = demo();
        let (events, _input, stop) = mock_voice(&mut h);
        request(&mut h, "assistant.toggle", json!({}));
        if cancellation_flag {
            stop.store(true, Ordering::Release);
        } else {
            events.send(VoiceEvent::Error("Audio stopped".into())).unwrap();
        }
        events
            .send(VoiceEvent::Annotate {
                call_id: "late".into(),
                revision: h.app.assistant.revision,
                operations: json!([{"kind": "text", "position": [0.5, 0.5], "text": "Must not appear", "color": [57,123,119,255], "size": 20}]),
            })
            .unwrap();
        h.step();
        assert!(h.app.assistant.ink().is_none());
        assert!(h.app.assistant.voice.is_none());
    }
}

#[test]
fn replacing_library_does_not_commit_into_its_matching_photo_id() {
    let (_fixture, mut h) = demo();
    request(&mut h, "assistant.toggle", json!({}));
    h.app.session.auto_sync = true;
    let revision = h.app.assistant.revision;
    let now = h.view.ctx.input(|i| i.time);
    begin_edit(&mut h.app, "old".into(), revision, "light.exposure".into(), 1.0, now).unwrap();
    let other = SyntheticLibrary::new("assistant-new-library");
    h.app.session = other.app().session;
    h.app.session.begin_interaction("New library action").unwrap();
    interrupt(&mut h.app);
    assert!(h.app.session.interaction.is_some(), "old edit must not end the new library's interaction");
    assert!(!h.app.session.auto_sync);
}

#[test]
fn voice_receives_cropped_editor_images_and_discards_photo_switch_captures() {
    use base64::Engine as _;
    let (_fixture, mut h) = demo();
    h.app.services.png = Some(Box::new(|image| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(image), &lightcraft_codecs::EncodeMeta::default()).unwrap()
    }));
    let (_events, input, stop) = mock_voice(&mut h);
    request(&mut h, "assistant.toggle", json!({}));
    let mut captured = None;
    for _ in 0..65 {
        h.step();
        captured = input.try_iter().find_map(|event| match event {
            VoiceInput::Frame { data_url, revision, context } => Some((data_url, revision, context)),
            _ => None,
        });
        if captured.is_some() {
            break;
        }
    }
    let (data_url, revision, context) = captured.expect("voice must receive an actual editor image");
    assert!(data_url.len() < 4 * 1024 * 1024);
    assert_eq!(revision, h.app.assistant.revision);
    let png = base64::engine::general_purpose::STANDARD.decode(data_url.strip_prefix("data:image/png;base64,").unwrap()).unwrap();
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    let editor = h.app.assistant.editor_rect.unwrap();
    assert_eq!(width, editor.width().round() as u32);
    assert_eq!(height, editor.height().round() as u32);
    assert!(height < 900, "the assistant toolbar must be excluded");
    let context: Value = serde_json::from_str(&context).unwrap();
    assert_eq!(context["photo"], "a.png");
    assert!(context["controls"]["light.exposure"].is_number());
    if let Ok(directory) = std::env::var("LIGHTCRAFT_ASSISTANT_ARTIFACTS") {
        std::fs::write(std::path::Path::new(&directory).join("shared-editor-view.png"), png).unwrap();
    }
    h.app.assistant.captured_at = f64::NEG_INFINITY;
    h.step(); // an in-flight capture of the old photo
    input.try_iter().for_each(drop);
    let first = h.app.session.active().unwrap();
    let other = h.app.session.catalog.photos().find(|p| p.id != first).unwrap().id;
    request(&mut h, "library.select", json!({"ids": [other.0]}));
    assert!(stop.load(Ordering::Acquire));
    assert!(!input.try_iter().any(|event| matches!(event, VoiceInput::Frame { .. })));
}

/// One agent tool call, as the MCP server brackets it.
fn agent_call(h: &mut Headless, commands: &[(&str, Value)]) {
    h.app.run("agent.step.begin", json!({"tool": "set_develop"})).unwrap();
    for (id, p) in commands {
        h.app.run(id, p.clone()).unwrap();
    }
    h.app.run("agent.step.end", json!({})).unwrap();
}

#[test]
fn an_agent_step_is_replayed_on_its_sliders_without_editing_the_photo() {
    let (_fixture, mut h) = demo();
    h.app.ui.view = ViewMode::Detail;
    h.step();
    let id = h.app.session.active().unwrap();
    let exposure = |h: &Headless| controls::get(&h.app.session.develop_of(id).unwrap(), "light.exposure").unwrap();
    agent_call(
        &mut h,
        &[("develop.set", json!({"control": "light.exposure", "value": 0.8})), ("develop.set", json!({"control": "effects.dehaze", "value": 30.0}))],
    );
    let undo = h.app.session.undo.len();
    let revision = h.app.session.catalog.revision;

    let (mut travelled, mut waited, mut frames) = (false, false, 0);
    h.step();
    assert!(replay_running(&h.app), "the step starts a replay");
    while replay_running(&h.app) && frames < 2000 {
        if let Some(shown) = replay_value(&h.app, "light.exposure") {
            travelled |= shown > 0.05 && shown < 0.75;
            // the slider after it waits at its old value for its turn
            waited |= replay_value(&h.app, "effects.dehaze") == Some(0.0);
        }
        assert_eq!(exposure(&h), 0.8, "the photo keeps the value the agent set");
        h.step();
        frames += 1;
    }
    assert!(!replay_running(&h.app), "the replay ends by itself");
    assert!(travelled && waited, "the slider is drawn travelling, one slider at a time");
    assert_eq!(replay_value(&h.app, "light.exposure"), None);
    assert_eq!(h.app.session.undo.len(), undo, "a replay adds no undo step");
    assert_eq!(h.app.session.catalog.revision, revision, "a replay edits nothing");
    assert!(h.app.ui.section_open("effects"), "the section of the slider on show is opened");
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "agent:bar"), "the step bar stays hidden");

    // the person's own edit is not a step, and an edit during a replay ends it
    agent_call(&mut h, &[("develop.set", json!({"control": "light.contrast", "value": 20.0}))]);
    h.step();
    assert!(replay_running(&h.app));
    h.app.run("develop.set", json!({"control": "light.contrast", "value": 5.0})).unwrap();
    h.step();
    assert!(!replay_running(&h.app), "the values it was showing are stale");
}

#[test]
fn an_outside_voice_opens_the_workspace_and_reports_its_status() {
    let (_fixture, mut h) = demo();
    request(&mut h, "assistant.toggle", json!({"on": true}));
    assert!(h.app.assistant.expanded);
    request(&mut h, "assistant.toggle", json!({"on": true}));
    assert!(h.app.assistant.expanded, "asking for the state it has changes nothing");
    assert!(h.app.assistant.error.is_none(), "no voice transport in the app is not an error: {:?}", h.app.assistant.error);
    request(&mut h, "assistant.status", json!({"status": "Listening", "transcript": "warm it up a little"}));
    assert_eq!(h.app.assistant.status, "Listening");
    assert_eq!(h.app.assistant.transcript, "warm it up a little");
    request(&mut h, "assistant.toggle", json!({"on": false}));
    assert!(!h.app.assistant.expanded);
}
