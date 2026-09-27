//! Layered RGBA document and compositing.

/// Integer pixel rectangle, half-open: `[x0, x1) x [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PxRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl PxRect {
    pub const EMPTY: PxRect = PxRect { x0: 0, y0: 0, x1: 0, y1: 0 };

    pub fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn width(&self) -> i32 {
        (self.x1 - self.x0).max(0)
    }
    pub fn height(&self) -> i32 {
        (self.y1 - self.y0).max(0)
    }
    pub fn union(&self, o: &PxRect) -> PxRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        PxRect::new(self.x0.min(o.x0), self.y0.min(o.y0), self.x1.max(o.x1), self.y1.max(o.y1))
    }
    pub fn intersect(&self, o: &PxRect) -> PxRect {
        let r = PxRect::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1));
        if r.is_empty() { PxRect::EMPTY } else { r }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const BLACK: Rgba = Rgba([0, 0, 0, 255]);
    pub const WHITE: Rgba = Rgba([255, 255, 255, 255]);
    pub fn rgb(&self) -> [u8; 3] {
        [self.0[0], self.0[1], self.0[2]]
    }
    pub fn hex(&self) -> String {
        let [r, g, b, a] = self.0;
        if a == 255 { format!("#{r:02X}{g:02X}{b:02X}") } else { format!("#{r:02X}{g:02X}{b:02X}{a:02X}") }
    }
}

#[derive(Clone)]
pub struct Layer {
    pub name: String,
    pub visible: bool,
    /// 0..=100 percent.
    pub opacity: u8,
    /// Straight (non-premultiplied) RGBA, row major.
    pub pixels: Vec<u8>,
}

impl Layer {
    pub fn new(name: impl Into<String>, w: u32, h: u32, fill: Option<Rgba>) -> Self {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        if let Some(c) = fill {
            for px in pixels.chunks_exact_mut(4) {
                px.copy_from_slice(&c.0);
            }
        }
        Self { name: name.into(), visible: true, opacity: 100, pixels }
    }
}

pub struct Document {
    pub width: u32,
    pub height: u32,
    /// Bottom-most layer first.
    pub layers: Vec<Layer>,
    pub active: usize,
    /// Flattened image over a transparency checkerboard (opaque RGBA),
    /// kept up to date incrementally for display.
    pub composite: Vec<u8>,
    layer_counter: u32,
}

/// "Source over" for straight alpha, `sa` in 0..=1.
#[inline]
pub fn blend_over(dst: &mut [u8], src: [u8; 3], sa: f32) {
    if sa <= 0.0 {
        return;
    }
    let da = dst[3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    let k = da * (1.0 - sa);
    for i in 0..3 {
        let c = (src[i] as f32 * sa + dst[i] as f32 * k) / oa;
        dst[i] = (c + 0.5) as u8;
    }
    dst[3] = (oa * 255.0 + 0.5) as u8;
}

#[inline]
pub fn checker(x: i32, y: i32) -> [u8; 3] {
    if ((x >> 3) + (y >> 3)) & 1 == 0 { [204, 204, 204] } else { [255, 255, 255] }
}

impl Document {
    pub fn new(width: u32, height: u32) -> Self {
        let mut d = Self {
            width,
            height,
            layers: vec![Layer::new("Background", width, height, Some(Rgba::WHITE))],
            active: 0,
            composite: vec![0; (width * height * 4) as usize],
            layer_counter: 1,
        };
        d.recomposite(d.bounds());
        d
    }

    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        let mut d = Self::new(width, height);
        d.layers[0].pixels = pixels;
        d.recomposite(d.bounds());
        d
    }

    /// Rebuild a saved document. `layers` is bottom-most first and non-empty.
    pub fn from_layers(width: u32, height: u32, layers: Vec<Layer>, active: usize, layer_counter: u32) -> Self {
        let mut d = Self {
            width,
            height,
            active: active.min(layers.len() - 1),
            layers,
            composite: vec![0; (width * height * 4) as usize],
            layer_counter,
        };
        d.recomposite(d.bounds());
        d
    }

    pub fn bounds(&self) -> PxRect {
        PxRect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// Number behind the most recent automatic "Layer N" name.
    pub fn layer_counter(&self) -> u32 {
        self.layer_counter
    }

    pub fn next_layer_name(&mut self) -> String {
        self.layer_counter += 1;
        format!("Layer {}", self.layer_counter)
    }

    pub fn blank_layer(&mut self) -> Layer {
        let name = self.next_layer_name();
        Layer::new(name, self.width, self.height, None)
    }

    /// Recompute the display composite for a region.
    pub fn recomposite(&mut self, r: PxRect) {
        let r = r.intersect(&self.bounds());
        if r.is_empty() {
            return;
        }
        let w = self.width as usize;
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                let i = (y as usize * w + x as usize) * 4;
                let mut px = {
                    let c = checker(x, y);
                    [c[0], c[1], c[2], 255]
                };
                for l in &self.layers {
                    if !l.visible || l.opacity == 0 {
                        continue;
                    }
                    let s = &l.pixels[i..i + 4];
                    if s[3] == 0 {
                        continue;
                    }
                    let sa = s[3] as f32 / 255.0 * l.opacity as f32 / 100.0;
                    blend_over(&mut px, [s[0], s[1], s[2]], sa);
                }
                self.composite[i..i + 4].copy_from_slice(&px);
            }
        }
    }

    /// Flatten visible layers with real transparency (for export / picking).
    pub fn flatten_pixel(&self, x: i32, y: i32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let mut px = [0u8; 4];
        for l in &self.layers {
            if !l.visible || l.opacity == 0 {
                continue;
            }
            let s = &l.pixels[i..i + 4];
            let sa = s[3] as f32 / 255.0 * l.opacity as f32 / 100.0;
            blend_over(&mut px, [s[0], s[1], s[2]], sa);
        }
        px
    }

    pub fn flatten(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.composite.len());
        for y in 0..self.height as i32 {
            for x in 0..self.width as i32 {
                out.extend_from_slice(&self.flatten_pixel(x, y));
            }
        }
        out
    }

    /// Copy a rectangle of a layer out into a tight buffer.
    pub fn read_rect(&self, layer: usize, r: PxRect) -> Vec<u8> {
        let w = self.width as usize;
        let mut out = Vec::with_capacity((r.width() * r.height() * 4) as usize);
        for y in r.y0..r.y1 {
            let s = (y as usize * w + r.x0 as usize) * 4;
            out.extend_from_slice(&self.layers[layer].pixels[s..s + r.width() as usize * 4]);
        }
        out
    }

    pub fn write_rect(&mut self, layer: usize, r: PxRect, data: &[u8]) {
        let w = self.width as usize;
        let rw = r.width() as usize * 4;
        for (row, y) in (r.y0..r.y1).enumerate() {
            let s = (y as usize * w + r.x0 as usize) * 4;
            self.layers[layer].pixels[s..s + rw].copy_from_slice(&data[row * rw..(row + 1) * rw]);
        }
    }

    /// Merge layer `idx` into the one below it.
    pub fn merge_down(&mut self, idx: usize) {
        if idx == 0 || idx >= self.layers.len() {
            return;
        }
        let upper = self.layers.remove(idx);
        let lower = &mut self.layers[idx - 1];
        if upper.visible {
            let op = upper.opacity as f32 / 100.0;
            for (d, s) in lower.pixels.chunks_exact_mut(4).zip(upper.pixels.chunks_exact(4)) {
                if s[3] > 0 {
                    blend_over(d, [s[0], s[1], s[2]], s[3] as f32 / 255.0 * op);
                }
            }
        }
        self.active = idx - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_opaque_replaces() {
        let mut d = [10, 20, 30, 255];
        blend_over(&mut d, [200, 100, 50], 1.0);
        assert_eq!(d, [200, 100, 50, 255]);
    }

    #[test]
    fn blend_onto_transparent_keeps_color() {
        let mut d = [0, 0, 0, 0];
        blend_over(&mut d, [200, 100, 50], 0.5);
        assert_eq!(&d[..3], &[200, 100, 50]);
        assert_eq!(d[3], 128);
    }

    #[test]
    fn rect_ops() {
        let a = PxRect::new(0, 0, 10, 10);
        let b = PxRect::new(5, 5, 20, 20);
        assert_eq!(a.intersect(&b), PxRect::new(5, 5, 10, 10));
        assert_eq!(a.union(&b), PxRect::new(0, 0, 20, 20));
        assert!(a.intersect(&PxRect::new(10, 10, 12, 12)).is_empty());
    }
}
