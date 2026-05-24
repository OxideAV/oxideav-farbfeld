//! DoS-hardening regression tests.
//!
//! `parse_farbfeld` cross-checks `width * height * 8` against the
//! actual body byte count BEFORE allocating the decoded pixel buffer.
//! Without that ordering, a 16-byte attacker-supplied file could
//! announce `width = height = 0x10000` (= 32 GiB of pixels) and force
//! the parser to allocate the announced capacity before discovering
//! the body is missing.
//!
//! These tests are not about decoding pathological images — they're
//! about confirming the parser refuses the *intent* without first
//! allocating the announced-but-absent body.

use oxideav_farbfeld::{
    parse_farbfeld, parse_farbfeld_header, FarbfeldError, FarbfeldStreamReader,
};
use std::io::Cursor;

/// Build a 16-byte buffer carrying the magic + announced (w, h) with
/// no body bytes following it.
fn header_only(width: u32, height: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    buf.extend_from_slice(b"farbfeld");
    buf.extend_from_slice(&width.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
    buf
}

#[test]
fn parser_refuses_huge_header_announcement_without_allocating() {
    // 16-byte attacker file announces 65,535 × 65,535 pixels.
    // body_len = 65_535 * 65_535 * 8 ~= 34 GB. If the parser allocated
    // first and checked second, this test would either OOM or run for
    // many seconds; we expect a fast `InvalidData` instead.
    let buf = header_only(65_535, 65_535);
    let t = std::time::Instant::now();
    let err = parse_farbfeld(&buf).expect_err("must refuse");
    let dt = t.elapsed();
    let FarbfeldError::InvalidData(msg) = err;
    assert!(msg.contains("body size mismatch"), "msg = {msg:?}");
    // Sanity: the rejection should be near-instant. Pick a generous
    // wall-clock budget to avoid flake under CI load while still
    // catching a regression that allocates gigabytes.
    assert!(
        dt < std::time::Duration::from_millis(500),
        "parser took {dt:?} — should reject crafted header in microseconds",
    );
}

#[test]
fn parser_refuses_overflow_dimensions_on_64bit() {
    // 64-bit hosts: u32::MAX * u32::MAX fits in usize, but
    // u32::MAX * u32::MAX * 8 = 1.474 * 10^20 — overflows usize.
    // We want the explicit overflow error message, not a body-size
    // mismatch. On 32-bit hosts the multiplication itself overflows
    // earlier; either way the parser must refuse.
    let buf = header_only(u32::MAX, u32::MAX);
    let err = parse_farbfeld(&buf).expect_err("must refuse");
    let FarbfeldError::InvalidData(msg) = err;
    assert!(msg.contains("overflow"), "msg = {msg:?}");
}

#[test]
fn header_only_decoder_does_not_allocate_body() {
    // parse_farbfeld_header is the "look but don't touch" entry point.
    // It must succeed on a header-only buffer even for huge dimensions
    // (callers use the returned body_len to make a sandbox decision).
    let buf = header_only(1_000, 1_000);
    let h = parse_farbfeld_header(&buf).expect("header parses");
    assert_eq!(h.width, 1_000);
    assert_eq!(h.height, 1_000);
    assert_eq!(h.body_len, 1_000 * 1_000 * 8);
}

#[test]
fn header_decoder_still_validates_magic() {
    let mut buf = vec![0u8; 16];
    buf[..8].copy_from_slice(b"NOTFARBF");
    assert!(parse_farbfeld_header(&buf).is_err());
}

#[test]
fn header_decoder_rejects_overflow_dimensions() {
    let buf = header_only(u32::MAX, u32::MAX);
    let err = parse_farbfeld_header(&buf).expect_err("overflows usize");
    let FarbfeldError::InvalidData(msg) = err;
    assert!(msg.contains("overflow"), "msg = {msg:?}");
}

#[test]
fn stream_read_all_rows_refuses_huge_header_without_allocating() {
    // 16-byte attacker file announces 65,536 × 65,536 pixels = ~34 GB of
    // u16 samples. `read_all_rows` must NOT pre-allocate the announced
    // sample count: it grows the output buffer one row at a time, so the
    // missing body is caught on the very first short row read having
    // allocated only a single row's worth (256 KiB) of scratch. If it
    // pre-allocated the announced size first, this would OOM or run for
    // seconds; we expect a fast `InvalidData` (truncated body) instead.
    let buf = header_only(65_536, 65_536);
    let mut reader = FarbfeldStreamReader::new(Cursor::new(buf)).expect("header parses");
    let t = std::time::Instant::now();
    let err = reader
        .read_all_rows()
        .expect_err("must refuse: body absent");
    let dt = t.elapsed();
    let FarbfeldError::InvalidData(msg) = err;
    assert!(
        msg.contains("truncated") || msg.contains("short"),
        "msg = {msg:?}"
    );
    assert!(
        dt < std::time::Duration::from_millis(500),
        "read_all_rows took {dt:?} — should reject crafted header without allocating the body",
    );
}

#[test]
fn stream_read_all_rows_zero_width_huge_height_returns_fast() {
    // width = 0, height = u32::MAX: the body is empty (0 bytes per row)
    // regardless of height, so `read_all_rows` must short-circuit rather
    // than spin ~4.3 billion empty per-row iterations (a CPU-time DoS).
    let buf = header_only(0, u32::MAX);
    let mut reader = FarbfeldStreamReader::new(Cursor::new(buf)).expect("header parses");
    let t = std::time::Instant::now();
    let rows = reader
        .read_all_rows()
        .expect("zero-width body is empty and valid");
    let dt = t.elapsed();
    assert!(rows.is_empty(), "zero-width image has no samples");
    assert_eq!(reader.rows_read(), u32::MAX, "all rows accounted for");
    assert!(
        dt < std::time::Duration::from_millis(500),
        "read_all_rows took {dt:?} — zero-width must short-circuit, not loop height times",
    );
}

#[test]
fn stream_read_all_rows_overflow_dimensions_refused() {
    // u32::MAX × u32::MAX × 4 overflows usize — explicit overflow error,
    // never a panic or an allocation attempt.
    let buf = header_only(u32::MAX, u32::MAX);
    let mut reader = FarbfeldStreamReader::new(Cursor::new(buf));
    // Construction may already reject (row size overflow); if it builds,
    // read_all_rows must reject on the sample-count overflow.
    if let Ok(reader) = reader.as_mut() {
        let err = reader
            .read_all_rows()
            .expect_err("overflow must be refused");
        let FarbfeldError::InvalidData(msg) = err;
        assert!(msg.contains("overflow"), "msg = {msg:?}");
    }
}

#[test]
fn empty_input_short_header_rejected_fast() {
    let t = std::time::Instant::now();
    assert!(parse_farbfeld(&[]).is_err());
    assert!(parse_farbfeld(b"farbfeld\x00").is_err());
    assert!(parse_farbfeld(b"farbfeld\x00\x00\x00\x00\x00\x00").is_err());
    let dt = t.elapsed();
    assert!(dt < std::time::Duration::from_millis(50));
}
