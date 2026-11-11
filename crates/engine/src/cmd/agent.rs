//! `agent.*`: the agent step log (see [`crate::agent`]). The MCP server brackets each tool call
//! with `agent.step.begin` / `agent.step.end`; frontends list the steps and move between them.

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::agent::{AgentStep, Position};
use crate::{Result, Session};

fn rect_json(r: lightcraft_geom::Rect) -> Value {
    json!([r.x0, r.y0, r.x1, r.y1])
}

fn step_json(step: &AgentStep) -> Value {
    let mut v = json!({
        "id": step.id,
        "tool": step.tool,
        "note": step.note,
        "photo": step.photo.0,
        "changes": step.control_changes(),
        "other": step.other_changes(),
    });
    if step.crop_changed() {
        let (from, to) = (&step.before.crop, &step.after.crop);
        v["crop"] = json!({
            "from": rect_json(from.geometry.rect),
            "to": rect_json(to.geometry.rect),
            "fromAngle": from.geometry.angle,
            "toAngle": to.geometry.angle,
        });
    }
    let masks = step.mask_changes();
    if !masks.is_empty() {
        v["masks"] = json!(masks);
    }
    v
}

/// `{photo?}` → the steps (of one photo, or all) and where the active photo's settings sit.
fn list(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "agent.steps.list";
    let only = match p.get("photo") {
        None | Some(Value::Null) => None,
        Some(v) => Some(PhotoId(v.as_u64().ok_or_else(|| bad(C, "`photo` must be a photo id (a whole number)"))?)),
    };
    let steps: Vec<Value> = s.agent.steps().iter().filter(|st| only.is_none_or(|id| st.photo == id)).map(step_json).collect();
    let position = s.active().and_then(|id| s.develop_of(id).map(|d| s.agent.position(id, &d)));
    Ok(json!({
        "steps": steps,
        "current": match position { Some(Position::After(id)) => json!(id), _ => Value::Null },
        "atStart": position == Some(Position::Start),
        "open": s.agent.is_open(),
    }))
}

/// `{step, side?: after|before}`: make the step's photo active and load the settings the step
/// left (or started from). A History step with those settings is gone to; settings History does
/// not list are loaded without touching it. Either way stepping adds no History step, and it is
/// one undo step.
fn goto(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "agent.steps.goto";
    let id = p.get("step").and_then(Value::as_u64).ok_or_else(|| bad(C, "missing `step` (an agent step id)"))?;
    let before = match str_param(p, "side").unwrap_or("after") {
        "after" => false,
        "before" => true,
        other => return Err(bad(C, format!("unknown side `{other}` (after, before)"))),
    };
    let step = s.agent.step(id).cloned().ok_or_else(|| bad(C, format!("no agent step {id}")))?;
    let target = if before { step.before } else { step.after };
    if s.catalog.photo(step.photo).is_none() {
        return Err(bad(C, format!("photo {} of step {id} is no longer in the library", step.photo.0)));
    }
    if s.active() != Some(step.photo) {
        s.execute("library.select", &json!({"ids": [step.photo.0]}))?;
    }
    let photo = s.catalog.photo(step.photo).ok_or_else(|| bad(C, "no photo"))?;
    if *photo.develop != *target {
        let in_history = photo.history.iter().rposition(|h| std::sync::Arc::ptr_eq(&h.settings, &target) || *h.settings == *target);
        match in_history {
            Some(i) => {
                super::develop::go_to_history_step(s, C, step.photo, i)?;
            }
            // settings History does not list (the ones the photo had before the agent's first
            // step, or a step History has since dropped): loaded as they were, leaving the History
            // list and its cursor alone. A new History step here would drop the steps after the
            // cursor, the agent's own among them.
            None => {
                let edited = Some((s.clock)());
                s.commit("Agent Step", lightcraft_catalog::Op::SetDevelop { id: step.photo, settings: target, label: "Agent Step".into(), edited })?;
            }
        }
    }
    Ok(json!({"step": id, "side": if before { "before" } else { "after" }, "photo": step.photo.0}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "agent.step.begin",
            "Begin Agent Step",
            [],
            None,
            "{tool, note?} — start of one agent tool call: develop edits until agent.step.end are recorded as its step. `note` is the agent's reason, shown with the step",
            always,
            |s, p| {
                const C: &str = "agent.step.begin";
                let tool = str_param(p, "tool").map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(C, "missing `tool`"))?;
                s.agent.begin(&s.catalog, tool, str_param(p, "note"));
                Ok(json!({}))
            }
        ),
        cmd!(
            query "agent.step.end",
            "End Agent Step",
            [],
            None,
            "{} → {steps: [id]} — end of the tool call: one step per photo whose develop settings it changed (none for a call that edited nothing)",
            always,
            |s, _| Ok(json!({"steps": s.agent.end(&s.catalog)}))
        ),
        cmd!(
            query "agent.steps.list",
            "List Agent Steps",
            [],
            None,
            "{photo?} → {steps: [{id, tool, note, photo, changes: [{id, label, section, from, to, fromText, toText}], crop?: {from, to, fromAngle, toAngle}, masks?: {added, changed, removed}, other}], current: the step the active photo's settings are at | null, atStart, open} — oldest first; `other`: the step also changed something that is not a slider, the crop or a mask",
            always,
            list
        ),
        cmd!(
            "agent.steps.goto",
            "Go to Agent Step",
            [],
            None,
            "{step, side?: after|before} — makes the step's photo active and loads the settings the step left (`before`: the ones it started from); adds no History step",
            always,
            goto
        ),
        cmd!(query "agent.steps.clear", "Clear Agent Steps", [], None, "{} → {cleared: n} — forgets the steps; the photos keep their edits", always, |s, _| {
            Ok(json!({"cleared": s.agent.clear()}))
        }),
    ]
}
