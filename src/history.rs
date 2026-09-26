//! Undo / redo.

use crate::document::{Document, Layer, PxRect};

pub enum Edit {
    Pixels { layer: usize, rect: PxRect, before: Vec<u8>, after: Vec<u8> },
    AddLayer { index: usize, layer: Layer },
    DeleteLayer { index: usize, layer: Layer },
    MoveLayer { from: usize, to: usize },
    Merge { index: usize, upper: Layer, lower_before: Vec<u8> },
}

impl Edit {
    fn bytes(&self) -> usize {
        match self {
            Edit::Pixels { before, after, .. } => before.len() + after.len(),
            Edit::AddLayer { layer, .. } | Edit::DeleteLayer { layer, .. } => layer.pixels.len(),
            Edit::MoveLayer { .. } => 0,
            Edit::Merge { upper, lower_before, .. } => upper.pixels.len() + lower_before.len(),
        }
    }
}

/// What part of the document an undo/redo step changed.
pub enum Changed {
    Region(PxRect),
    Everything,
}

pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    budget: usize,
}

impl History {
    pub fn new(budget_bytes: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), budget: budget_bytes }
    }

    pub fn push(&mut self, e: Edit) {
        self.redo.clear();
        self.undo.push(e);
        let mut total: usize = self.undo.iter().map(Edit::bytes).sum();
        while total > self.budget && self.undo.len() > 1 {
            total -= self.undo.remove(0).bytes();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self, doc: &mut Document) -> Option<Changed> {
        let e = self.undo.pop()?;
        let (inv, ch) = apply(doc, e, true);
        self.redo.push(inv);
        Some(ch)
    }

    pub fn redo(&mut self, doc: &mut Document) -> Option<Changed> {
        let e = self.redo.pop()?;
        let (inv, ch) = apply(doc, e, false);
        self.undo.push(inv);
        Some(ch)
    }
}

/// Apply an edit in the given direction; returns the edit to store on the
/// opposite stack (same edit, since each variant knows both directions).
fn apply(doc: &mut Document, e: Edit, undo: bool) -> (Edit, Changed) {
    match e {
        Edit::Pixels { layer, rect, before, after } => {
            doc.write_rect(layer, rect, if undo { &before } else { &after });
            doc.active = layer;
            (Edit::Pixels { layer, rect, before, after }, Changed::Region(rect))
        }
        Edit::AddLayer { index, layer } => {
            let layer = if undo {
                let l = doc.layers.remove(index);
                doc.active = index.saturating_sub(1).min(doc.layers.len() - 1);
                l
            } else {
                doc.layers.insert(index, layer.clone());
                doc.active = index;
                layer
            };
            (Edit::AddLayer { index, layer }, Changed::Everything)
        }
        Edit::DeleteLayer { index, layer } => {
            let layer = if undo {
                doc.layers.insert(index, layer.clone());
                doc.active = index;
                layer
            } else {
                let l = doc.layers.remove(index);
                doc.active = index.saturating_sub(1).min(doc.layers.len() - 1);
                l
            };
            (Edit::DeleteLayer { index, layer }, Changed::Everything)
        }
        Edit::MoveLayer { from, to } => {
            let (a, b) = if undo { (to, from) } else { (from, to) };
            let l = doc.layers.remove(a);
            doc.layers.insert(b, l);
            doc.active = b;
            (Edit::MoveLayer { from, to }, Changed::Everything)
        }
        Edit::Merge { index, upper, lower_before } => {
            if undo {
                doc.layers[index - 1].pixels.copy_from_slice(&lower_before);
                doc.layers.insert(index, upper.clone());
                doc.active = index;
            } else {
                doc.merge_down(index);
            }
            (Edit::Merge { index, upper, lower_before }, Changed::Everything)
        }
    }
}
