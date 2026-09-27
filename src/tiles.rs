//! The on-screen canvas split into a grid of small, cell-aligned Kitty images.
//!
//! Terminals treat an image as one unit: editing even a few pixels of a big
//! image makes the terminal re-process (compose, cache, upload to the GPU)
//! the *whole* bitmap. With tiles, a brush dab only re-sends the one or two
//! tiles it touches, so terminal work scales with the dirty area instead of
//! the canvas size.

use ratatui::layout::Rect;

use crate::document::PxRect;
use crate::kitty::Graphics;

pub struct Tiles {
    /// Area covered in cells; the bitmap is exactly this many cells big.
    pub area: Rect,
    pub cell: (u32, u32),
    /// Image id of the first tile; tile `i` uses `id_base + i`.
    id_base: u32,
    /// Tile size in cells.
    tc: u16,
    tr: u16,
    nx: u16,
    ny: u16,
    dirty: Vec<bool>,
}

impl Tiles {
    pub fn new(area: Rect, cell: (u32, u32), target_px: u32, id_base: u32) -> Self {
        let tc = ((target_px as f32 / cell.0 as f32).round() as u16).max(1);
        let tr = ((target_px as f32 / cell.1 as f32).round() as u16).max(1);
        let nx = area.width.div_ceil(tc);
        let ny = area.height.div_ceil(tr);
        Self { area, cell, id_base, tc, tr, nx, ny, dirty: vec![true; nx as usize * ny as usize] }
    }

    /// Bytes per row of the bitmap covering `area`.
    fn stride(&self) -> usize {
        self.area.width as usize * self.cell.0 as usize * 4
    }

    pub fn grid(&self) -> (u16, u16) {
        (self.nx, self.ny)
    }

    fn tile_w_px(&self) -> i32 {
        self.tc as i32 * self.cell.0 as i32
    }
    fn tile_h_px(&self) -> i32 {
        self.tr as i32 * self.cell.1 as i32
    }

    /// View-space pixel rectangle of tile (ix, iy).
    fn px_rect(&self, ix: u16, iy: u16) -> PxRect {
        let (cw, ch) = (self.cell.0 as i32, self.cell.1 as i32);
        let c0 = ix * self.tc;
        let r0 = iy * self.tr;
        let c1 = ((ix + 1) * self.tc).min(self.area.width);
        let r1 = ((iy + 1) * self.tr).min(self.area.height);
        PxRect::new(c0 as i32 * cw, r0 as i32 * ch, c1 as i32 * cw, r1 as i32 * ch)
    }

    /// Mark every tile overlapping a view-space rectangle.
    pub fn mark(&mut self, vr: PxRect) {
        if vr.is_empty() {
            return;
        }
        let (tw, th) = (self.tile_w_px(), self.tile_h_px());
        let ix0 = (vr.x0.max(0) / tw).min(self.nx as i32 - 1);
        let iy0 = (vr.y0.max(0) / th).min(self.ny as i32 - 1);
        let ix1 = ((vr.x1 - 1).max(0) / tw).min(self.nx as i32 - 1);
        let iy1 = ((vr.y1 - 1).max(0) / th).min(self.ny as i32 - 1);
        for iy in iy0..=iy1 {
            for ix in ix0..=ix1 {
                self.dirty[(iy * self.nx as i32 + ix) as usize] = true;
            }
        }
    }

    pub fn mark_all(&mut self) {
        self.dirty.fill(true);
    }

    /// Mark every tile whose pixels differ between two bitmaps of the area.
    pub fn mark_changed(&mut self, old: &[u8], new: &[u8]) {
        if old.len() != new.len() {
            return self.mark_all();
        }
        let stride = self.stride();
        for iy in 0..self.ny {
            for ix in 0..self.nx {
                let r = self.px_rect(ix, iy);
                let (a, b) = (r.x0 as usize * 4, r.x1 as usize * 4);
                let changed = (r.y0..r.y1).any(|y| {
                    let o = y as usize * stride;
                    old[o + a..o + b] != new[o + a..o + b]
                });
                if changed {
                    self.dirty[iy as usize * self.nx as usize + ix as usize] = true;
                }
            }
        }
    }

    /// Send every dirty tile of `buf` (a bitmap covering the area); returns
    /// the number of pixels sent.
    pub fn upload(&mut self, buf: &[u8], g: &mut Graphics, z: i32) -> usize {
        let stride = self.stride();
        let mut px = 0;
        for iy in 0..self.ny {
            for ix in 0..self.nx {
                let i = iy as usize * self.nx as usize + ix as usize;
                if !std::mem::take(&mut self.dirty[i]) {
                    continue;
                }
                let r = self.px_rect(ix, iy);
                let data = extract(buf, stride, r);
                g.transmit_and_place(
                    self.id_base + i as u32,
                    1,
                    self.area.x + ix * self.tc,
                    self.area.y + iy * self.tr,
                    r.width() as u32,
                    r.height() as u32,
                    &data,
                    z,
                );
                px += (r.width() * r.height()) as usize;
            }
        }
        px
    }

    pub fn delete_all(&self, g: &mut Graphics) {
        for i in 0..self.dirty.len() {
            g.delete_image(self.id_base + i as u32);
        }
    }
}

/// Copy rectangle `r` out of a bitmap with `stride` bytes per row.
fn extract(buf: &[u8], stride: usize, r: PxRect) -> Vec<u8> {
    let mut out = Vec::with_capacity((r.width() * r.height() * 4) as usize);
    for y in r.y0..r.y1 {
        let s = y as usize * stride + r.x0 as usize * 4;
        out.extend_from_slice(&buf[s..s + r.width() as usize * 4]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cover_area_exactly() {
        let t = Tiles::new(Rect::new(5, 2, 37, 19), (16, 32), 128, 1);
        assert_eq!(t.grid(), (5, 5));
        let mut area = 0;
        for iy in 0..t.ny {
            for ix in 0..t.nx {
                area += t.px_rect(ix, iy).width() * t.px_rect(ix, iy).height();
            }
        }
        assert_eq!(area, 37 * 16 * 19 * 32);
    }

    #[test]
    fn mark_hits_only_overlapping_tiles() {
        let mut t = Tiles::new(Rect::new(0, 0, 32, 16), (16, 32), 128, 1);
        t.dirty.fill(false);
        t.mark(PxRect::new(120, 100, 140, 110));
        let marked: Vec<usize> = t.dirty.iter().enumerate().filter(|(_, d)| **d).map(|(i, _)| i).collect();
        assert_eq!(marked, vec![0, 1]);
    }

    #[test]
    fn mark_changed_finds_only_differing_tiles() {
        // 32×16 cells of 16×32 px → 4×4 tiles of 128 px.
        let mut t = Tiles::new(Rect::new(0, 0, 32, 16), (16, 32), 128, 1);
        t.dirty.fill(false);
        let old = vec![0u8; 512 * 512 * 4];
        let mut new = old.clone();
        new[(300 * 512 + 200) * 4] = 1; // pixel (200, 300) → tile (1, 2)
        t.mark_changed(&old, &new);
        let marked: Vec<usize> = t.dirty.iter().enumerate().filter(|(_, d)| **d).map(|(i, _)| i).collect();
        assert_eq!(marked, vec![2 * 4 + 1]);
    }
}
