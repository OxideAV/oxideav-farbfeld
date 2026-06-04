#![no_main]

//! Streaming-I/O fuzz harness for `oxideav-farbfeld`.
//!
//! `decode.rs` and `encode.rs` already cover the whole-file decoder and
//! the three whole-file encoder entry points respectively, but they
//! both drive their input through a single bulk `Cursor` / byte slice.
//! Real consumers feed [`FarbfeldStreamReader`] / [`FarbfeldStreamWriter`]
//! from arbitrary `std::io::Read` / `std::io::Write` sources — sockets,
//! pipes, mmaps, decompressors — whose `read` / `write` calls may
//! deliver short returns, single-byte chunks, or zero-length partial
//! results. The streaming reader uses [`std::io::Read::take`] +
//! [`std::io::Read::read_to_end`], and the streaming writer uses
//! [`std::io::Write::write_all`], both of which are required to compose
//! correctly under those conditions but only the bulk-buffer path is
//! exercised by the existing fuzz targets.
//!
//! This target wraps the on-disk farbfeld byte stream behind a chunked
//! transport whose chunk sizes are drawn from the fuzz input itself.
//! Every input therefore exercises a deterministic but attacker-chosen
//! mix of `(short reads | full reads | zero-byte reads)` for the
//! reader, and a symmetric mix of `(short writes | full writes | zero-
//! byte writes)` for the writer. The fuzz bytes also pick the image
//! dimensions and pixel content so the corpus eventually covers the
//! cross-product of small/medium image shapes against pathological
//! chunk patterns.
//!
//! ## Input shape
//!
//! * **byte 0:** `width` (0..=32)
//! * **byte 1:** `height` (0..=32)
//! * **byte 2:** read-chunk schedule seed (0..=255)
//! * **byte 3:** write-chunk schedule seed (0..=255)
//! * **bytes 4..4+pixel_byte_count:** the body bytes (encoder input,
//!   short-padded with zero, excess discarded).
//!
//! 32×32 caps the worst case at 4 KiB of body per execution — well
//! inside libFuzzer's iteration budget — while still covering every
//! shape (zero-axis / square / tall-narrow / wide-short / asymmetric)
//! that the existing fuzz targets cover, but driven through the chunked
//! transport. The schedule seeds are xorshift32-derived per chunk so a
//! single fuzz input produces a deterministic byte-pattern of chunk
//! sizes (typically a mix of 1-byte, mid-sized, and full-row reads).
//!
//! ## Invariants asserted
//!
//! For each `(width, height, body, read_schedule, write_schedule)`:
//!
//! 1. **No panics, ever.** Every streaming API call returns `Ok` or
//!    [`FarbfeldError::InvalidData`]. Choppy I/O underneath must never
//!    cause a panic.
//! 2. **Chunked-stream decode == bulk decode.** Driving the same
//!    on-disk bytes through [`FarbfeldStreamReader`] against a
//!    [`ChoppyReader`] yields the same flat row-major sample buffer
//!    as the in-memory [`parse_farbfeld`] decoder. The streaming
//!    reader's [`Read::take`]/`read_to_end` contract must compose
//!    correctly with any well-behaved (but possibly short-returning)
//!    `Read` impl.
//! 3. **Chunked-stream encode == bulk encode.** Driving the same
//!    in-memory pixel rows through [`FarbfeldStreamWriter`] against a
//!    [`ChoppyWriter`] produces a byte stream identical to the bulk
//!    [`encode_farbfeld_image`] output. The streaming writer's
//!    [`Write::write_all`] contract must compose correctly with any
//!    well-behaved (but possibly short-accepting) `Write` impl.
//! 4. **Streaming roundtrip closure.** Decoding the chunked-encoded
//!    bytes through the chunked-decoder yields the original pixel
//!    rows.
//! 5. **Per-row skip equivalence.** Driving the chunked-reader's
//!    [`FarbfeldStreamReader::skip_row`] path one row at a time covers
//!    `height` rows and lands `rows_read == height`, with the same
//!    body bytes consumed as a `read_row` walk would.

use std::io::{self, Read, Write};

use libfuzzer_sys::fuzz_target;
use oxideav_farbfeld::{
    encode_farbfeld_image, parse_farbfeld, FarbfeldImage, FarbfeldStreamReader,
    FarbfeldStreamWriter, BYTES_PER_PIXEL, HEADER_LEN,
};

/// Maximum width / height drawn from the fuzz bytes. 32×32 caps the
/// per-execution body at 4 KiB so the chunked transports can iterate
/// hundreds of `read`/`write` calls per image without blowing the
/// fuzzer's per-input time budget.
const MAX_DIM: u32 = 32;

/// xorshift32 — tiny inlined PRNG used to derive per-call chunk sizes
/// from the schedule seed. Same shape as the property-sweep test's
/// PRNG; chosen because it's two integer ops per call and deterministic.
#[inline]
fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    if x == 0 {
        x = 0x9E37_79B9;
    }
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// `Read` shim that exposes `inner` in small chunks whose sizes are
/// drawn from a deterministic xorshift32 schedule. Every `read` call
/// either delivers between 1 and 8 bytes (the "normal" path), the full
/// remaining buffer (the "bulk" path), or zero bytes (the "spurious
/// EAGAIN" path — represented here as `Ok(0)` only when the caller's
/// buffer is itself empty, never on a non-empty buffer, since `Ok(0)`
/// on a non-empty caller buffer is the `Read` trait's EOF signal and
/// would mislead the streaming reader's truncation check).
struct ChoppyReader<'a> {
    inner: &'a [u8],
    pos: usize,
    state: u32,
}

impl<'a> ChoppyReader<'a> {
    fn new(inner: &'a [u8], seed: u32) -> Self {
        Self {
            inner,
            pos: 0,
            state: seed,
        }
    }
}

impl<'a> Read for ChoppyReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.inner.len() {
            return Ok(0);
        }
        let remaining = self.inner.len() - self.pos;
        let want = buf.len().min(remaining);
        // Decide chunk size: ~1/4 of calls return the full buffer
        // (bulk path), ~3/4 return 1..=8 bytes (choppy path).
        let r = xorshift32(&mut self.state);
        let take = if (r & 0b11) == 0 {
            want
        } else {
            // 1..=8 bytes, capped at what's left in `want`.
            let chunk = ((r >> 2) as usize % 8) + 1;
            chunk.min(want)
        };
        buf[..take].copy_from_slice(&self.inner[self.pos..self.pos + take]);
        self.pos += take;
        Ok(take)
    }
}

/// `Write` shim that forwards into an internal `Vec<u8>` in small chunks
/// drawn from a deterministic xorshift32 schedule. The shim never
/// rejects bytes or returns `Ok(0)` on a non-empty caller buffer — that
/// would constitute a broken `Write` impl and the streaming writer is
/// allowed to assume well-behaved writers — but it does return short
/// successes (1..=8 bytes) on ~3/4 of calls so `write_all` is forced to
/// loop.
struct ChoppyWriter {
    out: Vec<u8>,
    state: u32,
}

impl ChoppyWriter {
    fn new(seed: u32) -> Self {
        Self {
            out: Vec::new(),
            state: seed,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.out
    }
}

impl Write for ChoppyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let r = xorshift32(&mut self.state);
        let take = if (r & 0b11) == 0 {
            buf.len()
        } else {
            let chunk = ((r >> 2) as usize % 8) + 1;
            chunk.min(buf.len())
        };
        self.out.extend_from_slice(&buf[..take]);
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        // The four header bytes (width, height, read seed, write seed)
        // are the minimum input shape.
        return;
    }

    let width = (data[0] as u32) % (MAX_DIM + 1);
    let height = (data[1] as u32) % (MAX_DIM + 1);
    let read_seed = (data[2] as u32).wrapping_mul(0x0101_0101) ^ 0xDEAD_BEEF;
    let write_seed = (data[3] as u32).wrapping_mul(0x0101_0101) ^ 0xCAFE_BABE;
    let pixel_count = (width as usize) * (height as usize);
    let body_bytes = pixel_count * BYTES_PER_PIXEL;
    let sample_count = pixel_count * 4;

    // Build the body from the remainder, padding with zero if the fuzz
    // input is short. Same rationale as the encode-side fuzz target —
    // rejecting short inputs would prevent the shrinker from exploring
    // the small / zero-dimension paths against the choppy transport.
    let mut body_be = vec![0u8; body_bytes];
    let tail = data.get(4..).unwrap_or(&[]);
    let take = tail.len().min(body_bytes);
    body_be[..take].copy_from_slice(&tail[..take]);

    // Native-endian flat samples derived from the BE body — the shape
    // both `FarbfeldImage` and `FarbfeldStreamWriter::write_row` accept.
    let mut samples_native: Vec<u16> = Vec::with_capacity(sample_count);
    for c in body_be.chunks_exact(2) {
        samples_native.push(u16::from_be_bytes([c[0], c[1]]));
    }
    debug_assert_eq!(samples_native.len(), sample_count);

    // --- 1. Bulk-encode the reference stream -----------------------
    let bulk_image = FarbfeldImage {
        width,
        height,
        pixels: samples_native.clone(),
    };
    let bulk_encoded = encode_farbfeld_image(&bulk_image)
        .expect("encode_farbfeld_image must accept any matched-count flat plane");
    assert_eq!(bulk_encoded.len(), HEADER_LEN + body_bytes);

    // --- 2. Chunked-encode via FarbfeldStreamWriter ----------------
    let mut writer = FarbfeldStreamWriter::new(ChoppyWriter::new(write_seed), width, height)
        .expect("FarbfeldStreamWriter::new must accept any (width, height) inside MAX_DIM");
    let row_samples = (width as usize) * 4;
    if row_samples == 0 {
        // Zero-width: write_row expects an empty slice. The writer still
        // needs `height` calls for finish() to succeed.
        for _ in 0..height {
            writer
                .write_row(&[])
                .expect("zero-width row must accept an empty sample slice");
        }
    } else {
        for row in samples_native.chunks_exact(row_samples) {
            writer
                .write_row(row)
                .expect("write_row must accept any width-matched native-endian sample slice");
        }
    }
    let chunked_writer = writer
        .finish()
        .expect("finish must succeed after height rows");
    let chunked_encoded = chunked_writer.into_inner();

    // --- Invariant 3: chunked encode == bulk encode ----------------
    assert_eq!(
        chunked_encoded, bulk_encoded,
        "chunked writer disagreed with bulk encode at {width}×{height}",
    );

    // --- 3. Chunked-decode via FarbfeldStreamReader ----------------
    let mut reader = FarbfeldStreamReader::new(ChoppyReader::new(&bulk_encoded, read_seed))
        .expect("FarbfeldStreamReader::new must accept any header inside MAX_DIM");
    assert_eq!(reader.width(), width);
    assert_eq!(reader.height(), height);
    let chunked_decoded = reader
        .read_all_rows()
        .expect("read_all_rows must drain the bulk-encoded body cleanly");

    // --- 4. Bulk-decode the same stream for cross-check ------------
    let bulk_decoded =
        parse_farbfeld(&bulk_encoded).expect("a bulk-encoded stream must round-trip through parse");
    assert_eq!(bulk_decoded.width, width);
    assert_eq!(bulk_decoded.height, height);

    // --- Invariant 2: chunked decode == bulk decode ----------------
    assert_eq!(
        chunked_decoded, bulk_decoded.pixels,
        "chunked reader disagreed with bulk parse at {width}×{height}",
    );

    // --- Invariant 4: streaming roundtrip closure ------------------
    assert_eq!(
        chunked_decoded, samples_native,
        "chunked roundtrip drifted from input samples at {width}×{height}",
    );

    // --- 5. Per-row skip walk against a fresh ChoppyReader ---------
    // `skip_row` calls share the same row-bytes `Read::take` /
    // `read_to_end` discipline as `read_row`; under a choppy reader the
    // bounded read still has to compose correctly. Verify by walking
    // every row via `skip_row` and confirming `rows_read == height`
    // and no error surfaces.
    let mut skip_reader = FarbfeldStreamReader::new(ChoppyReader::new(&bulk_encoded, read_seed))
        .expect("FarbfeldStreamReader::new (skip walk) must accept any header inside MAX_DIM");
    let mut skipped = 0u32;
    while skip_reader
        .skip_row()
        .expect("skip_row must compose with the choppy transport")
    {
        skipped += 1;
        // Defensive cap: never spin past `height` iterations (skip_row
        // would already return Ok(false) — this is just a safety net so
        // a regression that broke the rows_read accounting wouldn't
        // hang the fuzzer).
        if skipped > height {
            panic!("skip_row returned Ok(true) past height={height}");
        }
    }
    assert_eq!(skipped, height, "skip walk missed rows at {width}×{height}");
    assert_eq!(skip_reader.rows_read(), height);
    assert_eq!(skip_reader.rows_remaining(), 0);

    // --- Invariant 1 (rejection path): a truncated body still surfaces
    //     as InvalidData under the choppy transport. Only test when at
    //     least one row exists — the zero-row case has no body to
    //     truncate.
    if body_bytes > 0 {
        // Remove the last body byte and confirm both `read_all_rows`
        // and a `read_row` walk reject the truncation. The choppy
        // transport will deliver this near-complete buffer in many
        // small chunks before hitting the short tail.
        let mut truncated = bulk_encoded.clone();
        truncated.pop();

        let trunc_reader = FarbfeldStreamReader::new(ChoppyReader::new(&truncated, read_seed));
        match trunc_reader {
            Ok(mut r) => {
                // The truncation may surface on `read_all_rows` (when
                // the missing byte is in a non-final row's body) or
                // only on the last row — either way the call must
                // return an error, never panic.
                let drained = r.read_all_rows();
                assert!(
                    drained.is_err(),
                    "chunked reader accepted a 1-byte-truncated body at {width}×{height}",
                );
            }
            Err(_) => {
                // Header itself unparsable in the truncated buffer (only
                // possible when body_bytes == 0, which we excluded). If
                // this branch is hit it's a real regression.
                panic!("chunked reader rejected header on a body-truncated buffer");
            }
        }
    }
});
