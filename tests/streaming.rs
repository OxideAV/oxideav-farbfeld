//! Integration tests for the row-at-a-time streaming reader/writer.
//!
//! Exercises the public `FarbfeldStreamReader` / `FarbfeldStreamWriter`
//! shape with realistic image sizes and confirms the streaming path
//! produces bytes identical to the in-memory `encode_farbfeld_*` path
//! and the `parse_farbfeld` decoder.

use std::io::Cursor;

use oxideav_farbfeld::{
    encode_farbfeld_from_rgba16, parse_farbfeld, FarbfeldStreamReader, FarbfeldStreamWriter,
};

/// Synthesise a deterministic flat RGBA u16 sample buffer of size
/// width × height × 4.
fn make_samples(width: u32, height: u32) -> Vec<u16> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            out.push((i.wrapping_mul(0x0123) & 0xFFFF) as u16);
            out.push((i.wrapping_mul(0x4567) & 0xFFFF) as u16);
            out.push((i.wrapping_mul(0x89AB) & 0xFFFF) as u16);
            out.push((i.wrapping_mul(0xCDEF) & 0xFFFF) as u16);
        }
    }
    out
}

#[test]
fn streaming_writer_output_equals_in_memory_encoder_output() {
    for &(w, h) in &[(1u32, 1u32), (1, 16), (16, 1), (4, 4), (33, 17), (128, 64)] {
        let samples = make_samples(w, h);

        // Streaming side.
        let mut writer = FarbfeldStreamWriter::new(Vec::new(), w, h).unwrap();
        for y in 0..h {
            let off = (y * w * 4) as usize;
            writer
                .write_row(&samples[off..off + (w * 4) as usize])
                .unwrap();
        }
        let streamed = writer.finish().unwrap();

        // In-memory side.
        let mut pixel_pairs: Vec<[u16; 4]> = Vec::with_capacity((w * h) as usize);
        for chunk in samples.chunks_exact(4) {
            pixel_pairs.push([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        let in_memory = encode_farbfeld_from_rgba16(w, h, &pixel_pairs).unwrap();

        assert_eq!(streamed, in_memory, "streamed != in-memory for {w}×{h}");
    }
}

#[test]
fn streaming_reader_output_equals_in_memory_parser_output() {
    for &(w, h) in &[(1u32, 1u32), (3, 5), (16, 16), (64, 33)] {
        let samples = make_samples(w, h);
        let mut pixel_pairs: Vec<[u16; 4]> = Vec::with_capacity((w * h) as usize);
        for chunk in samples.chunks_exact(4) {
            pixel_pairs.push([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        let bytes = encode_farbfeld_from_rgba16(w, h, &pixel_pairs).unwrap();

        // Streaming side.
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(reader.width(), w);
        assert_eq!(reader.height(), h);
        let streamed = reader.read_all_rows().unwrap();

        // In-memory side.
        let parsed = parse_farbfeld(&bytes).unwrap();

        assert_eq!(streamed, parsed.pixels, "streamed != parsed for {w}×{h}");
    }
}

#[test]
fn streaming_roundtrip_through_writer_then_reader() {
    // Write 16×16 streaming, then re-read it streaming, then compare.
    let w = 16u32;
    let h = 16u32;
    let samples = make_samples(w, h);

    let mut writer = FarbfeldStreamWriter::new(Vec::new(), w, h).unwrap();
    for y in 0..h {
        let off = (y * w * 4) as usize;
        writer
            .write_row(&samples[off..off + (w * 4) as usize])
            .unwrap();
    }
    let bytes = writer.finish().unwrap();

    let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes.clone())).unwrap();
    let mut buf = vec![0u16; (w * 4) as usize];
    let mut got = Vec::new();
    while reader.read_row(&mut buf).unwrap() {
        got.extend_from_slice(&buf);
    }
    assert_eq!(got, samples);

    // And bytes-on-disk must match the in-memory encoder.
    let mut pairs: Vec<[u16; 4]> = Vec::with_capacity((w * h) as usize);
    for chunk in samples.chunks_exact(4) {
        pairs.push([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    let in_mem = encode_farbfeld_from_rgba16(w, h, &pairs).unwrap();
    assert_eq!(bytes, in_mem);
}

#[test]
fn streaming_reader_truncated_at_first_row_is_an_error() {
    // 2×2 image (= 64 body bytes) but the input stops at the header.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"farbfeld");
    bytes.extend_from_slice(&2u32.to_be_bytes());
    bytes.extend_from_slice(&2u32.to_be_bytes());
    let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
    let mut row = vec![0u16; 8];
    assert!(reader.read_row(&mut row).is_err());
}

#[test]
fn streaming_reader_skip_rows_then_read_matches_in_memory_tail() {
    // Build a 32×16 image (2 048 samples), then with the streaming
    // reader skip the first 9 rows and read the remaining 7. The
    // pixel buffer must equal the corresponding tail of `parse_farbfeld`.
    let w = 32u32;
    let h = 16u32;
    let samples = make_samples(w, h);
    let mut pairs: Vec<[u16; 4]> = Vec::with_capacity((w * h) as usize);
    for chunk in samples.chunks_exact(4) {
        pairs.push([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    let bytes = encode_farbfeld_from_rgba16(w, h, &pairs).unwrap();
    let full = parse_farbfeld(&bytes).unwrap();

    let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
    let skipped = reader.skip_rows(9).unwrap();
    assert_eq!(skipped, 9);
    assert_eq!(reader.rows_read(), 9);

    // Drain the remaining 7 rows with read_row (NOT read_all_rows) to
    // exercise the per-row mix path.
    let row_samples = (w * 4) as usize;
    let mut tail: Vec<u16> = Vec::with_capacity(row_samples * 7);
    let mut row = vec![0u16; row_samples];
    while reader.read_row(&mut row).unwrap() {
        tail.extend_from_slice(&row);
    }
    assert_eq!(tail, full.pixels[row_samples * 9..]);
}

#[test]
fn streaming_reader_extra_trailing_bytes_are_observable_via_into_inner() {
    // The streaming reader doesn't reject trailing bytes (that's the
    // streaming contract — it only knows about the announced body).
    // Callers who care can drain the inner reader.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"farbfeld");
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&[0u8; 8]); // exactly one pixel
    bytes.push(0xFF); // trailing garbage

    let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
    let mut row = vec![0u16; 4];
    assert!(reader.read_row(&mut row).unwrap());
    assert!(!reader.read_row(&mut row).unwrap());
    // Now look at the inner cursor — header (16) + 1 row (8) = 24
    // bytes consumed; the cursor still holds 25 total bytes (the 24
    // farbfeld bytes plus our 1 byte of garbage).
    let inner = reader.into_inner();
    assert_eq!(inner.position(), 24);
    assert_eq!(inner.get_ref().len(), 25);
    // And the trailing byte is still readable.
    assert_eq!(inner.get_ref()[24], 0xFF);
}
