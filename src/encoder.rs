//! farbfeld byte-stream encoder.
//!
//! Mirror of the parser: takes pixel data and emits the on-disk byte
//! stream described in the workspace's own independent byte-layout
//! description at `docs/image/farbfeld/farbfeld-format.md` — 8-byte
//! ASCII magic, two big-endian `u32` dimensions, then `width * height`
//! pixels of four big-endian `u16` samples in `R, G, B, A` order.
//!
//! The contract entry points live at the crate root ([`crate::encode`]
//! from a [`FarbfeldImage`], [`crate::encode_rgba16`] from native-endian
//! samples, [`crate::encode_rgb8`] / [`crate::encode_rgba8`] from 8-bit
//! input); this module holds their whole-file cores, the shared
//! SIMD-friendly byte-swap helpers, and the deprecated pre-contract
//! names.

use crate::error::{FarbfeldError, Result};
use crate::image::FarbfeldImage;
use crate::parser::{FarbfeldHeader, BYTES_PER_PIXEL, HEADER_LEN, MAGIC};
use crate::rgba16::Rgba16Image;

/// Serialise a flat row-major plane of native-endian `u16` samples
/// (`R, G, B, A` repeated per pixel) into a big-endian byte body
/// pre-allocated by the caller.
///
/// Caller's contract: `out.len() == samples.len() * 2`. The function
/// fills the buffer with the per-sample BE bytes and does no
/// per-iteration bounds proof beyond the `chunks_exact_mut` guarantee,
/// which the auto-vectoriser turns into a SIMD bswap on x86_64
/// (`PSHUFB`) and aarch64 (`REV16`).
#[inline]
pub(crate) fn encode_be_samples(samples: &[u16], out: &mut [u8]) {
    for (sample, slot) in samples.iter().zip(out.chunks_exact_mut(2)) {
        let be = sample.to_be_bytes();
        slot[0] = be[0];
        slot[1] = be[1];
    }
}

/// Serialise a flat plane of native-endian `u16` samples into a
/// little-endian byte buffer pre-allocated by the caller.
///
/// The little-endian sibling of [`encode_be_samples`]. The framework
/// `Decoder` impl (gated behind the `registry` feature) hands the
/// framework a canonical little-endian
/// [`oxideav_core::PixelFormat::Rgba64Le`] plane, so the on-disk
/// big-endian samples decoded by the parser have to be re-serialised
/// in LE word order for `VideoPlane.data`. Routing that through this
/// shared helper — the same `iter().zip(chunks_exact_mut(2))` shape the
/// auto-vectoriser already lifts into a SIMD store for the BE path —
/// keeps the decode hot loop off the slower per-sample
/// `extend_from_slice(&sample.to_le_bytes())` append it used before.
///
/// Caller's contract: `out.len() == samples.len() * 2`. On a
/// little-endian host every `to_le_bytes()` is the identity layout, so
/// the loop collapses to a straight `memcpy`; on a big-endian host it
/// is the 16-bit byte-swap mirror of [`encode_be_samples`].
///
/// Only the framework `Decoder` consumes this, so it is gated behind the
/// `registry` feature — a standalone (`oxideav-core`-free) build never
/// produces an `Rgba64Le` plane and would see it as dead code.
#[inline]
pub(crate) fn encode_le_samples(samples: &[u16], out: &mut [u8]) {
    for (sample, slot) in samples.iter().zip(out.chunks_exact_mut(2)) {
        let le = sample.to_le_bytes();
        slot[0] = le[0];
        slot[1] = le[1];
    }
}

/// Byte-swap a little-endian 16-bit sample plane into a big-endian one,
/// pair by pair, writing into a caller-allocated output buffer.
///
/// The framework `Encoder` (gated behind `registry`) is handed canonical
/// little-endian [`oxideav_core::PixelFormat::Rgba64Le`] rows and must
/// re-serialise them in the on-disk big-endian word order. Doing that
/// with this `chunks_exact(2).zip(chunks_exact_mut(2))` shape — instead
/// of a per-sample `u16::from_le_bytes` / `to_be_bytes` round-trip
/// through a scalar then an `extend_from_slice` append — lets the
/// auto-vectoriser fuse the load, 16-bit swap and store into the same
/// SIMD `bswap` it already emits for [`encode_be_samples`].
///
/// Caller's contract: `dst.len() == src.len()` and both are an even
/// number of bytes. Any trailing odd byte (a malformed half-sample) is
/// left untouched in `dst` rather than panicking, mirroring the
/// defensive shape of [`crate::parser::decode_be_samples`]. The host's
/// own endianness is irrelevant: this is a pure byte-order transform
/// between two explicit on-wire layouts, so it behaves identically on
/// big- and little-endian targets.
///
/// Only the framework `Encoder` consumes this, so it is gated behind the
/// `registry` feature — a standalone build never sees an `Rgba64Le`
/// plane and would flag it as dead code.
#[inline]
pub(crate) fn swap_pairs(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.chunks_exact(2).zip(dst.chunks_exact_mut(2)) {
        // LE [lo, hi] -> BE [hi, lo].
        d[0] = s[1];
        d[1] = s[0];
    }
}

/// Allocate a whole farbfeld file for `header` with the 16-byte header
/// filled in and the body zeroed.
#[inline]
fn file_with_header(header: &FarbfeldHeader) -> Result<Vec<u8>> {
    let total = header.total_len()?;
    let mut out = vec![0u8; total];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&header.width.to_be_bytes());
    out[12..16].copy_from_slice(&header.height.to_be_bytes());
    Ok(out)
}

/// The whole-file encode behind [`crate::encode`]: the image's single
/// `Rgba64Le` plane, byte-swapped row by row (stride padding skipped)
/// into the big-endian wire body. The image's geometry was validated by
/// its constructor, so this only fails on `usize` overflow.
pub(crate) fn encode_image(image: &FarbfeldImage) -> Result<Vec<u8>> {
    let header = FarbfeldHeader::new(image.width, image.height)?;
    let mut out = file_with_header(&header)?;
    let row_bytes = image.width as usize * BYTES_PER_PIXEL;
    if row_bytes > 0 {
        let stride = image.stride();
        let src = image.as_bytes().unwrap_or(&[]);
        for (y, dst) in out[HEADER_LEN..].chunks_exact_mut(row_bytes).enumerate() {
            // The constructor proved every row is in bounds.
            swap_pairs(&src[y * stride..y * stride + row_bytes], dst);
        }
    }
    Ok(out)
}

/// The whole-file encode behind [`crate::encode_rgba16`]: native-endian
/// samples straight to the big-endian wire, one pass.
pub(crate) fn encode_rgba16_samples(width: u32, height: u32, samples: &[u16]) -> Result<Vec<u8>> {
    let header = FarbfeldHeader::new(width, height)?;
    let sample_count = header.body_len / 2;
    if samples.len() != sample_count {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: {} samples passed, header announces {sample_count} ({width}×{height} × 4)",
            samples.len()
        )));
    }
    let mut out = file_with_header(&header)?;
    encode_be_samples(samples, &mut out[HEADER_LEN..]);
    Ok(out)
}

/// Header + an already big-endian body, verbatim.
pub(crate) fn encode_be_body(width: u32, height: u32, body_be: &[u8]) -> Result<Vec<u8>> {
    let header = FarbfeldHeader::new(width, height)?;
    if body_be.len() != header.body_len {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: body length mismatch — caller passed {} bytes, header announces {} ({width}×{height} × {BYTES_PER_PIXEL})",
            body_be.len(),
            header.body_len
        )));
    }
    let mut out = file_with_header(&header)?;
    out[HEADER_LEN..].copy_from_slice(body_be);
    Ok(out)
}

/// Encode a complete farbfeld file from a pre-serialised big-endian
/// RGBA `u16` body (`width * height * 8` bytes, exactly).
///
/// Pre-contract entry point. The contract path is [`crate::encode`]
/// (from a [`FarbfeldImage`]) or [`crate::encode_rgba16`] (from
/// native-endian samples); a caller holding wire-order bytes can also
/// stream them through [`crate::FarbfeldStreamWriter::write_all_rows_raw`].
#[deprecated(
    note = "use oxideav_farbfeld::encode / encode_rgba16 or FarbfeldStreamWriter::write_all_rows_raw (IMAGE_CRATE_API)"
)]
pub fn encode_farbfeld(width: u32, height: u32, rgba_u16_be: &[u8]) -> Result<Vec<u8>> {
    encode_be_body(width, height, rgba_u16_be)
}

/// Encode a complete farbfeld file from native-endian `[R, G, B, A]`
/// `u16` quads (`width * height` of them, exactly).
///
/// Pre-contract entry point; [`crate::encode_rgba16`] takes the same
/// samples as a flat `&[u16]`.
#[deprecated(note = "use oxideav_farbfeld::encode_rgba16 (IMAGE_CRATE_API)")]
pub fn encode_farbfeld_from_rgba16(
    width: u32,
    height: u32,
    pixels: &[[u16; 4]],
) -> Result<Vec<u8>> {
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| {
            FarbfeldError::unsupported(format!(
                "farbfeld: width*height ({width} * {height}) overflows usize"
            ))
        })?;
    if pixels.len() != pixel_count {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: pixel count mismatch — caller passed {} pixels, header announces {pixel_count} ({width}×{height})",
            pixels.len()
        )));
    }
    encode_rgba16_samples(width, height, flatten_rgba_pixels(pixels))
}

/// Encode a complete farbfeld file from a 16-bit sample view.
///
/// Pre-contract entry point (its parameter was the old `FarbfeldImage`,
/// now [`Rgba16Image`]); [`crate::encode_rgba16`] or
/// `encode(&FarbfeldImage::from(img), ..)` are the contract paths.
#[deprecated(note = "use oxideav_farbfeld::encode / encode_rgba16 (IMAGE_CRATE_API)")]
pub fn encode_farbfeld_image(image: &Rgba16Image) -> Result<Vec<u8>> {
    encode_rgba16_samples(image.width, image.height, &image.data)
}

/// View `[u16; 4]` quads as a flat `&[u16]` of four times the length.
#[inline]
fn flatten_rgba_pixels(pixels: &[[u16; 4]]) -> &[u16] {
    // SAFETY: `[u16; 4]`'s memory layout — four packed `u16` values, no
    // niche, no discriminant, alignment of `u16` — is guaranteed by the
    // language reference, so the resulting `&[u16]` of length
    // `pixels.len() * 4` aliases the input bytes 1:1.
    unsafe { core::slice::from_raw_parts(pixels.as_ptr() as *const u16, pixels.len() * 4) }
}

#[cfg(test)]
#[allow(deprecated)]
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
            parsed.data,
            [0x1234, 0x5678, 0x9ABC, 0xDEF0, 0x0001, 0x0002, 0x0003, 0x0004]
        );
    }

    #[test]
    fn encode_image_round_trips_through_parser() {
        let img = Rgba16Image {
            width: 3,
            height: 2,
            data: (0..(3 * 2 * 4)).map(|i| (i * 0x1111) as u16).collect(),
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

    #[test]
    fn flatten_rgba_pixels_aliases_input_bytes_one_to_one() {
        // The unsafe `[[u16; 4]] -> [u16]` cast underpins the SIMD-
        // friendly `encode_be_samples` hot loop on
        // `encode_farbfeld_from_rgba16`. Prove it observes the same
        // samples in the same order, with no shuffling.
        let pixels = [
            [0x0001u16, 0x0002, 0x0003, 0x0004],
            [0x0005, 0x0006, 0x0007, 0x0008],
            [0x0009, 0x000A, 0x000B, 0x000C],
        ];
        let flat = flatten_rgba_pixels(&pixels);
        assert_eq!(flat.len(), 12);
        for (i, &sample) in flat.iter().enumerate() {
            let px = i / 4;
            let ch = i % 4;
            assert_eq!(sample, pixels[px][ch], "sample {i}: pixel {px} chan {ch}");
        }
    }

    #[test]
    fn flatten_rgba_pixels_handles_empty_input() {
        // Zero-length input is a degenerate but well-formed shape —
        // a 0×0 image carries no pixels.
        let pixels: [[u16; 4]; 0] = [];
        let flat = flatten_rgba_pixels(&pixels);
        assert!(flat.is_empty());
    }

    #[test]
    fn encode_be_samples_byte_swap_inverts_decode_be_samples() {
        // The shared hot-loop helpers are each other's inverse on every
        // u16: encode_be_samples(samples, out); decode_be_samples(out,
        // round) reproduces samples.
        use crate::parser::decode_be_samples;
        let samples: Vec<u16> = (0..1024u16).collect();
        let mut bytes = vec![0u8; samples.len() * 2];
        encode_be_samples(&samples, &mut bytes);
        let mut round = vec![0u16; samples.len()];
        decode_be_samples(&bytes, &mut round);
        assert_eq!(round, samples);
        // Spot-check the BE byte order on one sample.
        assert_eq!(&bytes[0..2], &[0x00, 0x00]); // sample 0
        assert_eq!(&bytes[2..4], &[0x00, 0x01]); // sample 1
        assert_eq!(&bytes[510..512], &[0x00, 0xFF]); // sample 255
        assert_eq!(&bytes[512..514], &[0x01, 0x00]); // sample 256
    }

    #[cfg(feature = "registry")]
    #[test]
    fn encode_le_samples_writes_little_endian_word_order() {
        // The LE helper underpins the framework decode hot loop. Prove
        // it emits the low byte first for every sample and is the exact
        // inverse of a `from_le_bytes` read.
        let samples: Vec<u16> = (0..1024u16).collect();
        let mut bytes = vec![0u8; samples.len() * 2];
        encode_le_samples(&samples, &mut bytes);
        // Spot-check the LE byte order on a few samples.
        assert_eq!(&bytes[0..2], &[0x00, 0x00]); // sample 0
        assert_eq!(&bytes[2..4], &[0x01, 0x00]); // sample 1 -> [lo, hi]
        assert_eq!(&bytes[510..512], &[0xFF, 0x00]); // sample 255
        assert_eq!(&bytes[512..514], &[0x00, 0x01]); // sample 256
                                                     // Round-trip: from_le_bytes reproduces every sample.
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            assert_eq!(u16::from_le_bytes([pair[0], pair[1]]), samples[i]);
        }
    }

    #[cfg(feature = "registry")]
    #[test]
    fn encode_le_samples_and_encode_be_samples_swap_each_other_byte_order() {
        // BE and LE serialisations of the same plane are byte-reversed
        // within every 2-byte pair.
        let samples: Vec<u16> = vec![0x1234, 0x5678, 0x9ABC, 0xDEF0];
        let mut be = vec![0u8; samples.len() * 2];
        let mut le = vec![0u8; samples.len() * 2];
        encode_be_samples(&samples, &mut be);
        encode_le_samples(&samples, &mut le);
        for (b, l) in be.chunks_exact(2).zip(le.chunks_exact(2)) {
            assert_eq!(b[0], l[1]);
            assert_eq!(b[1], l[0]);
        }
    }

    #[cfg(feature = "registry")]
    #[test]
    fn encode_le_samples_zero_length_is_a_noop() {
        let mut out: [u8; 0] = [];
        encode_le_samples(&[], &mut out);
        assert!(out.is_empty());
    }

    #[cfg(feature = "registry")]
    #[test]
    fn swap_pairs_reverses_each_two_byte_pair() {
        // LE [lo, hi] becomes BE [hi, lo] for every sample, and the
        // result equals reading the LE bytes as a u16 then writing it BE.
        let src = vec![0x34u8, 0x12, 0x78, 0x56, 0xBC, 0x9A, 0xF0, 0xDE];
        let mut dst = vec![0u8; src.len()];
        swap_pairs(&src, &mut dst);
        assert_eq!(dst, vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]);
        // Cross-check against the scalar from_le/to_be reference.
        for (s, d) in src.chunks_exact(2).zip(dst.chunks_exact(2)) {
            let v = u16::from_le_bytes([s[0], s[1]]);
            assert_eq!([d[0], d[1]], v.to_be_bytes());
        }
    }

    #[cfg(feature = "registry")]
    #[test]
    fn swap_pairs_is_its_own_inverse() {
        // Applying the LE->BE swap twice restores the original bytes
        // (the transform is a pure pairwise byte reversal).
        let src: Vec<u8> = (0..256u16).flat_map(|v| v.to_le_bytes()).collect();
        let mut once = vec![0u8; src.len()];
        swap_pairs(&src, &mut once);
        let mut twice = vec![0u8; src.len()];
        swap_pairs(&once, &mut twice);
        assert_eq!(twice, src);
    }

    #[cfg(feature = "registry")]
    #[test]
    fn swap_pairs_zero_length_is_a_noop() {
        let mut dst: [u8; 0] = [];
        swap_pairs(&[], &mut dst);
        assert!(dst.is_empty());
    }

    #[test]
    fn encode_farbfeld_from_rgba16_bulk_path_matches_per_pixel_reference() {
        // The optimised path routes through `flatten_rgba_pixels` +
        // `encode_be_samples`. Cross-check against a pure-Rust per-pixel
        // reference encoder that loops `to_be_bytes` to confirm byte
        // identity at every offset.
        let w = 17u32;
        let h = 13u32;
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = (y * w + x) as u16;
                pixels.push([
                    v.wrapping_mul(0x0123),
                    v.wrapping_mul(0x4567),
                    v.wrapping_mul(0x89AB),
                    v.wrapping_mul(0xCDEF),
                ]);
            }
        }
        let fast = encode_farbfeld_from_rgba16(w, h, &pixels).unwrap();

        // Per-pixel reference: 16-byte header + `to_be_bytes` per sample.
        let mut reference = Vec::with_capacity(fast.len());
        reference.extend_from_slice(MAGIC);
        reference.extend_from_slice(&w.to_be_bytes());
        reference.extend_from_slice(&h.to_be_bytes());
        for px in &pixels {
            for chan in px {
                reference.extend_from_slice(&chan.to_be_bytes());
            }
        }
        assert_eq!(fast, reference);
    }

    #[test]
    fn encode_farbfeld_image_bulk_path_matches_per_sample_reference() {
        // Same cross-check shape for `encode_farbfeld_image`, which
        // routes through `encode_be_samples` directly on the flat
        // `Vec<u16>` plane.
        let w = 19u32;
        let h = 11u32;
        let sample_count = (w * h * 4) as usize;
        let pixels: Vec<u16> = (0..sample_count).map(|i| (i * 0x1111) as u16).collect();
        let img = Rgba16Image {
            width: w,
            height: h,
            data: pixels.clone(),
        };
        let fast = encode_farbfeld_image(&img).unwrap();

        // Per-sample reference.
        let mut reference = Vec::with_capacity(fast.len());
        reference.extend_from_slice(MAGIC);
        reference.extend_from_slice(&w.to_be_bytes());
        reference.extend_from_slice(&h.to_be_bytes());
        for &s in &pixels {
            reference.extend_from_slice(&s.to_be_bytes());
        }
        assert_eq!(fast, reference);
    }
}
