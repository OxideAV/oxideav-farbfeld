//! farbfeld byte-stream encoder.
//!
//! Mirror of [`crate::parser::parse_farbfeld`]: takes pixel data and
//! emits the on-disk byte stream described in the workspace's own
//! independent byte-layout description at
//! `docs/image/farbfeld/farbfeld-format.md` — 8-byte ASCII magic,
//! two big-endian `u32` dimensions, then `width * height` pixels of
//! four big-endian `u16` samples in `R, G, B, A` order.
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
#[inline]
pub(crate) fn swap_pairs_le_to_be(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.chunks_exact(2).zip(dst.chunks_exact_mut(2)) {
        // LE [lo, hi] -> BE [hi, lo].
        d[0] = s[1];
        d[1] = s[0];
    }
}

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

    let mut out = vec![0u8; HEADER_LEN + body_len];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&width.to_be_bytes());
    out[12..16].copy_from_slice(&height.to_be_bytes());
    out[HEADER_LEN..].copy_from_slice(rgba_u16_be);
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
    let mut out = vec![0u8; HEADER_LEN + body_len];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&width.to_be_bytes());
    out[12..16].copy_from_slice(&height.to_be_bytes());
    // Reinterpret `pixels` as a flat `&[u16]` view of the body (each
    // `[u16; 4]` pixel is four consecutive samples) so we can run the
    // shared SIMD-friendly `encode_be_samples` loop over the whole plane
    // in one pass instead of four `extend_from_slice` calls per pixel.
    // `pixel_count * 4` cannot overflow because `pixel_count *
    // BYTES_PER_PIXEL` (= 8 ×) already succeeded just above.
    let flat: &[u16] = flatten_rgba_pixels(pixels);
    encode_be_samples(flat, &mut out[HEADER_LEN..]);
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
    let mut out = vec![0u8; HEADER_LEN + body_len];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&image.width.to_be_bytes());
    out[12..16].copy_from_slice(&image.height.to_be_bytes());
    encode_be_samples(&image.pixels, &mut out[HEADER_LEN..]);
    Ok(out)
}

/// View a `&[[u16; 4]]` pixel plane as a flat `&[u16]` sample plane,
/// in-place, with no allocation.
///
/// Lets [`encode_farbfeld_from_rgba16`] route through the same SIMD-
/// friendly `encode_be_samples` helper that the streaming writer and
/// `encode_farbfeld_image` use — without the helper, the `[[u16; 4]]`
/// input shape would force a per-pixel inner loop the optimiser
/// wouldn't vectorise.
///
/// The cast is sound because:
/// * `[u16; 4]` is a packed array of four `u16` values; Rust guarantees
///   no padding inside a `[T; N]`, and the array's alignment is
///   `align_of::<u16>()`. A `u16` slice over the same bytes therefore
///   aliases the same samples 1:1.
/// * The returned slice's lifetime is tied to the input borrow, so the
///   pixel buffer stays alive for the whole encode call.
/// * `pixels.len() * 4` cannot overflow at the call sites here: the
///   pixel-count cross-check in [`encode_farbfeld_from_rgba16`]
///   already proved `pixel_count * BYTES_PER_PIXEL` (= 8 ×) didn't
///   overflow, so the 4 × form can't either.
#[inline]
fn flatten_rgba_pixels(pixels: &[[u16; 4]]) -> &[u16] {
    // SAFETY: see the doc-comment above. `[u16; 4]`'s memory layout —
    // four packed `u16` values, no niche, no discriminant, alignment of
    // `u16` — is guaranteed by the language reference, so the resulting
    // `&[u16]` of length `pixels.len() * 4` aliases the input bytes 1:1.
    unsafe { core::slice::from_raw_parts(pixels.as_ptr() as *const u16, pixels.len() * 4) }
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

    #[test]
    fn encode_le_samples_zero_length_is_a_noop() {
        let mut out: [u8; 0] = [];
        encode_le_samples(&[], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn swap_pairs_le_to_be_reverses_each_two_byte_pair() {
        // LE [lo, hi] becomes BE [hi, lo] for every sample, and the
        // result equals reading the LE bytes as a u16 then writing it BE.
        let src = vec![0x34u8, 0x12, 0x78, 0x56, 0xBC, 0x9A, 0xF0, 0xDE];
        let mut dst = vec![0u8; src.len()];
        swap_pairs_le_to_be(&src, &mut dst);
        assert_eq!(dst, vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]);
        // Cross-check against the scalar from_le/to_be reference.
        for (s, d) in src.chunks_exact(2).zip(dst.chunks_exact(2)) {
            let v = u16::from_le_bytes([s[0], s[1]]);
            assert_eq!([d[0], d[1]], v.to_be_bytes());
        }
    }

    #[test]
    fn swap_pairs_le_to_be_is_its_own_inverse() {
        // Applying the LE->BE swap twice restores the original bytes
        // (the transform is a pure pairwise byte reversal).
        let src: Vec<u8> = (0..256u16).flat_map(|v| v.to_le_bytes()).collect();
        let mut once = vec![0u8; src.len()];
        swap_pairs_le_to_be(&src, &mut once);
        let mut twice = vec![0u8; src.len()];
        swap_pairs_le_to_be(&once, &mut twice);
        assert_eq!(twice, src);
    }

    #[test]
    fn swap_pairs_le_to_be_zero_length_is_a_noop() {
        let mut dst: [u8; 0] = [];
        swap_pairs_le_to_be(&[], &mut dst);
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
        let img = FarbfeldImage {
            width: w,
            height: h,
            pixels: pixels.clone(),
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
