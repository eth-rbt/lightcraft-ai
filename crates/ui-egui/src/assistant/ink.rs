//! Photo-bound marks. Coordinates and widths are relative to the displayed photo, so zooming
//! and panning move the marks with it; they never enter the develop recipe or photo export.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2};
use serde::{Deserialize, Serialize};

pub const MAX_MARKS: usize = 512;
const MAX_POINTS: usize = 40_000;
const HISTORY: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Mark {
    Stroke { points: Vec<[f32; 2]>, color: [u8; 4], width: f32 },
    Text { position: [f32; 2], text: String, color: [u8; 4], size: f32 },
}

impl Mark {
    pub fn point_count(&self) -> usize {
        match self {
            Self::Stroke { points, .. } => points.len(),
            Self::Text { .. } => 1,
        }
    }

    pub fn valid(&self) -> bool {
        let point = |p: &[f32; 2]| p.iter().all(|x| x.is_finite() && (-16.0..=16.0).contains(x));
        match self {
            Self::Stroke { points, width, .. } => {
                !points.is_empty() && points.len() <= MAX_POINTS && points.iter().all(point) && width.is_finite() && (0.00001..=1.0).contains(width)
            }
            Self::Text { position, text, size, .. } => {
                point(position) && !text.trim().is_empty() && text.len() <= 1000 && size.is_finite() && (0.00001..=1.0).contains(size)
            }
        }
    }

    pub fn paint(&self, painter: &Painter, photo: Rect) {
        match self {
            Self::Stroke { points, color, width } => {
                let ink = Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
                let width = width * photo.width();
                let points: Vec<_> = points.iter().map(|p| screen(*p, photo)).collect();
                if let Some(p) = points.first().filter(|_| points.len() == 1) {
                    painter.circle_filled(*p, width / 2.0, ink);
                } else {
                    painter.add(Shape::line(points, Stroke::new(width, ink)));
                }
            }
            Self::Text { position, text, color, size } => {
                let ink = Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
                painter.text(screen(*position, photo), Align2::LEFT_TOP, text, FontId::proportional(size * photo.width()), ink);
            }
        }
    }

    fn hit(&self, p: Pos2, photo: Rect) -> bool {
        match self {
            Self::Stroke { points, width, .. } => {
                let radius = 9.0 + width * photo.width() / 2.0;
                points.first().is_some_and(|q| screen(*q, photo).distance(p) <= radius)
                    || points.windows(2).any(|pair| {
                        let [a, b] = pair else { return false };
                        let (a, b) = (screen(*a, photo), screen(*b, photo));
                        let d = b - a;
                        let t = if d.length_sq() > 0.0 { ((p - a).dot(d) / d.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
                        (a + d * t).distance(p) <= radius
                    })
            }
            Self::Text { position, text, size, .. } => {
                let height = size * photo.width();
                Rect::from_min_size(screen(*position, photo), egui::vec2(text.chars().count() as f32 * height * 0.6, height * 1.3))
                    .expand(6.0)
                    .contains(p)
            }
        }
    }
}

pub fn normalized(p: Pos2, photo: Rect) -> [f32; 2] {
    [(p.x - photo.left()) / photo.width().max(1.0), (p.y - photo.top()) / photo.height().max(1.0)]
}

pub fn screen(p: [f32; 2], photo: Rect) -> Pos2 {
    pos2(photo.left() + p[0] * photo.width(), photo.top() + p[1] * photo.height())
}

#[derive(Default)]
pub struct Ink {
    pub marks: Vec<Mark>,
    undo: Vec<Vec<Mark>>,
    redo: Vec<Vec<Mark>>,
}

impl Ink {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn save(&mut self) {
        if self.undo.len() == HISTORY {
            self.undo.remove(0);
        }
        self.undo.push(self.marks.clone());
        self.redo.clear();
    }

    pub fn append(&mut self, marks: Vec<Mark>) -> Result<(), String> {
        if marks.is_empty() || marks.iter().any(|m| !m.valid()) {
            return Err("Invalid annotation".into());
        }
        let points: usize = self.marks.iter().chain(&marks).map(Mark::point_count).sum();
        if self.marks.len() + marks.len() > MAX_MARKS || points > MAX_POINTS {
            return Err("This photo's annotation layer is full. Clear some marks before drawing more.".into());
        }
        self.save();
        self.marks.extend(marks);
        Ok(())
    }

    pub fn erase(&mut self, p: Pos2, photo: Rect) -> bool {
        let Some(index) = self.marks.iter().rposition(|m| m.hit(p, photo)) else { return false };
        self.save();
        self.marks.remove(index);
        true
    }

    pub fn clear(&mut self) {
        if !self.marks.is_empty() {
            self.save();
            self.marks.clear();
        }
    }

    pub fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.marks, previous));
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.marks, next));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke() -> Mark {
        Mark::Stroke { points: vec![[0.1, 0.2], [0.9, 0.2]], color: [57, 123, 119, 255], width: 0.01 }
    }

    #[test]
    fn erasing_crosses_segments_and_undo_restores_transparency() {
        let mut ink = Ink::default();
        ink.append(vec![stroke()]).unwrap();
        assert!(ink.erase(pos2(50.0, 20.0), Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 100.0))));
        assert!(ink.marks.is_empty());
        ink.undo();
        assert_eq!(ink.marks.len(), 1);
        ink.redo();
        assert!(ink.marks.is_empty());
    }

    #[test]
    fn marks_follow_photo_translation_and_scale() {
        let rect = Rect::from_min_size(pos2(200.0, 100.0), egui::vec2(800.0, 600.0));
        assert_eq!(screen([0.5, 0.5], rect), pos2(600.0, 400.0));
        assert_eq!(normalized(pos2(600.0, 400.0), rect), [0.5, 0.5]);
    }

    #[test]
    fn invalid_batch_is_atomic_and_new_work_clears_redo() {
        let mut ink = Ink::default();
        ink.append(vec![stroke()]).unwrap();
        let bad = Mark::Stroke { points: vec![[f32::NAN, 0.0]], color: [0; 4], width: 0.1 };
        assert!(ink.append(vec![stroke(), bad]).is_err());
        assert_eq!(ink.marks.len(), 1);
        ink.undo();
        ink.append(vec![stroke()]).unwrap();
        assert!(!ink.can_redo());
    }
}
