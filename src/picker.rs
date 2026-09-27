//! The colour picker panel: primary/secondary swatches, a hue ring around a
//! rotating saturation/value triangle, and an opacity slider.
//!
//! The panel interior is one bitmap streamed as Kitty image tiles placed
//! *below* the text layer, like the canvas, with its cells left at the
//! default background. Labels and values are ordinary ratatui text drawn
//! over it, and popups occlude it without any image bookkeeping. A state
//! change re-renders the bitmap, but only the tiles whose pixels changed
//! are re-sent.

use ratatui::layout::{Position, Rect};
use ratatui::style::Color;

use crate::app::hsv_to_rgb;
use crate::document::Rgba;
use crate::kitty::{Graphics, Z_BELOW_BG};
use crate::tiles::Tiles;
use crate::ui;

pub const TILE_BASE: u32 = 0x7471_0000;
/// Below cells with a non-default background, like the canvas (the two
/// never overlap).
pub const Z: i32 = Z_BELOW_BG - 2;

const fn rgb(c: Color) -> [u8; 3] {
    match c {
        Color::Rgb(r, g, b) => [r, g, b],
        _ => panic!("expected an RGB colour"),
    }
}

const PANEL: [u8; 3] = rgb(ui::PANEL);
const ACCENT: [u8; 3] = rgb(ui::ACCENT);
const FIELD: [u8; 3] = [19, 20, 26];
const FIELD_HOVER: [u8; 3] = [56, 61, 80];
const LINE: [u8; 3] = [98, 103, 122];
const ICON: [u8; 3] = [170, 175, 190];
const WHITE: [u8; 3] = [255, 255, 255];
const BLACK: [u8; 3] = [0, 0, 0];
const SLASH: [u8; 3] = [226, 52, 52];

/// Rows below the wheel: the "Opacity" label and the slider.
const FOOTER_ROWS: u16 = 2;
const MIN_COLS: u16 = 24;
const MIN_ROWS: u16 = 8;

/// Interactive element of the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Ring,
    Triangle,
    Primary,
    Secondary,
    Swap,
    Transparent,
    Black,
    White,
    Hue,
    Saturation,
    Lightness,
    Hex,
    Alpha,
    AlphaField,
    AlphaMenu,
}

impl Part {
    /// Button-like parts, which light up under the pointer.
    pub fn hoverable(self) -> bool {
        use Part::*;
        matches!(self, Secondary | Swap | Transparent | Black | White | Hex | AlphaField | AlphaMenu)
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Circle {
    pub x: f32,
    pub y: f32,
    pub r: f32,
}

impl Circle {
    fn sdf(&self, x: f32, y: f32) -> f32 {
        (x - self.x).hypot(y - self.y) - self.r
    }
    fn contains(&self, p: (f32, f32)) -> bool {
        self.sdf(p.0, p.1) <= 0.0
    }
    fn grow(&self, d: f32) -> Circle {
        Circle { r: self.r + d, ..*self }
    }
    fn bounds(&self) -> FRect {
        FRect::new(self.x - self.r, self.y - self.r, self.x + self.r, self.y + self.r)
    }
}

/// Pixel rectangle with fractional edges.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct FRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl FRect {
    fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }
    fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x0 && p.0 < self.x1 && p.1 >= self.y0 && p.1 < self.y1
    }
    fn grow(&self, dx: f32, dy: f32) -> FRect {
        FRect::new(self.x0 - dx, self.y0 - dy, self.x1 + dx, self.y1 + dy)
    }
    fn union(&self, o: &FRect) -> FRect {
        FRect::new(self.x0.min(o.x0), self.y0.min(o.y0), self.x1.max(o.x1), self.y1.max(o.y1))
    }
    fn w(&self) -> f32 {
        self.x1 - self.x0
    }
    fn h(&self) -> f32 {
        self.y1 - self.y0
    }
    /// Distance from `p` to the rectangle (0 inside).
    fn dist(&self, p: (f32, f32)) -> f32 {
        let dx = (self.x0 - p.0).max(p.0 - self.x1).max(0.0);
        let dy = (self.y0 - p.1).max(p.1 - self.y1).max(0.0);
        dx.hypot(dy)
    }
    /// Signed distance to the rectangle with corners rounded by `rad`.
    fn sdf(&self, x: f32, y: f32, rad: f32) -> f32 {
        let qx = (x - (self.x0 + self.x1) / 2.0).abs() - self.w() / 2.0 + rad;
        let qy = (y - (self.y0 + self.y1) / 2.0).abs() - self.h() / 2.0 + rad;
        qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad
    }
}

/// The saturation/value triangle for one hue: vertices are the pure hue,
/// white and black, and every point is their barycentric mix, which is
/// exactly `hsv_to_rgb(h, s, v)` with `v = w_hue + w_white` and
/// `s = w_hue / v`.
struct Triangle {
    v: [(f32, f32); 3],
}

impl Triangle {
    fn bounds(&self) -> FRect {
        let [a, b, c] = self.v;
        FRect::new(a.0.min(b.0).min(c.0), a.1.min(b.1).min(c.1), a.0.max(b.0).max(c.0), a.1.max(b.1).max(c.1))
    }

    /// Weights of (hue, white, black) at `p`; negative outside.
    fn bary(&self, p: (f32, f32)) -> [f32; 3] {
        let [a, b, c] = self.v;
        let (v0, v1, v2) = ((b.0 - a.0, b.1 - a.1), (c.0 - a.0, c.1 - a.1), (p.0 - a.0, p.1 - a.1));
        let d00 = v0.0 * v0.0 + v0.1 * v0.1;
        let d01 = v0.0 * v1.0 + v0.1 * v1.1;
        let d11 = v1.0 * v1.0 + v1.1 * v1.1;
        let d20 = v2.0 * v0.0 + v2.1 * v0.1;
        let d21 = v2.0 * v1.0 + v2.1 * v1.1;
        let den = d00 * d11 - d01 * d01;
        let wb = (d11 * d20 - d01 * d21) / den;
        let wc = (d00 * d21 - d01 * d20) / den;
        [1.0 - wb - wc, wb, wc]
    }

    /// Smallest distance from `p` to an edge line, positive inside.
    fn inside(&self, p: (f32, f32)) -> f32 {
        (0..3)
            .map(|i| {
                let (a, b, c) = (self.v[i], self.v[(i + 1) % 3], self.v[(i + 2) % 3]);
                let (nx, ny) = (a.1 - b.1, b.0 - a.0);
                let len = nx.hypot(ny);
                let d = ((p.0 - a.0) * nx + (p.1 - a.1) * ny) / len;
                let side = (c.0 - a.0) * nx + (c.1 - a.1) * ny;
                if side < 0.0 { -d } else { d }
            })
            .fold(f32::INFINITY, f32::min)
    }

    /// The point of the triangle nearest to `p`.
    fn clamp(&self, p: (f32, f32)) -> (f32, f32) {
        if self.inside(p) >= 0.0 {
            return p;
        }
        (0..3)
            .map(|i| {
                let (a, b) = (self.v[i], self.v[(i + 1) % 3]);
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                (a.0 + t * dx, a.1 + t * dy)
            })
            .min_by(|a, b| (a.0 - p.0).hypot(a.1 - p.1).total_cmp(&(b.0 - p.0).hypot(b.1 - p.1)))
            .unwrap()
    }
}

/// Where everything goes, in pixels relative to the panel interior unless
/// noted. Derived from the interior's cell rectangle and the cell size.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Layout {
    /// Panel interior in screen cells; the bitmap covers it exactly.
    pub area: Rect,
    pub cell: (u32, u32),
    /// Hue ring centre and radii, and the triangle's circumradius.
    pub center: (f32, f32),
    pub r_out: f32,
    pub r_in: f32,
    pub r_tri: f32,
    /// Radius of the hue and saturation/value handles.
    pub knob: f32,
    pub secondary: Circle,
    pub primary: Circle,
    pub transparent: Circle,
    /// The swap arrow is the top-right quarter of this circle.
    pub swap: Circle,
    pub black: FRect,
    pub white: FRect,
    pub preview: Circle,
    pub track: FRect,
    // Text controls, in screen cells.
    pub hsl: [Rect; 3],
    pub hex_label: Rect,
    pub hex: Rect,
    pub alpha_label: Rect,
    pub alpha: Rect,
    pub menu: Rect,
}

/// Interior rows the panel would like for an interior `cols` wide: a wheel
/// zone about as tall as the panel is wide, plus the opacity rows.
pub fn rows_for(cols: u16, cell: (u32, u32)) -> u16 {
    let w = cols as f32 * cell.0 as f32;
    (w * 0.82 / cell.1 as f32).ceil() as u16 + FOOTER_ROWS
}

impl Layout {
    /// `None` when the interior is too small for the picker.
    pub fn new(area: Rect, cell: (u32, u32)) -> Option<Self> {
        if area.width < MIN_COLS || area.height < MIN_ROWS {
            return None;
        }
        let (cw, u) = (cell.0 as f32, cell.1 as f32);
        let w = area.width as f32 * cw;
        let rows = area.height;
        // Bottom row of the wheel zone.
        let k = rows - FOOTER_ROWS - 1;
        let at = |col: u16, row: u16, width: u16| Rect::new(area.x + col, area.y + row, width, 1);
        let px = |r: Rect| {
            FRect::new(
                (r.x - area.x) as f32 * cw,
                (r.y - area.y) as f32 * u,
                (r.right() - area.x) as f32 * cw,
                (r.bottom() - area.y) as f32 * u,
            )
        };

        // Text controls: H/S/L bottom-left and hex bottom-right of the wheel
        // zone, then the opacity rows.
        let hsl = [at(1, k - 2, 6), at(1, k - 1, 6), at(1, k, 6)];
        let hex = at(area.width - 11, k, 10);
        let hex_label = at(area.width - 14, k, 2);
        let alpha_label = at(1, rows - 2, 7);
        let menu = at(area.width - 3, rows - 1, 2);
        let alpha = at(area.width - 10, rows - 1, 7);

        // Swatches in the top corners.
        let (mx, my) = (cw, 0.25 * u);
        let rs = 0.55 * u;
        let secondary = Circle { x: mx + rs, y: my + rs, r: rs };
        let primary = Circle { x: secondary.x + rs, y: secondary.y + rs, r: 0.62 * u };
        let rt = 0.24 * u;
        let transparent = Circle { x: mx + rt, y: primary.y + primary.r - rt, r: rt };
        let swap = Circle { x: secondary.x + rs + 0.1 * u, y: secondary.y, r: 0.47 * u };
        let q = 0.7 * u;
        let white = FRect::new(w - mx - q, my + 0.1 * u, w - mx, my + 0.1 * u + q);
        let black = FRect { x0: white.x0 - q, x1: white.x0, ..white };

        // The biggest hue ring that is centred horizontally and clear of
        // the corner widgets: slide the centre down the zone and keep the
        // position that allows the largest radius.
        let zone_h = (k + 1) as f32 * u;
        let pad = 0.2 * u;
        let blocks = [
            FRect::new(0.0, 0.0, primary.x + primary.r, primary.y + primary.r).union(&swap_bounds(&swap, u)),
            black.union(&white),
            px(hsl[0]).union(&px(hsl[2])),
            px(hex_label).union(&px(hex)),
        ];
        let cx = w / 2.0;
        let mut best = (0.0f32, 0.0f32);
        for y in 0..=zone_h as i32 {
            let cy = y as f32;
            let fit = (cx - 0.5 * cw).min(cy - 0.1 * u).min(zone_h - cy - 0.1 * u);
            let r = blocks.iter().fold(fit, |r, b| r.min(b.dist((cx, cy)) - pad));
            if r > best.0 {
                best = (r, cy);
            }
        }
        let r_out = best.0.floor();
        if r_out < 1.5 * u {
            return None;
        }
        let t = (r_out * 0.14).max(4.0);
        let r_in = r_out - t;

        // Opacity slider row.
        let yc = (rows - 1) as f32 * u + u / 2.0;
        let preview = Circle { x: 2.0 * cw, y: yc, r: (0.34 * u).min(0.95 * cw) };
        let th = 0.36 * u;
        let track = FRect::new(4.0 * cw, yc - th / 2.0, (alpha.x - area.x - 1) as f32 * cw, yc + th / 2.0);

        Some(Self {
            area,
            cell,
            center: (cx, best.1),
            r_out,
            r_in,
            r_tri: r_in - (0.05 * u).max(1.5),
            knob: (t * 0.58).max(0.2 * u),
            secondary,
            primary,
            transparent,
            swap,
            black,
            white,
            preview,
            track,
            hsl,
            hex_label,
            hex,
            alpha_label,
            alpha,
            menu,
        })
    }

    /// Bitmap size in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.area.width as u32 * self.cell.0, self.area.height as u32 * self.cell.1)
    }

    /// Screen pixel → panel pixel centre.
    fn local(&self, m: (i32, i32)) -> (f32, f32) {
        (
            (m.0 - self.area.x as i32 * self.cell.0 as i32) as f32 + 0.5,
            (m.1 - self.area.y as i32 * self.cell.1 as i32) as f32 + 0.5,
        )
    }

    /// Screen cell rectangle → panel pixels.
    fn cells(&self, r: Rect) -> FRect {
        let (cw, ch) = (self.cell.0 as f32, self.cell.1 as f32);
        FRect::new(
            (r.x - self.area.x) as f32 * cw,
            (r.y - self.area.y) as f32 * ch,
            (r.right() - self.area.x) as f32 * cw,
            (r.bottom() - self.area.y) as f32 * ch,
        )
    }

    /// Element under screen pixel `m`, topmost first.
    pub fn part_at(&self, m: (i32, i32)) -> Option<Part> {
        let cell = Position::new((m.0 / self.cell.0 as i32) as u16, (m.1 / self.cell.1 as i32) as u16);
        if !self.area.contains(cell) {
            return None;
        }
        let texts = [
            (self.hex, Part::Hex),
            (self.hex_label, Part::Hex),
            (self.alpha, Part::AlphaField),
            (self.menu, Part::AlphaMenu),
            (self.hsl[0], Part::Hue),
            (self.hsl[1], Part::Saturation),
            (self.hsl[2], Part::Lightness),
        ];
        if let Some((_, part)) = texts.iter().find(|(r, _)| r.contains(cell)) {
            return Some(*part);
        }
        let p = self.local(m);
        let shapes = [
            (self.transparent.contains(p), Part::Transparent),
            (self.primary.contains(p), Part::Primary),
            (self.secondary.contains(p), Part::Secondary),
            (swap_bounds(&self.swap, self.cell.1 as f32).contains(p), Part::Swap),
            (self.black.contains(p), Part::Black),
            (self.white.contains(p), Part::White),
            (self.track.grow(self.alpha_knob(), self.cell.1 as f32).contains(p), Part::Alpha),
        ];
        if let Some((_, part)) = shapes.iter().find(|(hit, _)| *hit) {
            return Some(*part);
        }
        let d = (p.0 - self.center.0).hypot(p.1 - self.center.1);
        if d <= self.r_out + 2.0 && d >= (self.r_in + self.r_tri) / 2.0 {
            Some(Part::Ring)
        } else if d < self.r_in {
            // Anywhere inside the ring picks from the (clamped) triangle.
            Some(Part::Triangle)
        } else {
            None
        }
    }

    /// Hue (degrees) at a screen pixel: red at 3 o'clock, increasing clockwise.
    pub fn hue_at(&self, m: (i32, i32)) -> f32 {
        let p = self.local(m);
        (p.1 - self.center.1).atan2(p.0 - self.center.0).to_degrees().rem_euclid(360.0)
    }

    /// Saturation and value at a screen pixel, clamped to the triangle.
    /// Saturation is `None` on the black vertex, where it is undefined.
    pub fn sv_at(&self, m: (i32, i32), hue: f32) -> (Option<f32>, f32) {
        let tri = self.triangle(hue);
        let [wh, ww, _] = tri.bary(tri.clamp(self.local(m))).map(|w| w.clamp(0.0, 1.0));
        let v = (wh + ww).min(1.0);
        ((v > 1e-3).then(|| (wh / (wh + ww)).clamp(0.0, 1.0)), v)
    }

    /// Opacity (0..=255) at a screen pixel on the slider.
    pub fn alpha_at(&self, m: (i32, i32)) -> u8 {
        let k = self.alpha_knob();
        let t = (self.local(m).0 - self.track.x0 - k) / (self.track.w() - 2.0 * k);
        (t.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    fn triangle(&self, hue: f32) -> Triangle {
        let (cx, cy) = self.center;
        let v = [0.0f32, 120.0, 240.0].map(|o| {
            let a = (hue + o).to_radians();
            (cx + self.r_tri * a.cos(), cy + self.r_tri * a.sin())
        });
        Triangle { v }
    }

    fn sv_point(&self, (h, s, v): (f32, f32, f32)) -> (f32, f32) {
        let [ph, pw, pb] = self.triangle(h).v;
        let w = [s * v, v - s * v, 1.0 - v];
        (w[0] * ph.0 + w[1] * pw.0 + w[2] * pb.0, w[0] * ph.1 + w[1] * pw.1 + w[2] * pb.1)
    }

    fn hue_point(&self, h: f32) -> (f32, f32) {
        let r = (self.r_out + self.r_in) / 2.0;
        let a = h.to_radians();
        (self.center.0 + r * a.cos(), self.center.1 + r * a.sin())
    }

    fn alpha_knob(&self) -> f32 {
        self.track.h() * 0.85
    }
}

/// Hit box of the swap arrow (top-right quarter of `c`, with heads).
fn swap_bounds(c: &Circle, u: f32) -> FRect {
    FRect::new(c.x - 0.15 * u, c.y - c.r - 0.15 * u, c.x + c.r + 0.15 * u, c.y + 0.15 * u)
}

/// What the panel shows besides its layout.
#[derive(Clone, PartialEq, Debug)]
pub struct State {
    pub hsv: (f32, f32, f32),
    pub primary: Rgba,
    pub secondary: Rgba,
    /// Button under the pointer (only `Part::hoverable` ones).
    pub hover: Option<Part>,
    /// The hex field is being edited.
    pub editing: bool,
    /// The opacity preset menu is open.
    pub menu: bool,
}

/// Anti-aliased coverage of a pixel whose centre is `d` px outside a shape.
fn cover(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

fn checker(x: f32, y: f32, size: f32) -> [u8; 3] {
    if ((x / size) as i32 + (y / size) as i32) & 1 == 0 { [204, 204, 204] } else { [255, 255, 255] }
}

/// Distance from `p` to segment `a`–`b`.
fn segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

/// Opaque RGBA bitmap that shapes are blended onto.
struct Bitmap<'a> {
    w: usize,
    h: usize,
    px: &'a mut [u8],
}

impl Bitmap<'_> {
    /// Blend `f(x, y) = (colour, coverage)` over the pixels in `b`.
    fn paint(&mut self, b: FRect, f: impl Fn(f32, f32) -> Option<([u8; 3], f32)>) {
        let clip = |v: f32, n: usize| (v.max(0.0) as usize).min(n);
        let (x0, x1) = (clip(b.x0.floor(), self.w), clip(b.x1.ceil(), self.w));
        let (y0, y1) = (clip(b.y0.floor(), self.h), clip(b.y1.ceil(), self.h));
        for y in y0..y1 {
            for x in x0..x1 {
                if let Some((c, a)) = f(x as f32 + 0.5, y as f32 + 0.5)
                    && a > 0.0
                {
                    let i = (y * self.w + x) * 4;
                    let dst = [self.px[i], self.px[i + 1], self.px[i + 2]];
                    self.px[i..i + 3].copy_from_slice(&mix(dst, c, a.min(1.0)));
                }
            }
        }
    }

    /// Fill the shape described by a signed distance function.
    fn fill(&mut self, b: FRect, c: [u8; 3], sdf: impl Fn(f32, f32) -> f32) {
        self.paint(b.grow(1.0, 1.0), |x, y| Some((c, cover(sdf(x, y)))));
    }

    fn disc(&mut self, c: Circle, col: [u8; 3]) {
        self.fill(c.bounds(), col, |x, y| c.sdf(x, y));
    }

    /// A colour swatch: the colour over a checkerboard, with a thin rim.
    fn swatch(&mut self, c: Circle, col: Rgba, rim: [u8; 3], rim_w: f32, check: f32) {
        let [r, g, b, a] = col.0;
        let a = a as f32 / 255.0;
        self.paint(c.bounds().grow(1.0, 1.0), |x, y| {
            let base = if a < 1.0 { checker(x, y, check) } else { [0; 3] };
            Some((mix(base, [r, g, b], a), cover(c.sdf(x, y))))
        });
        self.ring(c, rim_w, rim);
    }

    /// A circle outline of width `w` just inside `c`.
    fn ring(&mut self, c: Circle, w: f32, col: [u8; 3]) {
        let mid = c.r - w / 2.0;
        self.fill(c.bounds(), col, |x, y| ((x - c.x).hypot(y - c.y) - mid).abs() - w / 2.0);
    }

    /// Slider/wheel handle: dark outline, white ring, filled centre.
    fn knob(&mut self, p: (f32, f32), r: f32, w: f32, fill: [u8; 3]) {
        let c = Circle { x: p.0, y: p.1, r };
        self.paint(c.grow(1.5).bounds(), |x, y| Some((BLACK, 0.55 * cover(c.grow(1.0).sdf(x, y)))));
        self.disc(c, WHITE);
        self.disc(c.grow(-w), fill);
    }

    fn rounded(&mut self, r: FRect, rad: f32, col: [u8; 3]) {
        self.fill(r, col, |x, y| r.sdf(x, y, rad));
    }

    fn outline(&mut self, r: FRect, rad: f32, w: f32, col: [u8; 3]) {
        self.fill(r, col, |x, y| (r.sdf(x, y, rad) + w / 2.0).abs() - w / 2.0);
    }

    fn line(&mut self, a: (f32, f32), b: (f32, f32), w: f32, col: [u8; 3]) {
        let bounds = FRect::new(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)).grow(w, w);
        self.fill(bounds, col, |x, y| segment((x, y), a, b) - w / 2.0);
    }

    fn triangle(&mut self, v: [(f32, f32); 3], col: [u8; 3]) {
        let t = Triangle { v };
        self.fill(t.bounds(), col, |x, y| -t.inside((x, y)));
    }
}

/// Background and hue ring: everything that depends only on the layout.
fn render_base(l: &Layout) -> Vec<u8> {
    let (w, h) = l.size();
    let mut px: Vec<u8> = [PANEL[0], PANEL[1], PANEL[2], 255].repeat((w * h) as usize);
    let mut bm = Bitmap { w: w as usize, h: h as usize, px: &mut px };
    let (cx, cy) = l.center;
    let (mid, half) = ((l.r_out + l.r_in) / 2.0, (l.r_out - l.r_in) / 2.0);
    let ring = Circle { x: cx, y: cy, r: l.r_out };
    bm.paint(ring.bounds().grow(1.0, 1.0), |x, y| {
        let (dx, dy) = (x - cx, y - cy);
        let c = cover((dx.hypot(dy) - mid).abs() - half);
        (c > 0.0).then(|| (hsv_to_rgb((dy.atan2(dx).to_degrees(), 1.0, 1.0)).rgb(), c))
    });
    px
}

/// Draw the state-dependent parts over a copy of the base.
fn render(l: &Layout, st: &State, px: &mut [u8]) {
    let (w, h) = l.size();
    let mut bm = Bitmap { w: w as usize, h: h as usize, px };
    let u = l.cell.1 as f32;
    let thin = (0.035 * u).max(1.0);
    let check = (0.16 * u).max(3.0);
    let hover = |p: Part| st.hover == Some(p);

    // Saturation/value triangle for the current hue.
    let (hue, s, v) = st.hsv;
    let tri = l.triangle(hue);
    let pure = hsv_to_rgb((hue, 1.0, 1.0)).rgb();
    bm.paint(tri.bounds().grow(1.0, 1.0), |x, y| {
        let c = cover(-tri.inside((x, y)));
        (c > 0.0).then(|| {
            let [wh, ww, _] = tri.bary(tri.clamp((x, y))).map(|w| w.clamp(0.0, 1.0));
            ([0, 1, 2].map(|i| (wh * pure[i] as f32 + ww * 255.0).round().min(255.0) as u8), c)
        })
    });
    let kw = (0.07 * u).max(1.5);
    bm.knob(l.hue_point(hue), l.knob, kw, pure);
    bm.knob(l.sv_point((hue, s, v)), l.knob, kw, st.primary.rgb());

    // Secondary behind primary, separated by a gap in the panel colour.
    let rim = |p: Part| if hover(p) { ACCENT } else { LINE };
    bm.swatch(l.secondary, st.secondary, rim(Part::Secondary), thin, check);
    bm.disc(l.primary.grow(0.07 * u), PANEL);
    bm.swatch(l.primary, st.primary, LINE, thin, check);

    // "None": a white disc struck through in red.
    let t = l.transparent;
    bm.disc(t, WHITE);
    let d = t.r * 0.62;
    bm.line((t.x - d, t.y + d), (t.x + d, t.y - d), (t.r * 0.28).max(1.5), SLASH);
    bm.ring(t, thin, if hover(Part::Transparent) { ACCENT } else { LINE });

    // Swap: a quarter arc from the top round to the right, heads at both ends.
    let sw = l.swap;
    let (aw, head) = ((0.06 * u).max(1.2), 0.2 * u);
    let col = if hover(Part::Swap) { WHITE } else { ICON };
    bm.fill(sw.grow(aw).bounds(), col, |x, y| {
        let (dx, dy) = (x - sw.x, y - sw.y);
        if dx >= 0.0 && dy <= 0.0 {
            (dx.hypot(dy) - sw.r).abs() - aw / 2.0
        } else {
            (x - sw.x).hypot(y - sw.y + sw.r).min((x - sw.x - sw.r).hypot(y - sw.y)) - aw / 2.0
        }
    });
    let (top, right) = ((sw.x, sw.y - sw.r), (sw.x + sw.r, sw.y));
    bm.triangle([(top.0 - head, top.1), (top.0 + head * 0.3, top.1 - head * 0.7), (top.0 + head * 0.3, top.1 + head * 0.7)], col);
    bm.triangle(
        [(right.0, right.1 + head), (right.0 - head * 0.7, right.1 - head * 0.3), (right.0 + head * 0.7, right.1 - head * 0.3)],
        col,
    );

    // Black and white quick swatches.
    let rad = 0.08 * u;
    for (r, c, p) in [(l.black, BLACK, Part::Black), (l.white, WHITE, Part::White)] {
        bm.rounded(r, rad, c);
        let (w, rim) = if hover(p) { (2.0 * thin, ACCENT) } else { (thin, LINE) };
        bm.outline(r, rad, w, rim);
    }

    // Fields behind the hex and opacity text, and the menu button.
    let field = |bm: &mut Bitmap, r: Rect, p: Part, on: bool| {
        let f = l.cells(r).grow(-0.15 * l.cell.0 as f32, -0.08 * u);
        bm.rounded(f, 0.18 * u, if hover(p) { FIELD_HOVER } else { FIELD });
        if on {
            bm.outline(f, 0.18 * u, 1.5 * thin, ACCENT);
        }
        f
    };
    field(&mut bm, l.hex, Part::Hex, st.editing);
    field(&mut bm, l.alpha, Part::AlphaField, false);
    let m = field(&mut bm, l.menu, Part::AlphaMenu, st.menu);
    let (mcx, mcy, cs) = ((m.x0 + m.x1) / 2.0, (m.y0 + m.y1) / 2.0, (0.13 * u).min(m.w() * 0.3));
    let cw = (0.05 * u).max(1.2);
    bm.line((mcx - cs, mcy - cs * 0.5), (mcx, mcy + cs * 0.5), cw, ICON);
    bm.line((mcx, mcy + cs * 0.5), (mcx + cs, mcy - cs * 0.5), cw, ICON);

    // Opacity: preview, then a transparent → colour track with a handle.
    bm.swatch(l.preview, st.primary, LINE, thin, check * 0.75);
    let tr = l.track;
    let trad = tr.h() / 2.0;
    let c = st.primary.rgb();
    bm.paint(tr.grow(1.0, 1.0), |x, y| {
        let t = ((x - tr.x0) / tr.w()).clamp(0.0, 1.0);
        Some((mix(checker(x - tr.x0, y - tr.y0, tr.h() / 2.0), c, t), cover(tr.sdf(x, y, trad))))
    });
    bm.outline(tr, trad, thin, FIELD);
    let k = l.alpha_knob();
    let kx = tr.x0 + k + st.primary.0[3] as f32 / 255.0 * (tr.w() - 2.0 * k);
    bm.knob((kx, (tr.y0 + tr.y1) / 2.0), k, kw, FIELD);
}

/// The picker's tiles on the terminal and what they currently show.
pub struct Gfx {
    pub layout: Layout,
    tiles: Tiles,
    base: Vec<u8>,
    shown: Vec<u8>,
    next: Vec<u8>,
    state: Option<State>,
}

impl Gfx {
    pub fn new(layout: Layout, tile_px: u32) -> Self {
        Self {
            tiles: Tiles::new(layout.area, layout.cell, tile_px, TILE_BASE),
            base: render_base(&layout),
            layout,
            shown: Vec::new(),
            next: Vec::new(),
            state: None,
        }
    }

    /// Re-render for `st` and re-send the tiles that changed; returns the
    /// number of pixels sent.
    pub fn sync(&mut self, st: &State, g: &mut Graphics) -> usize {
        if self.state.as_ref() == Some(st) {
            return 0;
        }
        self.next.clear();
        self.next.extend_from_slice(&self.base);
        render(&self.layout, st, &mut self.next);
        self.tiles.mark_changed(&self.shown, &self.next);
        let px = self.tiles.upload(&self.next, g, Z);
        std::mem::swap(&mut self.shown, &mut self.next);
        self.state = Some(st.clone());
        px
    }

    pub fn delete(&self, g: &mut Graphics) {
        self.tiles.delete_all(g);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (u32, u32) = (19, 42);

    fn layout() -> Layout {
        Layout::new(Rect::new(101, 3, 34, rows_for(34, CELL)), CELL).expect("fits")
    }

    fn state() -> State {
        State {
            hsv: (0.0, 0.0, 0.92),
            primary: Rgba([235, 235, 235, 255]),
            secondary: Rgba::BLACK,
            hover: None,
            editing: false,
            menu: false,
        }
    }

    /// Screen pixel of a panel-local point.
    fn screen(l: &Layout, p: (f32, f32)) -> (i32, i32) {
        ((l.area.x as u32 * l.cell.0) as i32 + p.0 as i32, (l.area.y as u32 * l.cell.1) as i32 + p.1 as i32)
    }

    #[test]
    fn wheel_is_inside_the_panel_and_clear_of_the_corners() {
        for cell in [CELL, (9, 18), (8, 16), (10, 22)] {
            for (cols, extra) in [(34, 0), (24, 0), (34, 6), (48, 0)] {
                let rows = rows_for(cols, cell) + extra;
                let l = Layout::new(Rect::new(0, 0, cols, rows), cell).unwrap();
                let (w, h) = l.size();
                let (cx, cy) = l.center;
                assert!(cx - l.r_out >= 0.0 && cx + l.r_out <= w as f32, "{cell:?} {cols}");
                assert!(cy - l.r_out >= 0.0 && cy + l.r_out <= (h - 2 * cell.1) as f32, "{cell:?} {cols}");
                assert!(l.r_out > 0.28 * w as f32, "wheel too small for {cell:?} {cols}×{rows}: {}", l.r_out);
                for r in [l.hsl[0], l.hsl[2], l.hex, l.hex_label] {
                    assert!(l.cells(r).dist(l.center) > l.r_out, "{r:?} overlaps the ring");
                }
                assert!(l.track.w() > 4.0 * cell.0 as f32);
            }
        }
    }

    #[test]
    fn too_small_panels_have_no_picker() {
        assert!(Layout::new(Rect::new(0, 0, 34, MIN_ROWS - 1), CELL).is_none());
        assert!(Layout::new(Rect::new(0, 0, MIN_COLS - 1, 20), CELL).is_none());
    }

    #[test]
    fn saturation_value_round_trips_through_the_triangle() {
        let l = layout();
        for hsv in [(0.0, 1.0, 1.0), (75.0, 0.5, 0.5), (200.0, 0.2, 0.9), (310.0, 0.9, 0.3)] {
            let (s, v) = l.sv_at(screen(&l, l.sv_point(hsv)), hsv.0);
            assert!((s.unwrap() - hsv.1).abs() < 0.02 && (v - hsv.2).abs() < 0.02, "{hsv:?} → {s:?} {v}");
        }
        // Far outside the triangle: clamped onto its nearest edge.
        let (s, v) = l.sv_at(screen(&l, (l.center.0 + 10.0 * l.r_out, l.center.1)), 0.0);
        assert_eq!((s.map(|s| s.round()), v.round()), (Some(1.0), 1.0));
        assert_eq!(l.sv_at(screen(&l, l.sv_point((0.0, 0.5, 0.0))), 0.0).0, None, "black has no saturation");
    }

    #[test]
    fn hue_follows_the_pointer_angle() {
        let l = layout();
        for h in [0.0, 60.0, 135.0, 270.0, 359.0] {
            let got = l.hue_at(screen(&l, l.hue_point(h)));
            assert!((got - h).abs() < 1.0 || (got - h).abs() > 359.0, "{h} → {got}");
        }
    }

    #[test]
    fn parts_are_found_where_they_are_drawn() {
        let l = layout();
        let at = |p: (f32, f32)| l.part_at(screen(&l, p));
        let (cx, cy) = l.center;
        assert_eq!(at((cx, cy)), Some(Part::Triangle));
        assert_eq!(at(l.hue_point(90.0)), Some(Part::Ring));
        assert_eq!(at((l.primary.x, l.primary.y)), Some(Part::Primary));
        assert_eq!(at((l.secondary.x - 0.5 * l.secondary.r, l.secondary.y)), Some(Part::Secondary));
        assert_eq!(at((l.transparent.x, l.transparent.y)), Some(Part::Transparent));
        assert_eq!(at((l.swap.x + 0.7 * l.swap.r, l.swap.y - 0.7 * l.swap.r)), Some(Part::Swap));
        assert_eq!(at((l.black.x0 + 2.0, l.black.y0 + 2.0)), Some(Part::Black));
        assert_eq!(at((l.white.x1 - 2.0, l.white.y1 - 2.0)), Some(Part::White));
        assert_eq!(at(((l.track.x0 + l.track.x1) / 2.0, l.track.y0)), Some(Part::Alpha));
        let cell = |r: Rect| l.part_at((r.x as i32 * 19 + 5, r.y as i32 * 42 + 5));
        assert_eq!(cell(l.hex), Some(Part::Hex));
        assert_eq!(cell(l.hex_label), Some(Part::Hex));
        assert_eq!(cell(l.alpha), Some(Part::AlphaField));
        assert_eq!(cell(l.menu), Some(Part::AlphaMenu));
        assert_eq!(cell(l.hsl[1]), Some(Part::Saturation));
        assert_eq!(l.part_at((0, 0)), None, "outside the panel");
    }

    fn pixel(buf: &[u8], l: &Layout, p: (f32, f32)) -> [u8; 3] {
        let i = (p.1 as usize * l.size().0 as usize + p.0 as usize) * 4;
        [buf[i], buf[i + 1], buf[i + 2]]
    }

    #[test]
    fn ring_and_triangle_show_the_colours_they_pick() {
        let l = layout();
        let base = render_base(&l);
        let near = |a: [u8; 3], b: [u8; 3]| (0..3).all(|i| a[i].abs_diff(b[i]) <= 3);
        assert!(near(pixel(&base, &l, l.hue_point(0.0)), [255, 0, 0]));
        assert!(near(pixel(&base, &l, l.hue_point(120.0)), [0, 255, 0]));
        assert_eq!(pixel(&base, &l, (1.0, l.center.1)), PANEL);

        let mut px = base.clone();
        let st = State { hsv: (120.0, 1.0, 1.0), ..state() };
        render(&l, &st, &mut px);
        // Inside the triangle, away from the knob on the hue vertex.
        let [_, pw, pb] = l.triangle(120.0).v;
        let (cx, cy) = l.center;
        let p = (0.4 * (pw.0 + pb.0) + 0.2 * cx, 0.4 * (pw.1 + pb.1) + 0.2 * cy);
        let (s, v) = l.sv_at(screen(&l, p), 120.0);
        let want = hsv_to_rgb((120.0, s.unwrap_or(0.0), v)).rgb();
        let got = pixel(&px, &l, p);
        assert!(near(got, want), "{got:?} vs {want:?}");
    }

    #[test]
    fn only_changed_tiles_are_resent() {
        let l = layout();
        let mut gfx = Gfx::new(l, 64);
        let mut g = Graphics::new(false);
        let (w, h) = l.size();
        assert_eq!(gfx.sync(&state(), &mut g), (w * h) as usize, "first frame sends everything");
        assert_eq!(gfx.sync(&state(), &mut g), 0, "same state → nothing");
        let hovered = State { hover: Some(Part::Black), ..state() };
        let px = gfx.sync(&hovered, &mut g);
        assert!(px > 0 && px < (w * h) as usize / 10, "hover re-sends a few tiles: {px}");
        let mut out = Vec::new();
        g.flush_to(&mut out).unwrap();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains(&format!("i={TILE_BASE},")) && s.contains(&format!("z={Z},C=1")));
    }

    /// Render the panel to `target/picker.png` for a look:
    /// `cargo test show_picker -- --ignored`
    #[test]
    #[ignore]
    fn show_picker() {
        let l = layout();
        let mut px = render_base(&l);
        let st = State { hsv: (18.0, 0.8, 0.85), primary: hsv_to_rgb((18.0, 0.8, 0.85)), hover: Some(Part::Hex), ..state() };
        render(&l, &st, &mut px);
        let (w, h) = l.size();
        let f = std::fs::File::create(concat!(env!("CARGO_MANIFEST_DIR"), "/target/picker.png")).unwrap();
        let mut enc = png::Encoder::new(f, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
    }
}
