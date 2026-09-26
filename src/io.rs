//! PNG import / export.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Seek};
use std::path::Path;

pub fn save_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(())
}

/// Load any PNG as 8-bit RGBA.
pub fn load_png(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    decode_png(BufReader::new(file))
}

/// Decode any PNG stream as 8-bit RGBA.
pub fn decode_png(r: impl BufRead + Seek) -> Result<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(r);
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().context("image too large")?];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width, info.height);
    let src = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => src.to_vec(),
        png::ColorType::Rgb => src.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Grayscale => src.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::GrayscaleAlpha => src.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        other => bail!("unsupported PNG colour type {other:?}"),
    };
    Ok((w, h, rgba))
}
