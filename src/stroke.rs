//! Rasterisation of strokes and shapes.
//!
//! Every paint operation renders a *coverage mask* (max coverage per pixel
//! over the whole operation) and then re-derives the affected pixels from a
//! snapshot of the layer taken when the operation began. This keeps stroke
//! opacity uniform where brush dabs overlap.
//!
//! A second, *temporary* mask holds geometry that is redrawn on every input
//! event: the live shape preview, and the provisional tail of a freehand
//! stroke that reaches from the last committed spline point to the current
//! pointer position (so the line never lags a sample behind the mouse).

use crate::document::{Document, PxRect, Rgba, blend_over};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Paint,
    Erase,
}

#[derive(Clone, Copy, Debug)]
pub struct BrushSpec {
    /// Diameter in pixels.
    pub size: f32,
    /// 0..=1, 1 = hard edge (still anti-aliased).
    pub hardness: f32,
    /// false = aliased pixel pencil.
    pub antialias: bool,
}

/// One in-progress paint operation on a single layer.
pub struct Operation {
    pub layer: usize,
    pub mode: Mode,
    pub color: Rgba,
    /// 0..=1 opacity of the whole operation.
    pub opacity: f32,
    pub brush: BrushSpec,
    snapshot: Vec<u8>,
    mask: Vec<u8>,
    /// Temporary coverage (shape preview / freehand tail), max-combined
    /// with `mask`.
    temp: Vec<u8>,
    /// Bounding box of everything currently in `temp`.
    temp_area: PxRect,
    /// Route dabs into `temp` instead of `mask`.
    to_temp: bool,
    width: u32,
    height: u32,
    /// Area touched so far (union over the whole operation).
    pub touched: PxRect,
    // Freehand state
    points: Vec<(f32, f32)>,
    carry: f32,
    last_dab: Option<(f32, f32)>,
}

impl Operation {
    pub fn begin(doc: &Document, mode: Mode, color: Rgba, opacity: f32, brush: BrushSpec) -> Self {
        let layer = doc.active;
        Self {
            layer,
            mode,
            color,
            opacity,
            brush,
            snapshot: doc.layers[layer].pixels.clone(),
            mask: vec![0; (doc.width * doc.height) as usize],
            temp: vec![0; (doc.width * doc.height) as usize],
            temp_area: PxRect::EMPTY,
            to_temp: false,
            width: doc.width,
            height: doc.height,
            touched: PxRect::EMPTY,
            points: Vec::new(),
            carry: 0.0,
            last_dab: None,
        }
    }

    pub fn snapshot_rect(&self, r: PxRect) -> Vec<u8> {
        let w = self.width as usize;
        let mut out = Vec::with_capacity((r.width() * r.height() * 4) as usize);
        for y in r.y0..r.y1 {
            let s = (y as usize * w + r.x0 as usize) * 4;
            out.extend_from_slice(&self.snapshot[s..s + r.width() as usize * 4]);
        }
        out
    }

    fn bounds(&self) -> PxRect {
        PxRect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// Re-derive layer pixels in `r` from snapshot + mask.
    fn apply(&self, doc: &mut Document, r: PxRect) {
        let r = r.intersect(&self.bounds());
        if r.is_empty() {
            return;
        }
        let w = self.width as usize;
        let pixels = &mut doc.layers[self.layer].pixels;
        let [cr, cg, cb, ca] = self.color.0;
        let base_alpha = self.opacity * ca as f32 / 255.0;
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                let mi = y as usize * w + x as usize;
                let i = mi * 4;
                let m = self.mask[mi].max(self.temp[mi]);
                let dst = &mut pixels[i..i + 4];
                dst.copy_from_slice(&self.snapshot[i..i + 4]);
                if m == 0 {
                    continue;
                }
                let cov = m as f32 / 255.0;
                match self.mode {
                    Mode::Paint => blend_over(dst, [cr, cg, cb], cov * base_alpha),
                    Mode::Erase => {
                        let a = dst[3] as f32 * (1.0 - cov * self.opacity);
                        dst[3] = (a + 0.5) as u8;
                    }
                }
            }
        }
    }

    /// Stamp a single dab into the mask; returns its bounding box.
    fn dab(&mut self, cx: f32, cy: f32) -> PxRect {
        let b = self.brush;
        let w = self.width as i32;
        let h = self.height as i32;
        let bounds = self.bounds();
        let target = if self.to_temp { &mut self.temp } else { &mut self.mask };
        let area;
        if !b.antialias {
            // Aliased pencil: integer disc centred on the pixel under the point.
            let px = cx.floor() as i32;
            let py = cy.floor() as i32;
            let rad = ((b.size - 1.0) / 2.0).max(0.0);
            let ri = rad.ceil() as i32;
            area = PxRect::new(px - ri, py - ri, px + ri + 1, py + ri + 1).intersect(&bounds);
            let r2 = (rad + 0.35) * (rad + 0.35);
            for y in area.y0..area.y1 {
                for x in area.x0..area.x1 {
                    let dx = (x - px) as f32;
                    let dy = (y - py) as f32;
                    if dx * dx + dy * dy <= r2 {
                        target[(y * w + x) as usize] = 255;
                    }
                }
            }
            return area;
        }
        let r = (b.size / 2.0).max(0.5);
        let inner = r * b.hardness.clamp(0.0, 1.0);
        let edge = (r - inner).max(1.0);
        area = PxRect::new(
            (cx - r - 1.0).floor() as i32,
            (cy - r - 1.0).floor() as i32,
            (cx + r + 1.0).ceil() as i32,
            (cy + r + 1.0).ceil() as i32,
        )
        .intersect(&PxRect::new(0, 0, w, h));
        for y in area.y0..area.y1 {
            let dy = y as f32 + 0.5 - cy;
            for x in area.x0..area.x1 {
                let dx = x as f32 + 0.5 - cx;
                let d = (dx * dx + dy * dy).sqrt();
                let t = ((r + 0.5 - d) / edge).clamp(0.0, 1.0);
                if t <= 0.0 {
                    continue;
                }
                // smoothstep for a soft falloff
                let c = t * t * (3.0 - 2.0 * t);
                let v = (c * 255.0 + 0.5) as u8;
                let m = &mut target[(y * w + x) as usize];
                if v > *m {
                    *m = v;
                }
            }
        }
        area
    }

    fn spacing(&self) -> f32 {
        if self.brush.antialias { (self.brush.size * 0.12).max(0.4) } else { 0.5 }
    }

    /// Stamp dabs along a straight segment with even spacing (carrying the
    /// remainder between segments so spacing stays uniform along the curve).
    fn segment(&mut self, a: (f32, f32), b: (f32, f32), dirty: &mut PxRect) {
        let sp = self.spacing();
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len = (dx * dx + dy * dy).sqrt();
        if self.last_dab.is_none() {
            *dirty = dirty.union(&self.dab(a.0, a.1));
            self.last_dab = Some(a);
            self.carry = 0.0;
        }
        if len <= f32::EPSILON {
            return;
        }
        let mut t = sp - self.carry;
        while t <= len {
            let p = (a.0 + dx * t / len, a.1 + dy * t / len);
            *dirty = dirty.union(&self.dab(p.0, p.1));
            self.last_dab = Some(p);
            t += sp;
        }
        self.carry = len - (t - sp);
    }

    /// Catmull-Rom spline between p1 and p2, flattened into short segments.
    fn spline(&mut self, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), dirty: &mut PxRect) {
        let dist = ((p2.0 - p1.0).powi(2) + (p2.1 - p1.1).powi(2)).sqrt();
        let steps = ((dist / 2.0).ceil() as usize).clamp(1, 64);
        let mut prev = p1;
        for s in 1..=steps {
            let t = s as f32 / steps as f32;
            let t2 = t * t;
            let t3 = t2 * t;
            let f = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * ((2.0 * b) + (-a + c) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (-a + 3.0 * b - 3.0 * c + d) * t3)
            };
            let p = (f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1));
            self.segment(prev, p, dirty);
            prev = p;
        }
    }

    /// Feed a new freehand sample. The committed spline lags one sample
    /// behind (it needs a look-ahead point); the gap up to `p` is drawn as a
    /// provisional tail in the temp mask and replaced on the next sample.
    pub fn freehand_to(&mut self, doc: &mut Document, p: (f32, f32)) -> PxRect {
        if let Some(&last) = self.points.last()
            && (last.0 - p.0).abs() < 0.01
            && (last.1 - p.1).abs() < 0.01
        {
            return PxRect::EMPTY;
        }
        self.points.push(p);
        let n = self.points.len();
        let mut dirty = PxRect::EMPTY;
        match n {
            1 => {
                dirty = self.dab(p.0, p.1);
                self.last_dab = Some(p);
            }
            2 => {}
            _ => {
                let p0 = if n >= 4 { self.points[n - 4] } else { self.points[n - 3] };
                let (p1, p2, p3) = (self.points[n - 3], self.points[n - 2], self.points[n - 1]);
                self.spline(p0, p1, p2, p3, &mut dirty);
            }
        }
        dirty = dirty.union(&self.clear_temp());
        if n >= 2 {
            dirty = dirty.union(&self.stamp_tail());
        }
        self.commit_dirty(doc, dirty)
    }

    /// Draw the provisional segment from the last committed point to the
    /// newest sample into the temp mask, without disturbing spacing state.
    fn stamp_tail(&mut self) -> PxRect {
        let n = self.points.len();
        let p0 = if n >= 3 { self.points[n - 3] } else { self.points[n - 2] };
        let (p1, p2) = (self.points[n - 2], self.points[n - 1]);
        let saved = (self.carry, self.last_dab);
        self.to_temp = true;
        let mut d = PxRect::EMPTY;
        self.spline(p0, p1, p2, p2, &mut d);
        self.to_temp = false;
        (self.carry, self.last_dab) = saved;
        self.temp_area = d;
        d
    }

    /// Zero the temp mask; returns the area that must be re-applied.
    fn clear_temp(&mut self) -> PxRect {
        let old = std::mem::replace(&mut self.temp_area, PxRect::EMPTY);
        let w = self.width as usize;
        for y in old.y0..old.y1 {
            let row = y as usize * w;
            self.temp[row + old.x0 as usize..row + old.x1 as usize].fill(0);
        }
        old
    }

    /// Finish the trailing spline segment of a freehand stroke.
    pub fn freehand_end(&mut self, doc: &mut Document) -> PxRect {
        let n = self.points.len();
        let mut dirty = self.clear_temp();
        if n >= 2 {
            let p0 = if n >= 3 { self.points[n - 3] } else { self.points[n - 2] };
            let (p1, p2) = (self.points[n - 2], self.points[n - 1]);
            self.spline(p0, p1, p2, p2, &mut dirty);
        }
        self.commit_dirty(doc, dirty)
    }

    fn commit_dirty(&mut self, doc: &mut Document, dirty: PxRect) -> PxRect {
        if dirty.is_empty() {
            return dirty;
        }
        self.apply(doc, dirty);
        self.touched = self.touched.union(&dirty);
        dirty
    }

    /// Replace the current shape preview with a new one described by a
    /// polyline (closed if `closed`).
    pub fn set_shape(&mut self, doc: &mut Document, pts: &[(f32, f32)], closed: bool) -> PxRect {
        let old = self.clear_temp();
        let mut dirty = PxRect::EMPTY;
        self.last_dab = None;
        self.carry = 0.0;
        self.to_temp = true;
        if let Some(&first) = pts.first() {
            if pts.len() == 1 {
                dirty = self.dab(first.0, first.1);
            }
            for win in pts.windows(2) {
                self.segment(win[0], win[1], &mut dirty);
            }
            if closed && pts.len() > 2 {
                self.segment(*pts.last().unwrap(), first, &mut dirty);
            }
        }
        self.to_temp = false;
        self.temp_area = dirty;
        let area = old.union(&dirty);
        self.apply(doc, area);
        self.touched = self.touched.union(&area);
        area
    }

    /// Flood fill starting at (x, y) on the operation's layer.
    pub fn flood_fill(&mut self, doc: &mut Document, x: i32, y: i32, tolerance: u8) -> PxRect {
        if !self.bounds().intersect(&PxRect::new(x, y, x + 1, y + 1)).is_empty() {
            let w = self.width as i32;
            let h = self.height as i32;
            let px = |i: usize| -> [u8; 4] { self.snapshot[i * 4..i * 4 + 4].try_into().unwrap() };
            let target = px((y * w + x) as usize);
            let tol = tolerance as i32;
            let matches = |c: [u8; 4]| -> bool {
                // Fully transparent pixels match each other regardless of RGB.
                if c[3] == 0 && target[3] == 0 {
                    return true;
                }
                (0..4).all(|k| (c[k] as i32 - target[k] as i32).abs() <= tol)
            };
            let mut visited = vec![false; (w * h) as usize];
            let mut stack = vec![(x, y)];
            let mut area = PxRect::EMPTY;
            while let Some((sx, sy)) = stack.pop() {
                let row = sy * w;
                if visited[(row + sx) as usize] {
                    continue;
                }
                let mut l = sx;
                while l > 0 && !visited[(row + l - 1) as usize] && matches(px((row + l - 1) as usize)) {
                    l -= 1;
                }
                let mut r = sx;
                while r + 1 < w && !visited[(row + r + 1) as usize] && matches(px((row + r + 1) as usize)) {
                    r += 1;
                }
                for xx in l..=r {
                    let i = (row + xx) as usize;
                    visited[i] = true;
                    self.mask[i] = 255;
                    for ny in [sy - 1, sy + 1] {
                        if ny >= 0 && ny < h {
                            let ni = (ny * w + xx) as usize;
                            if !visited[ni] && matches(px(ni)) {
                                stack.push((xx, ny));
                            }
                        }
                    }
                }
                area = area.union(&PxRect::new(l, sy, r + 1, sy + 1));
            }
            return self.commit_dirty(doc, area);
        }
        PxRect::EMPTY
    }
}

/// Polyline approximating an ellipse inscribed in the box a..b.
pub fn ellipse_points(a: (f32, f32), b: (f32, f32)) -> Vec<(f32, f32)> {
    let cx = (a.0 + b.0) / 2.0;
    let cy = (a.1 + b.1) / 2.0;
    let rx = (b.0 - a.0).abs() / 2.0;
    let ry = (b.1 - a.1).abs() / 2.0;
    let n = ((rx + ry) * 1.2).clamp(12.0, 720.0) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            (cx + rx * t.cos(), cy + ry * t.sin())
        })
        .collect()
}

pub fn rect_points(a: (f32, f32), b: (f32, f32)) -> Vec<(f32, f32)> {
    vec![a, (b.0, a.1), b, (a.0, b.1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brush(size: f32) -> BrushSpec {
        BrushSpec { size, hardness: 1.0, antialias: true }
    }

    #[test]
    fn freehand_draws_continuous_line() {
        let mut doc = Document::new(64, 16);
        let mut op = Operation::begin(&doc, Mode::Paint, Rgba::BLACK, 1.0, brush(3.0));
        op.freehand_to(&mut doc, (2.0, 8.0));
        op.freehand_to(&mut doc, (30.0, 8.0));
        op.freehand_to(&mut doc, (60.0, 8.0));
        op.freehand_end(&mut doc);
        for x in 3..59 {
            let i = (8 * 64 + x) * 4;
            assert!(doc.layers[0].pixels[i] < 40, "gap at x={x}");
        }
    }

    #[test]
    fn stroke_reaches_latest_sample_immediately() {
        let mut doc = Document::new(64, 16);
        let mut op = Operation::begin(&doc, Mode::Paint, Rgba::BLACK, 1.0, brush(3.0));
        op.freehand_to(&mut doc, (2.0, 8.0));
        op.freehand_to(&mut doc, (30.0, 8.0));
        assert!(doc.layers[0].pixels[(8 * 64 + 29) * 4] < 40, "tail not drawn");
        // The tail is replaced (not accumulated) when the stroke turns.
        op.freehand_to(&mut doc, (30.0, 2.0));
        op.freehand_end(&mut doc);
        let v = doc.layers[0].pixels[(8 * 64 + 29) * 4];
        assert!(v < 40 || v == 255, "unexpected partial value {v}");
    }

    #[test]
    fn overlapping_dabs_do_not_accumulate_opacity() {
        let mut doc = Document::new(32, 32);
        let mut op = Operation::begin(&doc, Mode::Paint, Rgba::BLACK, 0.5, brush(8.0));
        for i in 0..20 {
            op.freehand_to(&mut doc, (16.0 + (i % 2) as f32, 16.0));
        }
        op.freehand_end(&mut doc);
        let i = (16 * 32 + 16) * 4;
        let v = doc.layers[0].pixels[i];
        assert!((125..=130).contains(&v), "got {v}");
    }

    #[test]
    fn shape_preview_restores_previous() {
        let mut doc = Document::new(32, 32);
        let mut op = Operation::begin(&doc, Mode::Paint, Rgba::BLACK, 1.0, brush(1.0));
        op.set_shape(&mut doc, &[(1.0, 1.5), (30.0, 1.5)], false);
        op.set_shape(&mut doc, &[(1.0, 20.5), (30.0, 20.5)], false);
        assert_eq!(doc.layers[0].pixels[(32 + 10) * 4], 255);
        assert!(doc.layers[0].pixels[(20 * 32 + 10) * 4] < 128);
    }

    #[test]
    fn fill_stops_at_border() {
        let mut doc = Document::new(16, 16);
        let mut op = Operation::begin(&doc, Mode::Paint, Rgba::BLACK, 1.0, BrushSpec { size: 1.0, hardness: 1.0, antialias: false });
        op.set_shape(&mut doc, &[(8.5, 0.0), (8.5, 15.9)], false);
        let _ = op;
        let mut op = Operation::begin(&doc, Mode::Paint, Rgba([255, 0, 0, 255]), 1.0, brush(1.0));
        op.flood_fill(&mut doc, 0, 0, 0);
        assert_eq!(&doc.layers[0].pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&doc.layers[0].pixels[(15 * 16 + 15) * 4..][..4], &[255, 255, 255, 255]);
    }
}
