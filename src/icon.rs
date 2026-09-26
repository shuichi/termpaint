//! The application icon (`assets/icon.png`), embedded in the binary and
//! shown in the About dialog as a Kitty image.

use std::io::Cursor;

use crate::io::decode_png;

const PNG: &[u8] = include_bytes!("../assets/icon.png");

/// Edge length of the embedded (square) icon in pixels.
pub const SIZE: u32 = 512;

/// The icon area-averaged down to `side`×`side` pixels (`side` ≤ `SIZE`),
/// as straight-alpha RGBA for `Graphics::transmit_rgba`.
pub fn bitmap(side: u32) -> Vec<u8> {
    let (w, h, px) = decode_png(Cursor::new(PNG)).expect("embedded icon is a valid PNG");
    debug_assert_eq!((w, h), (SIZE, SIZE));
    downscale(&px, w, side.clamp(1, w))
}

/// For each output row/column: the source pixels it covers and their
/// weights (summing to 1).
fn spans(n: u32, side: u32) -> Vec<Vec<(usize, f32)>> {
    let scale = n as f32 / side as f32;
    (0..side)
        .map(|i| {
            let (a, b) = (i as f32 * scale, (i + 1) as f32 * scale);
            (a.floor() as u32..(b.ceil() as u32).min(n))
                .map(|s| (s as usize, (((s + 1) as f32).min(b) - (s as f32).max(a)) / scale))
                .filter(|&(_, w)| w > 0.0)
                .collect()
        })
        .collect()
}

/// Area-average an `n`×`n` RGBA image down to `side`×`side`. Colours are
/// weighted by alpha so transparent pixels don't darken the edges.
fn downscale(src: &[u8], n: u32, side: u32) -> Vec<u8> {
    let spans = spans(n, side);
    let n = n as usize;
    let mut out = vec![0u8; spans.len() * spans.len() * 4];
    for (oy, ys) in spans.iter().enumerate() {
        for (ox, xs) in spans.iter().enumerate() {
            let mut acc = [0f32; 4];
            for &(sy, wy) in ys {
                for &(sx, wx) in xs {
                    let p = &src[(sy * n + sx) * 4..][..4];
                    let w = wx * wy * p[3] as f32;
                    for c in 0..3 {
                        acc[c] += p[c] as f32 * w;
                    }
                    acc[3] += w;
                }
            }
            if acc[3] > 0.0 {
                let o = &mut out[(oy * spans.len() + ox) * 4..][..4];
                for c in 0..3 {
                    o[c] = (acc[c] / acc[3]).round() as u8;
                }
                o[3] = acc[3].round().min(255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_has_the_declared_size() {
        let (w, h, px) = decode_png(Cursor::new(PNG)).unwrap();
        assert_eq!((w, h), (SIZE, SIZE));
        assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn scaled_icon_keeps_transparent_corners() {
        for side in [37, 96, 252, SIZE] {
            let px = bitmap(side);
            assert_eq!(px.len(), (side * side * 4) as usize);
            let alpha = |x: u32, y: u32| px[((y * side + x) * 4 + 3) as usize];
            assert_eq!(alpha(0, 0), 0, "corner of {side}px icon");
            assert_eq!(alpha(side / 2, side / 2), 255, "centre of {side}px icon");
        }
    }

    #[test]
    fn downscale_weights_colour_by_alpha() {
        // One opaque red pixel among three transparent black ones.
        let src = [255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(downscale(&src, 2, 1), [255, 0, 0, 64]);
    }
}
