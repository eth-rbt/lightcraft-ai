//! The agent step log: what an agent's tool calls changed, one step per call and photo.
//!
//! An MCP tool call is bracketed by `agent.step.begin` / `agent.step.end` (`cmd/agent.rs`). While
//! a step is open, every develop edit that reaches the catalog is observed here before it is
//! applied, so the settings each photo had when the call started are kept. At the end, a photo
//! whose settings differ from those becomes one [`AgentStep`] holding both sides. A call that
//! edits nothing (a query, a render, a screenshot) leaves no step.
//!
//! The step keeps the settings themselves, not History indices: History drops its oldest steps
//! and can be cleared, and the frontends that show a step (what moved, where on the photo) need
//! both sides to draw it. The log lives with the session and is not saved with the library.

use std::collections::BTreeMap;
use std::sync::Arc;

use lightcraft_catalog::{Catalog, Op, PhotoId};
use lightcraft_develop::{DevelopSettings, controls};
use serde::Serialize;

/// Steps kept; the oldest are dropped past it.
pub const MAX_STEPS: usize = 500;
/// Photos one tool call can make steps for (a batch edit of a whole shoot is not a list of steps).
pub const MAX_PHOTOS_PER_CALL: usize = 64;
/// Characters kept of a tool name and of the agent's note.
pub const MAX_TOOL_CHARS: usize = 120;
pub const MAX_NOTE_CHARS: usize = 1000;

/// What one tool call changed on one photo.
#[derive(Clone, Debug)]
pub struct AgentStep {
    /// Unique in the session, increasing.
    pub id: u64,
    /// The MCP tool the agent called (`set_develop`, `crop`, `cmd_mask_add`…).
    pub tool: String,
    /// Why, in the agent's words, when it said.
    pub note: Option<String>,
    pub photo: PhotoId,
    /// The photo's settings when the call started and when it ended.
    pub before: Arc<DevelopSettings>,
    pub after: Arc<DevelopSettings>,
}

/// A develop control whose value a step changed.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlChange {
    pub id: String,
    pub label: &'static str,
    pub section: &'static str,
    pub from: f64,
    pub to: f64,
    /// The values as the slider shows them (`+0.70`).
    pub from_text: String,
    pub to_text: String,
}

/// Mask ids a step added, changed and removed.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MaskChanges {
    pub added: Vec<u32>,
    pub changed: Vec<u32>,
    pub removed: Vec<u32>,
}

impl MaskChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

impl AgentStep {
    /// The sliders the step moved, in panel order.
    pub fn control_changes(&self) -> Vec<ControlChange> {
        control_changes(&self.before, &self.after)
    }

    pub fn crop_changed(&self) -> bool {
        self.before.crop != self.after.crop
    }

    pub fn mask_changes(&self) -> MaskChanges {
        let mut out = MaskChanges::default();
        for m in &self.after.masks {
            match self.before.masks.iter().find(|b| b.id == m.id) {
                None => out.added.push(m.id),
                Some(b) if b != m => out.changed.push(m.id),
                Some(_) => {}
            }
        }
        out.removed = self.before.masks.iter().filter(|b| !self.after.masks.iter().any(|m| m.id == b.id)).map(|b| b.id).collect();
        out
    }

    /// The step changed something that is neither a slider, the crop nor a mask (a curve, the
    /// profile, spots…): the listed changes do not account for the whole difference.
    pub fn other_changes(&self) -> bool {
        let mut d = (*self.before).clone();
        for c in self.control_changes() {
            controls::set(&mut d, &c.id, c.to);
        }
        d.crop = self.after.crop;
        d.masks = self.after.masks.clone();
        d != *self.after
    }
}

/// The controls whose value differs between two settings, in panel order. A per-photo control
/// (`pointColor.0.hueShift`) that only `after` has starts from its default.
pub fn control_changes(before: &DevelopSettings, after: &DevelopSettings) -> Vec<ControlChange> {
    let plain = controls::CONTROLS.iter().map(|spec| (spec.id.to_string(), spec));
    plain
        .chain(controls::indexed_instances(after))
        .filter_map(|(id, spec)| {
            let to = controls::get(after, &id)?;
            let from = controls::get(before, &id).unwrap_or(spec.default);
            // NaN on either side is not a change a slider can show
            let moved = (from - to).abs() > 1e-9;
            moved.then(|| ControlChange {
                label: spec.label,
                section: spec.section.label(),
                from,
                to,
                from_text: spec.format(from),
                to_text: spec.format(to),
                id,
            })
        })
        .collect()
}

struct Open {
    tool: String,
    note: Option<String>,
    /// The settings each edited photo had before the call's first edit of it.
    before: BTreeMap<PhotoId, Arc<DevelopSettings>>,
}

/// The session's agent steps and the tool call in progress.
#[derive(Default)]
pub struct AgentLog {
    steps: Vec<AgentStep>,
    open: Option<Open>,
    next_id: u64,
    generation: u64,
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

impl AgentLog {
    /// Steps, oldest first.
    pub fn steps(&self) -> &[AgentStep] {
        &self.steps
    }

    pub fn step(&self, id: u64) -> Option<&AgentStep> {
        self.steps.iter().find(|s| s.id == id)
    }

    /// Changes whenever the list of steps does, so a frontend can cache what it derives from it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A tool call is in progress.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Start observing edits for one tool call. A call still open (its end never arrived) is
    /// ended first, so its edits are not credited to this one.
    pub fn begin(&mut self, catalog: &Catalog, tool: &str, note: Option<&str>) {
        self.end(catalog);
        let note = note.map(str::trim).filter(|n| !n.is_empty()).map(|n| clip(n, MAX_NOTE_CHARS));
        self.open = Some(Open { tool: clip(tool, MAX_TOOL_CHARS), note, before: BTreeMap::new() });
    }

    /// Called with every op before the catalog applies it: keeps the settings of the photos a
    /// develop edit is about to change, the first time the open call touches each.
    pub fn observe(&mut self, catalog: &Catalog, op: &Op) {
        let Some(open) = &mut self.open else { return };
        let mut pending = vec![op];
        while let Some(op) = pending.pop() {
            match op {
                Op::SetDevelop { id, .. } if open.before.len() < MAX_PHOTOS_PER_CALL || open.before.contains_key(id) => {
                    if let Some(photo) = catalog.photo(*id) {
                        open.before.entry(*id).or_insert_with(|| photo.develop.clone());
                    }
                }
                Op::Batch { ops } => pending.extend(ops),
                _ => {}
            }
        }
    }

    /// End the open call: one step per photo whose settings it changed. Returns the new step ids.
    pub fn end(&mut self, catalog: &Catalog) -> Vec<u64> {
        let Some(open) = self.open.take() else { return Vec::new() };
        let mut made = Vec::new();
        for (photo, before) in open.before {
            let Some(after) = catalog.photo(photo).map(|p| p.develop.clone()) else { continue };
            if *before == *after {
                continue;
            }
            self.next_id = self.next_id.saturating_add(1);
            made.push(self.next_id);
            self.steps.push(AgentStep { id: self.next_id, tool: open.tool.clone(), note: open.note.clone(), photo, before, after });
        }
        if !made.is_empty() {
            let extra = self.steps.len().saturating_sub(MAX_STEPS);
            self.steps.drain(..extra);
            self.generation = self.generation.wrapping_add(1);
        }
        made
    }

    /// Forget every step (the photos keep their edits). Returns how many there were.
    pub fn clear(&mut self) -> usize {
        let n = self.steps.len();
        if n > 0 {
            self.steps.clear();
            self.generation = self.generation.wrapping_add(1);
        }
        n
    }

    /// Where `settings` (photo `photo`'s current ones) sit among its steps: the latest step that
    /// left exactly these settings, or [`Position::Start`] when they are what its first step
    /// started from. `Elsewhere` when the photo was edited since, or has no steps.
    pub fn position(&self, photo: PhotoId, settings: &Arc<DevelopSettings>) -> Position {
        // the same allocation first: a frontend asks every frame, and the settings a step left are
        // usually the very ones the photo still has
        let same = |a: &Arc<DevelopSettings>| Arc::ptr_eq(a, settings) || **a == **settings;
        let mut own = self.steps.iter().filter(|s| s.photo == photo);
        let first = own.next();
        if let Some(at) = self.steps.iter().rev().find(|s| s.photo == photo && same(&s.after)) {
            return Position::After(at.id);
        }
        match first {
            Some(first) if same(&first.before) => Position::Start,
            _ => Position::Elsewhere,
        }
    }
}

/// See [`AgentLog::position`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    /// Before the photo's first step.
    Start,
    /// At the settings step `id` left.
    After(u64),
    Elsewhere,
}
