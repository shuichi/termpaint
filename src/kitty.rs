//! Minimal encoder for the Kitty terminal graphics protocol.
//!
//! Only the subset needed by the paint program is implemented:
//! transmitting RGBA bitmaps, placing them at a cell position with a z-index
//! and deleting images / placements.
//!
//! All commands are sent with `q=2` so the terminal never answers; that keeps
//! responses out of crossterm's input stream.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::fmt::Write as _;
use std::io::Write;

/// Maximum size of a single base64 chunk allowed by the protocol.
const CHUNK: usize = 4096;

/// z-index threshold below which images are drawn *under* cells that have a
/// non-default background colour (INT32_MIN / 2).
pub const Z_BELOW_BG: i32 = -1_073_741_824;

/// Accumulates escape sequences so they can be written in one go.
#[derive(Default)]
pub struct Graphics {
    out: Vec<u8>,
    compress: bool,
}

impl Graphics {
    pub fn new(compress: bool) -> Self {
        Self { out: Vec::with_capacity(1 << 16), compress }
    }

    /// Writes all buffered commands to `w` and clears the buffer.
    pub fn flush_to(&mut self, w: &mut impl Write) -> std::io::Result<()> {
        if !self.out.is_empty() {
            w.write_all(&self.out)?;
            self.out.clear();
        }
        Ok(())
    }

    /// Emit a command with a (possibly large) payload, split into chunks.
    fn command_with_payload(&mut self, control: &str, raw: &[u8]) {
        let (payload, compressed) = if self.compress {
            let mut enc = ZlibEncoder::new(Vec::with_capacity(raw.len() / 4), Compression::fast());
            enc.write_all(raw).expect("in-memory write");
            (enc.finish().expect("in-memory write"), true)
        } else {
            (raw.to_vec(), false)
        };
        let b64 = B64.encode(&payload);
        let bytes = b64.as_bytes();
        let mut first = true;
        let mut pos = 0;
        loop {
            let end = (pos + CHUNK).min(bytes.len());
            let more = if end < bytes.len() { 1 } else { 0 };
            self.out.extend_from_slice(b"\x1b_G");
            if first {
                self.out.extend_from_slice(control.as_bytes());
                if compressed {
                    self.out.extend_from_slice(b",o=z");
                }
                let _ = write!(self.out, ",q=2,m={more}");
                first = false;
            } else {
                let _ = write!(self.out, "m={more}");
            }
            self.out.push(b';');
            self.out.extend_from_slice(&bytes[pos..end]);
            self.out.extend_from_slice(b"\x1b\\");
            pos = end;
            if more == 0 {
                break;
            }
        }
    }

    fn command(&mut self, control: &str) {
        let _ = write!(self.out, "\x1b_G{control},q=2\x1b\\");
    }

    /// Transmit (upload) an RGBA image without displaying it.
    pub fn transmit_rgba(&mut self, id: u32, w: u32, h: u32, rgba: &[u8]) {
        debug_assert_eq!(rgba.len(), (w * h * 4) as usize);
        self.command_with_payload(&format!("a=t,f=32,t=d,i={id},s={w},v={h}"), rgba);
    }

    /// Transmit an image and (re)place it at a cell in one command. Sending
    /// an existing `(id, pid)` replaces both the pixels and the placement, so
    /// small canvas tiles can be refreshed independently.
    #[allow(clippy::too_many_arguments)]
    pub fn transmit_and_place(&mut self, id: u32, pid: u32, col: u16, row: u16, w: u32, h: u32, rgba: &[u8], z: i32) {
        debug_assert_eq!(rgba.len(), (w * h * 4) as usize);
        let _ = write!(self.out, "\x1b[{};{}H", row as u32 + 1, col as u32 + 1);
        self.command_with_payload(&format!("a=T,f=32,t=d,i={id},p={pid},s={w},v={h},z={z},C=1"), rgba);
    }

    pub fn len(&self) -> usize {
        self.out.len()
    }

    /// Create or move a placement of image `id` at the given cell, with a
    /// pixel offset inside that cell, an optional source crop and a z-index.
    /// Re-issuing with the same `(id, pid)` moves the placement atomically.
    #[allow(clippy::too_many_arguments)]
    pub fn place(
        &mut self,
        id: u32,
        pid: u32,
        col: u16,
        row: u16,
        offset: (u32, u32),
        crop: Option<(u32, u32, u32, u32)>,
        z: i32,
    ) {
        let _ = write!(self.out, "\x1b[{};{}H", row as u32 + 1, col as u32 + 1);
        let mut ctl = format!("a=p,i={id},p={pid},z={z},C=1");
        if offset.0 > 0 || offset.1 > 0 {
            let _ = write!(ctl, ",X={},Y={}", offset.0, offset.1);
        }
        if let Some((x, y, w, h)) = crop {
            let _ = write!(ctl, ",x={x},y={y},w={w},h={h}");
        }
        self.command(&ctl);
    }

    /// Remove a placement but keep the image data.
    pub fn delete_placement(&mut self, id: u32, pid: u32) {
        self.command(&format!("a=d,d=i,i={id},p={pid}"));
    }

    /// Remove an image, its placements and free its data.
    pub fn delete_image(&mut self, id: u32) {
        self.command(&format!("a=d,d=I,i={id}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_have_only_m_key_after_first() {
        let mut g = Graphics::new(false);
        let data = vec![7u8; 10_000];
        g.transmit_rgba(1, 50, 50, &data);
        let s = String::from_utf8(g.out.clone()).unwrap();
        let parts: Vec<&str> = s.split("\x1b_G").filter(|p| !p.is_empty()).collect();
        assert!(parts.len() > 1);
        assert!(parts[0].starts_with("a=t,f=32"));
        assert!(parts[0].contains("m=1"));
        for p in &parts[1..parts.len() - 1] {
            assert!(p.starts_with("m=1;"));
        }
        assert!(parts.last().unwrap().starts_with("m=0;"));
    }
}
