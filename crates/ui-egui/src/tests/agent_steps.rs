//! Headless tests of the agent step bar and overlay: the step the photo's settings are at is named
//! in a bar over the loupe and drawn on the photo and the sliders, and Back / Advance move through
//! the steps without adding any.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
    r["result"].clone()
}

fn execute(h: &mut Headless, command: &str, params: Value) -> Value {
    ask(h, "engine.execute", json!({"command": command, "params": params}))
}

/// Run `commands` as one agent tool call.
fn agent_call(h: &mut Headless, tool: &str, commands: &[(&str, Value)]) {
    execute(h, "agent.step.begin", json!({"tool": tool, "note": "why"}));
    for (command, params) in commands {
        execute(h, command, params.clone());
    }
    execute(h, "agent.step.end", json!({}));
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn exposure(h: &Headless) -> f64 {
    let id = h.app.session.selection.active.unwrap();
    h.app.session.develop_of(id).unwrap().light.exposure
}

/// The demo library in Detail with the Edit panel's Light section open.
fn detail() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[0].0;
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}));
    ask(&mut h, "ui.set", json!({"view": "detail", "right": "edit"}));
    h.app.ui.open_sections = vec!["light".to_string()];
    h.settle(SETTLE);
    h
}

#[test]
fn the_bar_steps_back_and_forward_and_marks_what_each_step_changed() {
    let mut h = detail();
    assert!(!has(&h, "agent:bar"), "no agent steps, no bar");
    agent_call(&mut h, "set_develop", &[("develop.set", json!({"control": "light.exposure", "value": 0.7}))]);
    agent_call(&mut h, "crop", &[("crop.set", json!({"rect": [0.2, 0.1, 0.9, 0.9]}))]);
    agent_call(&mut h, "cmd_mask_add", &[("mask.add", json!({"kind": "linear", "start": [0.5, 0.1], "end": [0.5, 0.5]}))]);
    let history = |h: &Headless| h.app.session.catalog.photo(h.app.session.active().unwrap()).unwrap().history.len();
    let steps = history(&h);

    // at the latest step: its mask is drawn on the photo, inside the photo
    assert!(has(&h, "agent:bar"));
    let image = h.app.image_rect.expect("photo on screen");
    assert!(image.expand(1.0).contains_rect(rect(&h, "agent:mask:1")), "the mask's label is over the photo");
    assert!(!has(&h, "agent:crop") && !has(&h, "agentMark:light.exposure"));

    ask(&mut h, "ui.clickWidget", json!({"id": "agent:back"}));
    assert!(has(&h, "agent:crop"), "the crop step shows its diagram");
    assert!(!has(&h, "agent:mask:1"));

    ask(&mut h, "ui.clickWidget", json!({"id": "agent:back"}));
    assert_eq!(exposure(&h), 0.7);
    assert!(has(&h, "agentMark:light.exposure"), "the slider the step moved is marked");
    assert!(has(&h, "agent:changes") && !has(&h, "agent:crop"));

    ask(&mut h, "ui.clickWidget", json!({"id": "agent:back"}));
    assert_eq!(exposure(&h), 0.0, "before the agent's first step");
    assert!(has(&h, "agent:bar") && !has(&h, "agentMark:light.exposure") && !has(&h, "agent:changes"));

    ask(&mut h, "ui.clickWidget", json!({"id": "agent:next"}));
    assert_eq!(exposure(&h), 0.7);
    assert_eq!(history(&h), steps, "stepping adds no History step");
    assert_eq!(h.app.session.agent.steps().len(), 3, "stepping is not an agent step");

    // the marks follow the photo when the window changes size
    let before = rect(&h, "agent:changes");
    ask(&mut h, "ui.resize", json!({"width": 1000.0, "height": 700.0}));
    let image = h.app.image_rect.expect("photo on screen");
    assert!(image.expand(1.0).contains_rect(rect(&h, "agent:changes")));
    assert_ne!(rect(&h, "agent:changes"), before);

    ask(&mut h, "ui.clickWidget", json!({"id": "agent:close"}));
    assert!(!has(&h, "agent:bar"));
    assert_eq!(exposure(&h), 0.7, "hiding the steps keeps the edits");
}
