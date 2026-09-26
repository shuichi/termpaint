//! Maps document pixels to the on-screen canvas bitmap (pan + zoom) and
//! renders that bitmap, which is what gets streamed to the terminal.

use crate::document::{Document, PxRect};

pub const ZOOMS: &[f32] = &[0.125, 0.25, 0.5, 1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0];
const OUTSIDE: [u8; 4] = [44, 45, 52, 255];

pub struct Viewport {
    pub w: u32,
    pub h: u32,
    pub zoom: f32,
    /// Position of the document origin in view pixels.
    pub ox: f32,
    pub oy: f32,
    pub buf: Vec<u8>,
}

impl Viewport {
    pub fn new() -> Self {
        Self { w: 0, h: 0, zoom: 1.0, ox: 0.0, oy: 0.0, buf: Vec::new() }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        // Keep the document centred point stable across resizes.
        let dcx = self.ox - self.w as f32 / 2.0;
        let dcy = self.oy - self.h as f32 / 2.0;
        self.w = w;
        self.h = h;
        self.ox = (dcx + w as f32 / 2.0).round();
        self.oy = (dcy + h as f32 / 2.0).round();
        self.buf = vec![0; (w * h * 4) as usize];
    }

    /// Choose the largest zoom level that fits and centre the document.
    pub fn fit(&mut self, doc: &Document) {
        let fz = (self.w as f32 / doc.width as f32).min(self.h as f32 / doc.height as f32);
        self.zoom = ZOOMS.iter().copied().rfind(|z| *z <= fz.min(1.0)).unwrap_or(ZOOMS[0]);
        self.center(doc);
    }

    pub fn center(&mut self, doc: &Document) {
        self.ox = ((self.w as f32 - doc.width as f32 * self.zoom) / 2.0).round();
        self.oy = ((self.h as f32 - doc.height as f32 * self.zoom) / 2.0).round();
    }

    pub fn view_to_doc(&self, vx: f32, vy: f32) -> (f32, f32) {
        ((vx - self.ox) / self.zoom, (vy - self.oy) / self.zoom)
    }

    /// Zoom to `z`, keeping the document point under view pixel `p` fixed.
    pub fn zoom_to(&mut self, z: f32, p: (f32, f32)) {
        let (dx, dy) = self.view_to_doc(p.0, p.1);
        self.zoom = z;
        self.ox = (p.0 - dx * z).round();
        self.oy = (p.1 - dy * z).round();
    }

    pub fn zoom_step(&mut self, dir: i32, p: (f32, f32)) {
        let idx = ZOOMS.iter().position(|z| (*z - self.zoom).abs() < 1e-6).unwrap_or(3) as i32;
        let n = (idx + dir).clamp(0, ZOOMS.len() as i32 - 1) as usize;
        self.zoom_to(ZOOMS[n], p);
    }

    pub fn view_rect(&self) -> PxRect {
        PxRect::new(0, 0, self.w as i32, self.h as i32)
    }

    /// View-space rectangle covering a document rectangle.
    pub fn doc_to_view(&self, r: PxRect) -> PxRect {
        if r.is_empty() {
            return PxRect::EMPTY;
        }
        PxRect::new(
            (r.x0 as f32 * self.zoom + self.ox).floor() as i32,
            (r.y0 as f32 * self.zoom + self.oy).floor() as i32,
            (r.x1 as f32 * self.zoom + self.ox).ceil() as i32,
            (r.y1 as f32 * self.zoom + self.oy).ceil() as i32,
        )
        .intersect(&self.view_rect())
    }

    /// Render the composite into the view buffer for view rect `r`.
    pub fn render(&mut self, doc: &Document, r: PxRect) {
        let r = r.intersect(&self.view_rect());
        if r.is_empty() {
            return;
        }
        let dw = doc.width as i32;
        let dh = doc.height as i32;
        let inv = 1.0 / self.zoom;
        let cols: Vec<i32> = (r.x0..r.x1)
            .map(|vx| ((vx as f32 + 0.5 - self.ox) * inv).floor() as i32)
            .collect();
        let vw = self.w as usize;
        for vy in r.y0..r.y1 {
            let dy = ((vy as f32 + 0.5 - self.oy) * inv).floor() as i32;
            let row = vy as usize * vw;
            for (k, &dx) in cols.iter().enumerate() {
                let o = (row + r.x0 as usize + k) * 4;
                let px = if dx >= 0 && dy >= 0 && dx < dw && dy < dh {
                    let i = (dy as usize * doc.width as usize + dx as usize) * 4;
                    &doc.composite[i..i + 4]
                } else {
                    &OUTSIDE[..]
                };
                self.buf[o..o + 4].copy_from_slice(px);
            }
        }
    }

    pub fn extract(&self, r: PxRect) -> Vec<u8> {
        let vw = self.w as usize;
        let mut out = Vec::with_capacity((r.width() * r.height() * 4) as usize);
        for y in r.y0..r.y1 {
            let s = (y as usize * vw + r.x0 as usize) * 4;
            out.extend_from_slice(&self.buf[s..s + r.width() as usize * 4]);
        }
        out
    }
}
