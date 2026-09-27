//! PNG import / export.
//!
//! The flattened image is stored as ordinary PNG image data, so any viewer can
//! open the file. The editable document (layers with their names, visibility
//! and opacity, plus the active layer) rides along in private `tpLR` chunks
//! after the image data, and is restored when termpaint opens the file again.
//!
//! `tpLR` payload (all integers big-endian), split across as many consecutive
//! chunks as needed:
//!
//! ```text
//! u8   format version (1)
//! u32  width, u32 height        must match IHDR
//! u32  CRC-32 of the flattened RGBA image the chunk was saved with
//! u32  active layer index
//! u32  layer name counter
//! u32  layer count, then per layer (bottom-most first):
//!      u8  visible (0 / 1)
//!      u8  opacity (0..=100)
//!      u16 name length, UTF-8 name
//!      u32 PNG length, the layer's straight RGBA pixels as an RGBA8 PNG
//! ```
//!
//! The chunk is marked unsafe-to-copy, so other editors are expected to drop
//! it when they change the image. If one keeps it anyway, the CRC no longer
//! matches the image and the layers are ignored in favour of what was edited.

use anyhow::{Context, Result, bail, ensure};
use flate2::Crc;
use std::fs::File;
use std::io::{BufRead, BufWriter, Cursor, Seek, Write};
use std::path::Path;

use crate::document::{Document, Layer};

/// Ancillary, private, unsafe-to-copy chunk holding the layered document.
const LAYERS_CHUNK: [u8; 4] = *b"tpLR";
const LAYERS_VERSION: u8 = 1;
/// Layer data is split into chunks of at most this many bytes.
const LAYERS_CHUNK_MAX: usize = 1 << 20;

/// Save the flattened image plus the layered document.
pub fn save_document(path: &Path, doc: &Document) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    encode_document(BufWriter::new(file), doc)
}

/// Open a PNG. Layers saved by termpaint are restored; any other PNG becomes
/// a single-layer document. The second value explains why layer data that
/// was present had to be ignored.
pub fn load_document(path: &Path) -> Result<(Document, Option<String>)> {
    let bytes = std::fs::read(path).with_context(|| format!("open {}", path.display()))?;
    decode_document(&bytes)
}

fn encode_document(w: impl Write, doc: &Document) -> Result<()> {
    let flat = doc.flatten();
    let layers = encode_layers(doc, &flat)?;
    let mut writer = png_writer(w, doc.width, doc.height)?;
    writer.write_image_data(&flat)?;
    for part in layers.chunks(LAYERS_CHUNK_MAX) {
        writer.write_chunk(png::chunk::ChunkType(LAYERS_CHUNK), part)?;
    }
    writer.finish()?;
    Ok(())
}

fn decode_document(bytes: &[u8]) -> Result<(Document, Option<String>)> {
    let (w, h, flat) = decode_png(Cursor::new(bytes))?;
    let restored = layers_payload(bytes).and_then(|p| p.map(|p| decode_layers(&p, w, h, &flat)).transpose());
    Ok(match restored {
        Ok(Some(doc)) => (doc, None),
        Ok(None) => (Document::from_rgba(w, h, flat), None),
        Err(e) => (Document::from_rgba(w, h, flat), Some(format!("Layer data ignored: {e:#}"))),
    })
}

fn encode_layers(doc: &Document, flat: &[u8]) -> Result<Vec<u8>> {
    let mut out = vec![LAYERS_VERSION];
    for v in [doc.width, doc.height, crc32(flat), doc.active as u32, doc.layer_counter(), doc.layers.len() as u32] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    for l in &doc.layers {
        out.extend_from_slice(&[l.visible as u8, l.opacity]);
        let name_len = u16::try_from(l.name.len()).context("layer name too long")?;
        out.extend_from_slice(&name_len.to_be_bytes());
        out.extend_from_slice(l.name.as_bytes());
        let mut png = Vec::new();
        let mut writer = png_writer(&mut png, doc.width, doc.height)?;
        writer.write_image_data(&l.pixels)?;
        writer.finish()?;
        let png_len = u32::try_from(png.len()).context("layer too large")?;
        out.extend_from_slice(&png_len.to_be_bytes());
        out.extend_from_slice(&png);
    }
    Ok(out)
}

fn decode_layers(payload: &[u8], w: u32, h: u32, flat: &[u8]) -> Result<Document> {
    let mut r = Bytes(payload);
    let version = r.u8()?;
    ensure!(version == LAYERS_VERSION, "unsupported format version {version}");
    ensure!((r.u32()?, r.u32()?) == (w, h), "size does not match the image");
    ensure!(r.u32()? == crc32(flat), "the image was changed outside termpaint");
    let active = r.u32()? as usize;
    let counter = r.u32()?;
    let count = r.u32()?;
    ensure!(count > 0, "no layers");
    let mut layers = Vec::new();
    for _ in 0..count {
        let visible = r.u8()? != 0;
        let opacity = r.u8()?.min(100);
        let name_len = r.u16()? as usize;
        let name = std::str::from_utf8(r.take(name_len)?).context("layer name is not UTF-8")?.to_string();
        let png_len = r.u32()? as usize;
        let (lw, lh, pixels) = decode_png(Cursor::new(r.take(png_len)?)).context("layer image")?;
        ensure!((lw, lh) == (w, h), "layer '{name}' size does not match the image");
        layers.push(Layer { name, visible, opacity, pixels });
    }
    // Trailing bytes are ignored so later versions can append fields.
    Ok(Document::from_layers(w, h, layers, active, counter))
}

/// Concatenated data of the `tpLR` chunks: `None` if there are none, an
/// error if they are damaged.
fn layers_payload(png: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut out: Option<Vec<u8>> = None;
    // Skip the signature, which the decoder has already checked.
    let mut rest = &png[8..];
    while let Some((len, tail)) = rest.split_first_chunk::<4>() {
        let len = u32::from_be_bytes(*len) as usize;
        // Chunk type, data and CRC.
        let Some((chunk, next)) = len.checked_add(8).and_then(|n| tail.split_at_checked(n)) else {
            // The file is cut short, but the image decoded, so this only
            // matters if it cut into layer data.
            ensure!(!tail.starts_with(&LAYERS_CHUNK), "layer data is truncated");
            break;
        };
        let (kind, data, crc) = (&chunk[..4], &chunk[4..4 + len], &chunk[4 + len..]);
        if kind == LAYERS_CHUNK {
            ensure!(crc32(&chunk[..4 + len]).to_be_bytes() == crc, "layer data is damaged");
            out.get_or_insert_default().extend_from_slice(data);
        }
        if kind == b"IEND" {
            break;
        }
        rest = next;
    }
    Ok(out)
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc::new();
    c.update(data);
    c.sum()
}

/// Big-endian reader over the layer payload.
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (a, b) = self.0.split_at_checked(n).context("layer data is truncated")?;
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
}

fn png_writer<W: Write>(w: W, width: u32, height: u32) -> Result<png::Writer<W>> {
    let mut enc = png::Encoder::new(w, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    Ok(enc.write_header()?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Rgba;

    fn layered_doc() -> Document {
        let mut d = Document::new(20, 12);
        let mut top = d.blank_layer();
        for px in top.pixels.chunks_exact_mut(4).step_by(3) {
            px.copy_from_slice(&[200, 30, 10, 128]);
        }
        top.opacity = 60;
        let mut hidden = Layer::new("Hidden", 20, 12, Some(Rgba([0, 0, 255, 255])));
        hidden.visible = false;
        d.layers.push(top);
        d.layers.push(hidden);
        d.active = 1;
        d.recomposite(d.bounds());
        d
    }

    fn encode(doc: &Document) -> Vec<u8> {
        let mut buf = Vec::new();
        encode_document(&mut buf, doc).unwrap();
        buf
    }

    #[test]
    fn layers_round_trip() {
        let d = layered_doc();
        let bytes = encode(&d);
        let (back, note) = decode_document(&bytes).unwrap();
        assert_eq!(note, None);
        assert_eq!(back.layers.len(), 3);
        for (a, b) in d.layers.iter().zip(&back.layers) {
            assert_eq!((&a.name, a.visible, a.opacity), (&b.name, b.visible, b.opacity));
            assert!(a.pixels == b.pixels);
        }
        assert_eq!(back.active, 1);
        assert_eq!(back.layer_counter(), d.layer_counter());
        assert!(back.composite == d.composite);
    }

    #[test]
    fn image_data_is_the_flattened_document() {
        let d = layered_doc();
        let (w, h, px) = decode_png(Cursor::new(encode(&d))).unwrap();
        assert_eq!((w, h), (20, 12));
        assert!(px == d.flatten());
    }

    #[test]
    fn plain_png_opens_as_one_layer() {
        let mut buf = Vec::new();
        let mut w = png_writer(&mut buf, 2, 1).unwrap();
        w.write_image_data(&[1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
        w.finish().unwrap();
        let (d, note) = decode_document(&buf).unwrap();
        assert_eq!(note, None);
        assert_eq!(d.layers.len(), 1);
        assert_eq!(d.layers[0].pixels, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    /// Re-encode the image with different pixels but keep the `tpLR` chunks,
    /// like an editor that copies unknown chunks blindly.
    #[test]
    fn stale_layers_are_ignored() {
        let d = layered_doc();
        let bytes = encode(&d);
        let payload = layers_payload(&bytes).unwrap().unwrap();
        let edited = vec![9u8; 20 * 12 * 4];
        let mut buf = Vec::new();
        let mut w = png_writer(&mut buf, 20, 12).unwrap();
        w.write_image_data(&edited).unwrap();
        w.write_chunk(png::chunk::ChunkType(LAYERS_CHUNK), &payload).unwrap();
        w.finish().unwrap();
        let (back, note) = decode_document(&buf).unwrap();
        assert!(note.unwrap().contains("changed outside termpaint"));
        assert_eq!(back.layers.len(), 1);
        assert!(back.layers[0].pixels == edited);
    }

    #[test]
    fn damaged_layers_fall_back_to_the_image() {
        let d = layered_doc();
        let mut bytes = encode(&d);
        let at = bytes.windows(4).position(|c| c == LAYERS_CHUNK).unwrap();
        bytes[at + 40] ^= 0xFF;
        let (back, note) = decode_document(&bytes).unwrap();
        assert!(note.unwrap().contains("damaged"));
        assert_eq!(back.layers.len(), 1);
        assert!(back.layers[0].pixels == d.flatten());
    }

    #[test]
    fn payload_spans_several_chunks() {
        let mut d = Document::new(700, 700);
        // Noise so the layer PNG does not compress below the chunk limit.
        let mut s = 1u32;
        for b in d.layers[0].pixels.iter_mut() {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *b = s as u8;
        }
        let bytes = encode(&d);
        assert!(bytes.windows(4).filter(|c| *c == LAYERS_CHUNK).count() > 1);
        let (back, note) = decode_document(&bytes).unwrap();
        assert_eq!(note, None);
        assert!(back.layers[0].pixels == d.layers[0].pixels);
    }
}
