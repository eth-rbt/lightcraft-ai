//! Feature: the agent step log. A bracketed tool call that edits a photo becomes one step with
//! both sides of the edit; going to a step moves through History without adding steps.

use serde_json::{Value, json};

use crate::Session;

fn exposure(s: &Session) -> f64 {
    s.develop_of(s.active().unwrap()).unwrap().light.exposure
}

fn history_len(s: &Session) -> usize {
    s.catalog.photo(s.active().unwrap()).unwrap().history.len()
}

/// Run `commands` as one agent tool call.
fn call(s: &mut Session, tool: &str, note: Option<&str>, commands: &[(&str, Value)]) -> Vec<u64> {
    s.execute("agent.step.begin", &json!({"tool": tool, "note": note})).unwrap();
    for (id, params) in commands {
        s.execute(id, params).unwrap();
    }
    let ended = s.execute("agent.step.end", &json!({})).unwrap();
    ended["steps"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect()
}

fn list(s: &mut Session) -> Value {
    s.execute("agent.steps.list", &json!({})).unwrap()
}

#[test]
fn an_editing_call_becomes_a_step_with_both_values() {
    let mut s = Session::with_demo();
    let made = call(
        &mut s,
        "set_develop",
        Some("  The subject is a little dark.  "),
        &[("develop.set", json!({"control": "light.exposure", "value": 0.7}))],
    );
    assert_eq!(made.len(), 1);
    let l = list(&mut s);
    let step = &l["steps"][0];
    assert_eq!(step["id"], made[0]);
    assert_eq!(step["tool"], "set_develop");
    assert_eq!(step["note"], "The subject is a little dark.");
    assert_eq!(step["photo"], s.active().unwrap().0);
    let changes = step["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0]["id"], "light.exposure");
    assert_eq!(changes[0]["from"], 0.0);
    assert_eq!(changes[0]["to"], 0.7);
    assert_eq!(changes[0]["toText"], "+0.70");
    assert_eq!(step["other"], false);
    assert!(step.get("crop").is_none() && step.get("masks").is_none());
    assert_eq!(l["current"], made[0]);
    assert_eq!(l["open"], false);
}

#[test]
fn calls_that_edit_nothing_and_edits_outside_a_call_leave_no_step() {
    let mut s = Session::with_demo();
    assert!(call(&mut s, "get_develop", None, &[("develop.get", json!({}))]).is_empty());
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    // an edit that is undone within the call changed nothing either
    let undone = call(&mut s, "set_develop", None, &[("develop.set", json!({"control": "light.contrast", "value": 20})), ("edit.undo", json!({}))]);
    assert!(undone.is_empty());
    assert_eq!(list(&mut s)["steps"].as_array().unwrap().len(), 0);
}

#[test]
fn several_edits_in_one_call_are_one_step() {
    let mut s = Session::with_demo();
    let made = call(
        &mut s,
        "command_batch",
        None,
        &[
            ("develop.set", json!({"control": "light.exposure", "value": 0.5})),
            ("develop.set", json!({"control": "light.contrast", "value": 15})),
            ("develop.set", json!({"control": "light.exposure", "value": 0.9})),
        ],
    );
    assert_eq!(made.len(), 1);
    let l = list(&mut s);
    let ids: Vec<&str> = l["steps"][0]["changes"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["light.exposure", "light.contrast"]);
    assert_eq!(l["steps"][0]["changes"][0]["to"], 0.9, "the value the call ended on");
}

#[test]
fn crop_and_mask_edits_are_reported_as_geometry() {
    let mut s = Session::with_demo();
    call(&mut s, "crop", None, &[("crop.set", json!({"rect": [0.1, 0.1, 0.8, 0.9]}))]);
    call(&mut s, "cmd_mask_add", None, &[("mask.add", json!({"kind": "linear", "start": [0.5, 0.1], "end": [0.5, 0.5]}))]);
    let l = list(&mut s);
    let crop = &l["steps"][0]["crop"];
    assert_eq!(crop["from"], json!([0.0, 0.0, 1.0, 1.0]));
    assert_eq!(crop["to"], json!([0.1, 0.1, 0.8, 0.9]));
    assert!(l["steps"][0].get("masks").is_none());
    let masks = &l["steps"][1]["masks"];
    assert_eq!(masks["added"].as_array().unwrap().len(), 1, "{masks}");
    assert!(l["steps"][1].get("crop").is_none());
}

#[test]
fn going_to_a_step_moves_through_history_without_adding_steps() {
    let mut s = Session::with_demo();
    let a = call(&mut s, "set_develop", None, &[("develop.set", json!({"control": "light.exposure", "value": 1.0}))])[0];
    let b = call(&mut s, "set_develop", None, &[("develop.set", json!({"control": "light.exposure", "value": 2.0}))])[0];
    let n = history_len(&s);

    s.execute("agent.steps.goto", &json!({"step": a})).unwrap();
    assert_eq!(exposure(&s), 1.0);
    assert_eq!(list(&mut s)["current"], a);
    s.execute("agent.steps.goto", &json!({"step": b})).unwrap();
    assert_eq!(exposure(&s), 2.0);
    s.execute("agent.steps.goto", &json!({"step": b, "side": "before"})).unwrap();
    assert_eq!(exposure(&s), 1.0, "the second step started from the first one's settings");
    assert_eq!(history_len(&s), n, "stepping adds no History step");
    assert_eq!(list(&mut s)["steps"].as_array().unwrap().len(), 2, "stepping is not an agent step");

    // an edit by the person since: the photo is at none of the steps
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 3.0})).unwrap();
    let l = list(&mut s);
    assert!(l["current"].is_null());
    assert_eq!(l["atStart"], false);
}

#[test]
fn settings_history_does_not_list_are_loaded_without_changing_history() {
    let mut s = Session::with_demo();
    let a = call(&mut s, "set_develop", None, &[("develop.set", json!({"control": "light.exposure", "value": 1.0}))])[0];
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 2.0})).unwrap();
    s.execute("history.clear", &json!({})).unwrap();
    s.execute("agent.steps.goto", &json!({"step": a})).unwrap();
    assert_eq!(exposure(&s), 1.0);
    assert_eq!(history_len(&s), 1, "the cleared History is left as it was");

    // the settings before the agent's first step were never a History step: going there from the
    // middle of History must not drop the steps after the cursor
    let mut s = Session::with_demo();
    let a = call(&mut s, "set_develop", None, &[("develop.set", json!({"control": "light.exposure", "value": 1.0}))])[0];
    let b = call(&mut s, "set_develop", None, &[("develop.set", json!({"control": "light.exposure", "value": 2.0}))])[0];
    let n = history_len(&s);
    s.execute("agent.steps.goto", &json!({"step": a})).unwrap();
    s.execute("agent.steps.goto", &json!({"step": a, "side": "before"})).unwrap();
    assert_eq!(exposure(&s), 0.0);
    assert_eq!(list(&mut s)["atStart"], true);
    assert_eq!(history_len(&s), n);
    s.execute("agent.steps.goto", &json!({"step": b})).unwrap();
    assert_eq!((exposure(&s), history_len(&s)), (2.0, n));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(exposure(&s), 0.0, "going to a step is one undo step");
}

#[test]
fn bad_arguments_are_errors() {
    let mut s = Session::with_demo();
    assert!(s.execute("agent.step.begin", &json!({})).is_err(), "a step names its tool");
    assert!(s.execute("agent.steps.goto", &json!({"step": 99})).is_err());
    assert!(s.execute("agent.steps.goto", &json!({})).is_err());
    assert!(s.execute("agent.steps.list", &json!({"photo": "1"})).is_err());
    let a = call(&mut s, "set_develop", None, &[("develop.set", json!({"control": "light.exposure", "value": 1.0}))])[0];
    assert!(s.execute("agent.steps.goto", &json!({"step": a, "side": "sideways"})).is_err());
}

#[test]
fn a_call_left_open_ends_when_the_next_begins_and_the_log_is_bounded() {
    let mut s = Session::with_demo();
    s.execute("agent.step.begin", &json!({"tool": "first"})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    assert_eq!(list(&mut s)["open"], true);
    call(&mut s, "second", None, &[("develop.set", json!({"control": "light.exposure", "value": 2.0}))]);
    let l = list(&mut s);
    let tools: Vec<&str> = l["steps"].as_array().unwrap().iter().map(|st| st["tool"].as_str().unwrap()).collect();
    assert_eq!(tools, ["first", "second"]);

    for i in 0..crate::agent::MAX_STEPS + 5 {
        call(&mut s, "set_develop", None, &[("develop.set", json!({"control": "light.contrast", "value": (i % 50) as f64 + 1.0}))]);
    }
    assert_eq!(s.agent.steps().len(), crate::agent::MAX_STEPS);
    assert_eq!(s.execute("agent.steps.clear", &json!({})).unwrap()["cleared"], crate::agent::MAX_STEPS);
    assert!(s.agent.steps().is_empty());
}
