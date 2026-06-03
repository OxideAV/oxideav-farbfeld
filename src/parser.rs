//! farbfeld byte-stream parser.
//!
//! The format, in full (per the workspace's independent factual
//! byte-layout description at
//! [`docs/image/farbfeld/farbfeld-format.md`](https://github.com/OxideAV/docs/tree/master/image/farbfeld)):
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
//!
//! ## DoS hardening
//!
//! The header carries two `u32` dimensions. A maliciously-crafted file
//! can declare `width = height = u32::MAX / 2` while shipping only the
//! 16-byte header. To prevent that turning into a multi-gigabyte
//! [`Vec`] allocation, [`parse_farbfeld`] cross-checks the announced
//! `width * height * 8` body length against the **actual** number of
//! body bytes available **before** allocating the decoded pixel buffer.
//! Any mismatch is reported as [`FarbfeldError::InvalidData`] without
//! attempting to allocate the announced-but-absent body capacity.

use crate::error::{FarbfeldError, Result};
use crate::image::FarbfeldImage;

/// Magic bytes that prefix every farbfeld file: ASCII `"farbfeld"`.
pub const MAGIC: &[u8; 8] = b"farbfeld";

/// Length of the fixed-size header (magic + width + height) in bytes.
pub const HEADER_LEN: usize = 16;

/// Bytes per pixel on disk: 4 channels × 2 bytes per channel.
pub const BYTES_PER_PIXEL: usize = 8;

/// Decoded farbfeld header: dimensions plus the computed body length.
///
/// Returned by [`parse_farbfeld_header`] for callers that want to
/// inspect the dimensions before committing to a full in-memory parse
/// (e.g. to refuse images larger than a per-application sandbox cap, or
/// to choose between [`parse_farbfeld`] and the streaming reader).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FarbfeldHeader {
    /// Picture width in pixels, exactly as carried on disk.
    pub width: u32,
    /// Picture height in pixels, exactly as carried on disk.
    pub height: u32,
    /// `width * height * 8` — the number of body bytes that **must**
    /// follow the 16-byte header for the file to be well-formed.
    pub body_len: usize,
}

impl FarbfeldHeader {
    /// Total on-disk size of the farbfeld file this header announces,
    /// in bytes: `HEADER_LEN + body_len` = `16 + width * height * 8`.
    ///
    /// Returns [`FarbfeldError::InvalidData`] in the degenerate case
    /// where `body_len` is so large that adding the 16-byte header
    /// overflows `usize` (only reachable on 32-bit hosts with
    /// `width * height` near `usize::MAX / 8`).
    ///
    /// Useful for pre-flight checks against a per-application file-size
    /// cap before committing to a body read, and to assert the
    /// header-promised file size against what an `io::Read` source
    /// will deliver.
    pub fn total_len(&self) -> Result<usize> {
        HEADER_LEN.checked_add(self.body_len).ok_or_else(|| {
            FarbfeldError::invalid("farbfeld: total file size overflows usize".to_string())
        })
    }
}

/// Peek the 16-byte farbfeld header off the front of `bytes` without
/// touching the body.
///
/// Convenience wrapper around [`parse_farbfeld_header`] that takes the
/// whole file (or a prefix of it) and reads only the first
/// [`HEADER_LEN`] bytes; the rest is ignored, so callers can pass a
/// short prefix `&buf[..16]`, a `bytes` of arbitrary length, or even a
/// memory-mapped large file without committing to a full parse.
///
/// Returns [`FarbfeldError::InvalidData`] for any of:
/// * fewer than 16 header bytes;
/// * the magic prefix is not literal ASCII `"farbfeld"`;
/// * `width * height * 8` overflows `usize`.
///
/// The returned [`FarbfeldHeader`]'s [`FarbfeldHeader::total_len`]
/// reports the exact on-disk file size the header announces, which
/// callers can use to refuse over-large images before allocating the
/// body. This function never inspects bytes past offset 15, so the
/// "DoS hardening" body-mismatch check that [`parse_farbfeld`] runs
/// **is not** run here — it's the caller's contract to verify the
/// body length before committing further.
pub fn peek_farbfeld_dimensions(bytes: &[u8]) -> Result<FarbfeldHeader> {
    // `parse_farbfeld_header` already only reads the first 16 bytes;
    // this entry point exists to give callers a name that documents
    // the intent (peek, don't parse) and to keep the call site short.
    parse_farbfeld_header(bytes)
}

/// Parse the 16-byte farbfeld header.
///
/// Validates the magic prefix and that `width * height * 8` does not
/// overflow `usize`. Does **not** look at the body — callers can use
/// [`FarbfeldHeader::body_len`] to size the body read or to reject an
/// over-large image before committing to a buffer allocation.
pub fn parse_farbfeld_header(header: &[u8]) -> Result<FarbfeldHeader> {
    if header.len() < HEADER_LEN {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: header truncated — got {} bytes, need at least {HEADER_LEN}",
            header.len()
        )));
    }
    if &header[..8] != MAGIC {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: bad magic {:?}, expected {:?}",
            &header[..8],
            MAGIC
        )));
    }
    let width = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    let height = u32::from_be_bytes([header[12], header[13], header[14], header[15]]);
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
    Ok(FarbfeldHeader {
        width,
        height,
        body_len,
    })
}

/// Parse a complete farbfeld byte stream into a [`FarbfeldImage`].
///
/// Returns [`FarbfeldError::InvalidData`] for any of:
/// * fewer than 16 header bytes;
/// * the magic prefix is not literal ASCII `"farbfeld"`;
/// * `width * height * 8` overflows `usize` (only on 32-bit hosts with
///   pathological dimensions);
/// * the body length doesn't exactly equal `width * height * 8`.
///
/// The body length cross-check happens **before** the pixel buffer is
/// allocated, so a crafted header announcing a multi-gigabyte body on
/// a 17-byte file is rejected without first allocating gigabytes of
/// pixel-buffer capacity. See the module-level "DoS hardening" note.
pub fn parse_farbfeld(bytes: &[u8]) -> Result<FarbfeldImage> {
    let header = parse_farbfeld_header(bytes)?;
    let expected_total = header.total_len()?;
    if bytes.len() != expected_total {
        return Err(FarbfeldError::invalid(format!(
            "farbfeld: body size mismatch — file has {} bytes, header announces {} ({}×{} pixels × {BYTES_PER_PIXEL} bytes + {HEADER_LEN} header)",
            bytes.len(),
            expected_total,
            header.width,
            header.height,
        )));
    }

    // Body length matches the header; safe to allocate the pixel buffer
    // at full capacity. Each pixel is 4 u16 samples, 8 bytes on disk.
    //
    // Hot path: instead of `push`-ing one u16 at a time inside an
    // index-and-decode loop (which the optimiser tends not to vectorise
    // because of the in-bounds proof on each `body[off]`), allocate the
    // sample buffer up front with `resize` and walk it in lockstep with
    // `body.chunks_exact(2)`. The compiler then sees two `&[u8; 2]`-
    // shaped slices joined by `zip` and can hoist the `from_be_bytes`
    // into a single 16-bit byte-swap per pair, which the auto-vectoriser
    // turns into a SIMD bswap (8 samples / 16 bytes per cycle on
    // contemporary x86_64 / aarch64).
    let body = &bytes[HEADER_LEN..];
    let sample_count = header.body_len / 2;
    let mut pixels = vec![0u16; sample_count];
    decode_be_samples(body, &mut pixels);

    Ok(FarbfeldImage {
        width: header.width,
        height: header.height,
        pixels,
    })
}

/// Decode `body` (a `2 * out.len()`-byte big-endian u16 plane) into
/// `out`'s slots, in lockstep.
///
/// Factored out so [`parse_farbfeld`] and the streaming reader can share
/// the same hot loop. The shape — `chunks_exact(2)` zipped with a
/// `&mut [u16]` — is the one the auto-vectoriser picks up; per-iteration
/// bounds proofs are discharged by `chunks_exact`'s static length, so
/// the inner body collapses to `u16::from_be_bytes` on a `[u8; 2]` slot
/// and a single store.
///
/// Caller's contract: `body.len() == out.len() * 2`. The function will
/// only fill min(out.len(), body.len() / 2) slots if the contract is
/// violated; it does not panic.
#[inline]
pub(crate) fn decode_be_samples(body: &[u8], out: &mut [u16]) {
    for (chunk, slot) in body.chunks_exact(2).zip(out.iter_mut()) {
        // `chunks_exact` yields `&[u8]` of guaranteed length 2; the
        // `try_into` is a const-time copy the optimiser sees through.
        *slot = u16::from_be_bytes([chunk[0], chunk[1]]);
    }
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
    fn header_total_len_matches_header_plus_body() {
        // 3×4 image = 12 pixels × 8 bytes = 96 body bytes; +16 header
        // = 112 total.
        let h = FarbfeldHeader {
            width: 3,
            height: 4,
            body_len: 96,
        };
        assert_eq!(h.total_len().unwrap(), HEADER_LEN + 96);
        assert_eq!(h.total_len().unwrap(), 112);
    }

    #[test]
    fn header_total_len_handles_zero_dimension() {
        // Empty body — total is just the header.
        let h = FarbfeldHeader {
            width: 0,
            height: 0,
            body_len: 0,
        };
        assert_eq!(h.total_len().unwrap(), HEADER_LEN);
    }

    #[test]
    fn header_total_len_reports_overflow_without_panicking() {
        // Synthetic header whose body_len is at the very top of usize;
        // adding HEADER_LEN must fail with InvalidData, not panic.
        let h = FarbfeldHeader {
            width: 0xFFFF_FFFF,
            height: 0xFFFF_FFFF,
            body_len: usize::MAX,
        };
        let err = h.total_len().unwrap_err();
        let FarbfeldError::InvalidData(s) = err;
        assert!(s.contains("overflow"), "msg = {s:?}");
    }

    #[test]
    fn peek_farbfeld_dimensions_returns_same_as_parse_header() {
        // Build a 5×7 file but feed only its 16-byte header to the peek
        // — the result must match calling the underlying header parser.
        let mut buf = Vec::from(&b"farbfeld"[..]);
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.extend_from_slice(&7u32.to_be_bytes());
        // Append a fake (but byte-correctly-sized) body so we can prove
        // the peek does not depend on the body content.
        buf.extend_from_slice(&vec![0u8; 5 * 7 * BYTES_PER_PIXEL]);

        let h_full = peek_farbfeld_dimensions(&buf).unwrap();
        let h_prefix = peek_farbfeld_dimensions(&buf[..HEADER_LEN]).unwrap();
        assert_eq!(h_full, h_prefix);
        assert_eq!(h_full.width, 5);
        assert_eq!(h_full.height, 7);
        assert_eq!(h_full.body_len, 5 * 7 * BYTES_PER_PIXEL);
        assert_eq!(h_full.total_len().unwrap(), buf.len());
    }

    #[test]
    fn peek_farbfeld_dimensions_rejects_short_buffer() {
        // 15-byte buffer can't carry a 16-byte header.
        assert!(peek_farbfeld_dimensions(&[0u8; HEADER_LEN - 1]).is_err());
        // Empty buffer.
        assert!(peek_farbfeld_dimensions(&[]).is_err());
    }

    #[test]
    fn peek_farbfeld_dimensions_rejects_wrong_magic() {
        let mut buf = [0u8; HEADER_LEN];
        buf[..8].copy_from_slice(b"farbFELD");
        assert!(peek_farbfeld_dimensions(&buf).is_err());
    }

    #[test]
    fn peek_farbfeld_dimensions_accepts_body_announcing_header_only() {
        // 0×0 — well-formed header, no body needed; peek must accept
        // any input that starts with this 16-byte preamble.
        let mut buf = Vec::from(&b"farbfeld"[..]);
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        let h = peek_farbfeld_dimensions(&buf).unwrap();
        assert_eq!(h.width, 0);
        assert_eq!(h.height, 0);
        assert_eq!(h.body_len, 0);
        assert_eq!(h.total_len().unwrap(), HEADER_LEN);
    }

    #[test]
    fn peek_farbfeld_dimensions_does_not_validate_body_length() {
        // Announce 4×4 (= 128 body bytes) but provide only the 16-byte
        // header. `peek_farbfeld_dimensions` is by contract the
        // "look-but-don't-allocate" path: it must not reject this just
        // because the body is absent — that's `parse_farbfeld`'s job.
        let mut buf = Vec::from(&b"farbfeld"[..]);
        buf.extend_from_slice(&4u32.to_be_bytes());
        buf.extend_from_slice(&4u32.to_be_bytes());
        let h = peek_farbfeld_dimensions(&buf).unwrap();
        assert_eq!(h.width, 4);
        assert_eq!(h.height, 4);
        assert_eq!(h.body_len, 4 * 4 * BYTES_PER_PIXEL);
        // The whole-file parser, by contrast, must reject the same
        // input — the cross-check between announced size and bytes
        // present is the parser's contract.
        assert!(parse_farbfeld(&buf).is_err());
    }

    #[test]
    fn decode_be_samples_handles_unit_size_and_long_runs() {
        // Smallest non-empty: one sample.
        let mut out = [0u16; 1];
        decode_be_samples(&[0x12, 0x34], &mut out);
        assert_eq!(out, [0x1234]);

        // Long run — every u16 from 0..1024.
        let bytes: Vec<u8> = (0..1024u16).flat_map(|v| v.to_be_bytes()).collect();
        let mut out = vec![0u16; 1024];
        decode_be_samples(&bytes, &mut out);
        assert_eq!(out, (0..1024u16).collect::<Vec<_>>());
    }

    #[test]
    fn decode_be_samples_zero_length_is_a_noop() {
        // Empty body / empty output — both must be accepted without
        // panicking. (Zero-pixel farbfeld images take this branch.)
        let mut out: [u16; 0] = [];
        decode_be_samples(&[], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn decode_be_samples_caps_at_smaller_side_without_panic() {
        // Body has 4 bytes (= 2 u16s) but `out` has 4 slots. Per the
        // helper's contract the call only fills the lockstep prefix and
        // does NOT panic on the asymmetric case — leftover slots stay
        // at their pre-existing value, which keeps the helper crash-
        // safe in any future caller that violates the precondition.
        let mut out = [0xFFFFu16; 4];
        decode_be_samples(&[0x12, 0x34, 0x56, 0x78], &mut out);
        assert_eq!(out, [0x1234, 0x5678, 0xFFFF, 0xFFFF]);
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
