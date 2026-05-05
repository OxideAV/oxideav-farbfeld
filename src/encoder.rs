//! farbfeld byte-stream encoder.
//!
//! Mirror of [`crate::parser::parse_farbfeld`]: takes pixel data and
//! emits the on-disk byte stream described in `farbfeld(5)`.
//!
//! Two entry points cover the two natural caller shapes:
//! * [`encode_farbfeld`] — accepts a pre-serialised big-endian RGBA u16
//!   plane (i.e. 8 bytes per pixel, raw on-disk layout). The encoder
//!   prepends the magic + dimensions header and validates the body
//!   length; no per-sample byte swap.
//! * [`encode_farbfeld_from_rgba16`] — accepts native-endian
//!   `[u16; 4]`-per-pixel and performs the big-endian conversion.

use crate::error::{FarbfeldError, Result};
use crate::image::FarbfeldImage;
use crate::parser::{BYTES_PER_PIXEL, HEADER_LEN, MAGIC};

/// Encode a farbfeld file from a raw, already-big-endian RGBA u16 body
/// plane.
///
/// `rgba_u16_be` must be exactly `width * height * 8` bytes long: each
/// pixel is four 16-bit channels, big-endian, in `R, G, B, A` order. No
/// per-sample byte swap is performed; the body is concatenated to the
/// header verbatim.
///
/// Returns [`FarbfeldError::InvalidData`] if the body length doesn't
/// match the announced dimensions.
pub fn encode_farbfeld(width: u32, height: u32, rgba_u16_be: &[u8]) -> Result<Vec<u8>> {
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
    if rgba_u16_be.len() != body_len {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: body length mismatch — caller passed {} bytes, header announces {} ({width}×{height} × {BYTES_PER_PIXEL})",
            rgba_u16_be.len(),
            body_len
        )));
    }

    let mut out = Vec::with_capacity(HEADER_LEN + body_len);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(rgba_u16_be);
    Ok(out)
}

/// Convenience encoder that takes native-endian RGBA u16 pixels and
/// performs the big-endian conversion.
///
/// `pixels` must be exactly `width * height` entries; each
/// `[r, g, b, a]` is written as four big-endian 16-bit samples to the
/// output body.
pub fn encode_farbfeld_from_rgba16(
    width: u32,
    height: u32,
    pixels: &[[u16; 4]],
) -> Result<Vec<u8>> {
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| {
            FarbfeldError::invalid(format!(
                "farbfeld: width*height ({width} * {height}) overflows usize"
            ))
        })?;
    if pixels.len() != pixel_count {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: pixel count mismatch — caller passed {} pixels, header announces {pixel_count} ({width}×{height})",
            pixels.len()
        )));
    }

    let body_len = pixel_count * BYTES_PER_PIXEL;
    let mut out = Vec::with_capacity(HEADER_LEN + body_len);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    for px in pixels {
        for chan in px {
            out.extend_from_slice(&chan.to_be_bytes());
        }
    }
    Ok(out)
}

/// Encode a [`FarbfeldImage`] (native-endian flat plane) into the
/// on-disk byte stream. Convenience wrapper over the two functions
/// above for callers that already hold a [`FarbfeldImage`].
pub fn encode_farbfeld_image(image: &FarbfeldImage) -> Result<Vec<u8>> {
    let pixel_count = (image.width as usize)
        .checked_mul(image.height as usize)
        .ok_or_else(|| {
            FarbfeldError::invalid(format!(
                "farbfeld: width*height ({} * {}) overflows usize",
                image.width, image.height
            ))
        })?;
    let sample_count = pixel_count * 4;
    if image.pixels.len() != sample_count {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: image.pixels has {} samples, header announces {sample_count} ({}×{} × 4)",
            image.pixels.len(),
            image.width,
            image.height
        )));
    }

    let body_len = pixel_count * BYTES_PER_PIXEL;
    let mut out = Vec::with_capacity(HEADER_LEN + body_len);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&image.width.to_be_bytes());
    out.extend_from_slice(&image.height.to_be_bytes());
    for &sample in &image.pixels {
        out.extend_from_slice(&sample.to_be_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_farbfeld;

    #[test]
    fn encode_rejects_body_length_mismatch() {
        // 1×1 pixel = 8 bytes, but caller passes 4.
        assert!(encode_farbfeld(1, 1, &[0u8; 4]).is_err());
        assert!(encode_farbfeld(1, 1, &[0u8; 16]).is_err());
    }

    #[test]
    fn encode_zero_image_is_just_header() {
        let bytes = encode_farbfeld(0, 0, &[]).unwrap();
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(&bytes[8..12], &[0, 0, 0, 0]);
        assert_eq!(&bytes[12..16], &[0, 0, 0, 0]);
    }

    #[test]
    fn encode_single_pixel_byte_exact() {
        let body = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
        let bytes = encode_farbfeld(1, 1, &body).unwrap();
        let mut expected = Vec::from(&b"farbfeld"[..]);
        expected.extend_from_slice(&1u32.to_be_bytes());
        expected.extend_from_slice(&1u32.to_be_bytes());
        expected.extend_from_slice(&body);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn encode_from_rgba16_round_trips() {
        let pixels = [
            [0x1234, 0x5678, 0x9ABC, 0xDEF0],
            [0x0001, 0x0002, 0x0003, 0x0004],
        ];
        let bytes = encode_farbfeld_from_rgba16(2, 1, &pixels).unwrap();
        let parsed = parse_farbfeld(&bytes).unwrap();
        assert_eq!(parsed.width, 2);
        assert_eq!(parsed.height, 1);
        assert_eq!(
            parsed.pixels,
            [0x1234, 0x5678, 0x9ABC, 0xDEF0, 0x0001, 0x0002, 0x0003, 0x0004]
        );
    }

    #[test]
    fn encode_image_round_trips_through_parser() {
        let img = FarbfeldImage {
            width: 3,
            height: 2,
            pixels: (0..(3 * 2 * 4)).map(|i| (i * 0x1111) as u16).collect(),
        };
        let bytes = encode_farbfeld_image(&img).unwrap();
        let parsed = parse_farbfeld(&bytes).unwrap();
        assert_eq!(parsed, img);
    }

    #[test]
    fn encode_from_rgba16_rejects_pixel_count_mismatch() {
        // Caller says 2×2 (=4 pixels) but only passes 3.
        let pixels = [[0u16; 4]; 3];
        assert!(encode_farbfeld_from_rgba16(2, 2, &pixels).is_err());
    }
}
