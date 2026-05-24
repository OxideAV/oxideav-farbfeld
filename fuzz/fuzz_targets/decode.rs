#![no_main]

//! Decode-hardening fuzz harness for `oxideav-farbfeld`.
//!
//! farbfeld's entire on-disk format is an 8-byte ASCII magic, two
//! big-endian `u32` dimensions, and `width * height` pixels of four
//! big-endian `u16` samples (R, G, B, A). There is no compression,
//! palette, or per-pixel metadata, and no canonical system library
//! worth dlopen-ing to cross-decode against — so this target fuzzes the
//! parser directly against arbitrary attacker bytes.
//!
//! The decode entry points must **never panic** on any input. The
//! classic danger is the `width * height * 8` body-size computation: a
//! 16-byte file can announce `width = height = 0x10000` and trick a
//! naive parser into allocating tens of gigabytes (or wrapping the
//! multiplication and under-allocating). [`parse_farbfeld`] guards both
//! by using `checked_mul` and cross-checking the announced body length
//! against the bytes actually present *before* allocating; this harness
//! is the fuzz-side proof of that contract.
//!
//! Surfaces exercised on every input:
//!
//! * [`parse_farbfeld`] — the whole-file decoder (the primary surface).
//! * [`parse_farbfeld_header`] — the look-but-don't-allocate header peek.
//! * [`FarbfeldStreamReader`] — the row-at-a-time `io::Read` decoder,
//!   driven over the same bytes; its result must agree with the
//!   whole-file decoder on whether the input is valid.
//!
//! Roundtrip invariants checked on inputs that *do* parse:
//!
//! * `pixels.len() == width * height * 4` (the [`FarbfeldImage`]
//!   invariant);
//! * re-encoding the decoded image reproduces the original bytes exactly
//!   (farbfeld is lossless and has exactly one valid byte serialisation
//!   per image);
//! * the streaming decoder yields the same flat sample buffer as the
//!   whole-file decoder.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use oxideav_farbfeld::{
    encode_farbfeld_from_rgba16, parse_farbfeld, parse_farbfeld_header, FarbfeldStreamReader,
    HEADER_LEN,
};

fuzz_target!(|data: &[u8]| {
    // 1. The header peek must never panic and must never allocate the
    //    announced body (it only inspects the first 16 bytes).
    let header = parse_farbfeld_header(data);

    // 2. The whole-file decoder must never panic on arbitrary bytes.
    let parsed = parse_farbfeld(data);

    // 3. The streaming reader, driven over the identical bytes, must
    //    reach the same accept/reject verdict as the whole-file decoder.
    let streamed = stream_decode(data);
    match (&parsed, &streamed) {
        (Ok(img), Ok(rows)) => {
            assert_eq!(
                &img.pixels, rows,
                "stream decode disagreed with whole-file decode on a valid input"
            );
        }
        (Err(_), Err(_)) => {}
        (Ok(_), Err(e)) => panic!("whole-file decode accepted but stream decode rejected: {e}"),
        (Err(e), Ok(_)) => panic!("whole-file decode rejected but stream decode accepted: {e}"),
    }

    let Ok(img) = parsed else {
        // Rejected input: the header peek, if it succeeded, must not
        // have promised a body that the file actually satisfies — but
        // a header-only buffer (exactly 16 bytes, 0×0) is the one case
        // where the header peek and a successful parse coincide. Beyond
        // the no-panic guarantee there is nothing more to assert on a
        // rejected stream.
        let _ = header;
        return;
    };

    // The decoded image must honour the flat-buffer invariant.
    let expected_samples = (img.width as usize)
        .checked_mul(img.height as usize)
        .and_then(|n| n.checked_mul(4))
        .expect("a successfully-parsed image cannot overflow its own sample count");
    assert_eq!(
        img.pixels.len(),
        expected_samples,
        "parsed FarbfeldImage violated pixels.len() == width*height*4 ({}x{})",
        img.width,
        img.height,
    );

    // A successful parse implies the header peek also succeeded and
    // reported the matching dimensions / body length.
    let hdr = header.expect("header peek must succeed whenever the whole-file parse does");
    assert_eq!(hdr.width, img.width, "header width disagreed with parse");
    assert_eq!(hdr.height, img.height, "header height disagreed with parse");
    assert_eq!(
        hdr.body_len,
        expected_samples * 2,
        "header body_len disagreed with the decoded sample count",
    );
    assert_eq!(
        data.len(),
        HEADER_LEN + hdr.body_len,
        "a valid farbfeld file is exactly header + body bytes",
    );

    // farbfeld is lossless with exactly one valid serialisation per
    // image, so re-encoding the decoded pixels must reproduce the input
    // byte-for-byte.
    let rgba: Vec<[u16; 4]> = img
        .pixels
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    let reencoded = encode_farbfeld_from_rgba16(img.width, img.height, &rgba)
        .expect("re-encoding a decoded image must succeed");
    assert_eq!(
        reencoded.as_slice(),
        data,
        "decode->encode roundtrip was not byte-identical",
    );
});

/// Decode `data` through the row-at-a-time streaming reader, returning
/// the same flat row-major `Vec<u16>` the whole-file decoder produces,
/// or an error string if the stream rejects the input.
fn stream_decode(data: &[u8]) -> Result<Vec<u16>, String> {
    let mut reader = FarbfeldStreamReader::new(Cursor::new(data)).map_err(|e| e.to_string())?;
    let rows = reader.read_all_rows().map_err(|e| e.to_string())?;
    // The whole-file decoder rejects trailing garbage; the streaming
    // reader stops at the announced body end, so mirror that strictness
    // here by insisting the cursor consumed every byte.
    let consumed = HEADER_LEN + (reader.width() as usize) * (reader.height() as usize) * 8;
    if consumed != data.len() {
        return Err(format!(
            "stream: trailing bytes — consumed {consumed} of {}",
            data.len()
        ));
    }
    Ok(rows)
}
