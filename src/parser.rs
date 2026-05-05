//! farbfeld byte-stream parser.
//!
//! The format, in full (per the public `farbfeld(5)` man page):
//!
//! ```text
//!   offset  bytes  field
//!   ------  -----  -----------------------------
//!        0      8  magic = ASCII "farbfeld"
//!        8      4  width  (u32 big-endian)
//!       12      4  height (u32 big-endian)
//!       16    8·N  pixels: width*height rows of 4×u16 BE = R, G, B, A
//! ```
//!
//! There is no compression, no palette, no per-pixel metadata — every
//! pixel is exactly 16 bits per channel, four channels, big-endian, in
//! row-major scan order. Total file size is `16 + 8 * width * height`
//! bytes; anything shorter is truncated, anything longer carries
//! trailing garbage which this parser rejects.

use crate::error::{FarbfeldError, Result};
use crate::image::FarbfeldImage;

/// Magic bytes that prefix every farbfeld file: ASCII `"farbfeld"`.
pub const MAGIC: &[u8; 8] = b"farbfeld";

/// Length of the fixed-size header (magic + width + height) in bytes.
pub const HEADER_LEN: usize = 16;

/// Bytes per pixel on disk: 4 channels × 2 bytes per channel.
pub const BYTES_PER_PIXEL: usize = 8;

/// Parse a complete farbfeld byte stream into a [`FarbfeldImage`].
///
/// Returns [`FarbfeldError::InvalidData`] for any of:
/// * fewer than 16 header bytes;
/// * the magic prefix is not literal ASCII `"farbfeld"`;
/// * `width * height * 8` overflows `usize` (only on 32-bit hosts with
///   pathological dimensions);
/// * the body length doesn't exactly equal `width * height * 8`.
pub fn parse_farbfeld(bytes: &[u8]) -> Result<FarbfeldImage> {
    if bytes.len() < HEADER_LEN {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: header truncated — got {} bytes, need at least {HEADER_LEN}",
            bytes.len()
        )));
    }
    if &bytes[..8] != MAGIC {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: bad magic {:?}, expected {:?}",
            &bytes[..8],
            MAGIC
        )));
    }
    // SAFETY of indexing: we just checked `bytes.len() >= 16`.
    let width = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let height = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);

    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| {
            FarbfeldError::invalid(format!(
                "farbfeld: width*height ({width} * {height}) overflows usize"
            ))
        })?;
    let body_len = pixel_count.checked_mul(BYTES_PER_PIXEL).ok_or_else(|| {
        FarbfeldError::invalid(format!(
            "farbfeld: pixel byte count ({pixel_count} * {BYTES_PER_PIXEL}) overflows usize"
        ))
    })?;
    let expected_total = HEADER_LEN.checked_add(body_len).ok_or_else(|| {
        FarbfeldError::invalid("farbfeld: total file size overflows usize".to_string())
    })?;

    if bytes.len() != expected_total {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: body size mismatch — file has {} bytes, header announces {} ({width}×{height} pixels × {BYTES_PER_PIXEL} bytes + {HEADER_LEN} header)",
            bytes.len(),
            expected_total
        )));
    }

    // Decode big-endian 16-bit samples into native-endian u16. Each
    // pixel takes 8 bytes / 4 samples; the loop is the entire body.
    let body = &bytes[HEADER_LEN..];
    let sample_count = pixel_count * 4;
    let mut pixels = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let off = i * 2;
        pixels.push(u16::from_be_bytes([body[off], body[off + 1]]));
    }

    Ok(FarbfeldImage {
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_input() {
        assert!(parse_farbfeld(&[]).is_err());
    }

    #[test]
    fn rejects_short_header() {
        assert!(parse_farbfeld(b"farbfeld\x00\x00").is_err());
    }

    #[test]
    fn rejects_wrong_magic() {
        // 16 bytes, but magic is wrong.
        let mut buf = [0u8; 16];
        buf[..8].copy_from_slice(b"FARBFELD");
        assert!(parse_farbfeld(&buf).is_err());
    }

    #[test]
    fn rejects_truncated_body() {
        // 1×1 image (header announces 8 body bytes) but body has 4.
        let mut buf = Vec::from(&b"farbfeld"[..]);
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&[0u8; 4]);
        assert!(parse_farbfeld(&buf).is_err());
    }

    #[test]
    fn rejects_oversized_body() {
        // 1×1 image (header announces 8 body bytes) but body has 16.
        let mut buf = Vec::from(&b"farbfeld"[..]);
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&[0u8; 16]);
        assert!(parse_farbfeld(&buf).is_err());
    }

    #[test]
    fn parses_single_pixel() {
        let mut buf = Vec::from(&b"farbfeld"[..]);
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&1u32.to_be_bytes());
        // R=0x1234, G=0x5678, B=0x9ABC, A=0xDEF0
        buf.extend_from_slice(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]);
        let img = parse_farbfeld(&buf).unwrap();
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 1);
        assert_eq!(img.pixels, [0x1234, 0x5678, 0x9ABC, 0xDEF0]);
    }

    #[test]
    fn parses_zero_dimension() {
        // 0×0 image — valid, body is empty, total file = 16 bytes.
        let mut buf = Vec::from(&b"farbfeld"[..]);
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        let img = parse_farbfeld(&buf).unwrap();
        assert_eq!(img.width, 0);
        assert_eq!(img.height, 0);
        assert!(img.pixels.is_empty());
    }

    #[test]
    fn parses_2x3_pixel_order_is_row_major() {
        // 2×3 = 6 pixels; encode each pixel with R = (y*2+x), 0 elsewhere.
        let w = 2u32;
        let h = 3u32;
        let mut buf = Vec::from(&b"farbfeld"[..]);
        buf.extend_from_slice(&w.to_be_bytes());
        buf.extend_from_slice(&h.to_be_bytes());
        for y in 0..h {
            for x in 0..w {
                let r = (y * w + x) as u16;
                buf.extend_from_slice(&r.to_be_bytes());
                buf.extend_from_slice(&0u16.to_be_bytes());
                buf.extend_from_slice(&0u16.to_be_bytes());
                buf.extend_from_slice(&0u16.to_be_bytes());
            }
        }
        let img = parse_farbfeld(&buf).unwrap();
        for y in 0..h {
            for x in 0..w {
                let base = ((y * w + x) * 4) as usize;
                assert_eq!(img.pixels[base], (y * w + x) as u16);
                assert_eq!(img.pixels[base + 1], 0);
                assert_eq!(img.pixels[base + 2], 0);
                assert_eq!(img.pixels[base + 3], 0);
            }
        }
    }
}
