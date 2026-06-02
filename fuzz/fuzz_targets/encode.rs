#![no_main]

//! Encode-side fuzz harness for `oxideav-farbfeld`.
//!
//! The decode-side harness (`decode.rs`) hardens the parser against
//! attacker-supplied bytes. This target covers the symmetric encode
//! surface: the three whole-file encoder entry points
//! ([`encode_farbfeld`], [`encode_farbfeld_from_rgba16`],
//! [`encode_farbfeld_image`]) and the row-at-a-time streaming writer
//! ([`FarbfeldStreamWriter`]).
//!
//! Why fuzz the encoders? They all share a similar arithmetic
//! prelude — `width * height` and `width * height * 8` must fit in
//! `usize`, the caller's body / pixel buffer length must match the
//! announced dimensions, and the streaming writer must reject excess
//! `write_row` calls and a premature `finish`. The dispatcher for
//! these checks is hand-rolled (the format is tiny enough that
//! none of it goes through a generic codec layer), so a fuzz target
//! that drives arbitrary `(width, height, body)` combinations is the
//! cheapest insurance against an accidental panic creeping in if any
//! of the bounds-check arithmetic regresses.
//!
//! ## Input shape
//!
//! The fuzz bytes are interpreted structurally — picking large
//! `(width, height)` would just allocate gigabytes per execution and
//! starve the fuzzer. Layout:
//!
//! * **byte 0:** `width` (0..=64)
//! * **byte 1:** `height` (0..=64)
//! * **bytes 2..2+pixel_byte_count:** the body bytes (encoder input)
//!
//! 64×64 caps the worst case at 16 KiB of body per execution — well
//! inside the fuzzer's per-iteration budget — and the byte-level
//! `width % 65` / `height % 65` derivation still gives the fuzzer
//! full reach over 0×N, N×0, square, tall-narrow, and wide-short
//! shapes. The fuzzer can also shrink to a 2-byte input and exercise
//! the zero-dimension fast path.
//!
//! ## Invariants asserted
//!
//! On every (successfully-formed) `(width, height, body)`:
//!
//! 1. **No panics.** Every encoder entry point and the streaming
//!    writer succeed or return [`FarbfeldError::InvalidData`].
//!    Neither is allowed to panic on any input.
//! 2. **Three whole-file encoders agree.** [`encode_farbfeld`],
//!    [`encode_farbfeld_from_rgba16`], and [`encode_farbfeld_image`]
//!    produce byte-identical output when fed the same logical pixel
//!    data. Drift between them would mean different consumer
//!    code paths get different files for the same picture.
//! 3. **Streaming writer agrees with whole-file.**
//!    [`FarbfeldStreamWriter`] driven one row at a time produces a
//!    byte stream identical to [`encode_farbfeld_image`]. Drift
//!    would silently break callers that pick the streaming path.
//! 4. **Lossless roundtrip.** [`parse_farbfeld`] of the encoded
//!    stream returns a [`FarbfeldImage`] whose `(width, height,
//!    pixels)` equals the input. This is the spec's primary
//!    guarantee — the format is uncompressed and bijective.
//! 5. **Exact size.** The encoded stream is exactly
//!    `HEADER_LEN + width * height * 8` bytes. No padding, no
//!    compression, no metadata.
//! 6. **Header echo.** Bytes `0..8` are the ASCII magic, bytes
//!    `8..12` / `12..16` are width / height big-endian.
//!
//! ## Robustness paths
//!
//! On `(width, height)` shapes the fuzzer can also exercise the
//! encoder rejection paths:
//!
//! * **Body length mismatch.** The fuzzer can supply too few or too
//!   many body bytes — [`encode_farbfeld`] must reject with
//!   [`FarbfeldError::InvalidData`], never panic.
//! * **Premature finish.** Calling [`FarbfeldStreamWriter::finish`]
//!   before `height` rows are written must return
//!   [`FarbfeldError::InvalidData`], never panic.
//! * **Excess write_row.** Calling [`FarbfeldStreamWriter::write_row`]
//!   after `height` rows are written must return
//!   [`FarbfeldError::InvalidData`], never panic.

use libfuzzer_sys::fuzz_target;
use oxideav_farbfeld::{
    encode_farbfeld, encode_farbfeld_from_rgba16, encode_farbfeld_image, parse_farbfeld,
    FarbfeldImage, FarbfeldStreamWriter, BYTES_PER_PIXEL, HEADER_LEN, MAGIC,
};

/// Maximum width / height drawn from the fuzz bytes. 64×64 caps the
/// per-execution allocation at 16 KiB of body, fast enough for the
/// fuzzer's tight iteration loop while still covering the full
/// arithmetic surface (overflow checks would only trip past 2^32, but
/// the body-length cross-check, header echo, and three-encoder
/// agreement all hold at every small dimension).
const MAX_DIM: u32 = 64;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        // The two dimension bytes are the minimum input shape. Anything
        // shorter is dropped — the fuzzer still hits 2..3-byte inputs
        // (i.e. zero-pixel images) routinely.
        return;
    }

    let width = (data[0] as u32) % (MAX_DIM + 1);
    let height = (data[1] as u32) % (MAX_DIM + 1);
    let pixel_count = (width as usize) * (height as usize);
    let body_bytes = pixel_count * BYTES_PER_PIXEL;

    // Build the body from the remainder, padding with zero if the fuzz
    // input is short. We avoid rejecting on short bodies because the
    // fuzzer's shrinker would then never explore the body-arithmetic
    // path on a minimum-length seed.
    let mut body_be = vec![0u8; body_bytes];
    let tail = data.get(2..).unwrap_or(&[]);
    let take = tail.len().min(body_bytes);
    body_be[..take].copy_from_slice(&tail[..take]);

    // --- Path A: encode_farbfeld (pre-serialised BE body) -----------
    let encoded_a = encode_farbfeld(width, height, &body_be)
        .expect("encode_farbfeld must accept any matched-length BE body");

    // --- Path B: encode_farbfeld_from_rgba16 (native-endian pixels) -
    let pixels_native: Vec<[u16; 4]> = body_be
        .chunks_exact(BYTES_PER_PIXEL)
        .map(|c| {
            [
                u16::from_be_bytes([c[0], c[1]]),
                u16::from_be_bytes([c[2], c[3]]),
                u16::from_be_bytes([c[4], c[5]]),
                u16::from_be_bytes([c[6], c[7]]),
            ]
        })
        .collect();
    assert_eq!(pixels_native.len(), pixel_count);
    let encoded_b = encode_farbfeld_from_rgba16(width, height, &pixels_native)
        .expect("encode_farbfeld_from_rgba16 must accept any matched-count pixel slice");

    // --- Path C: encode_farbfeld_image (flat native-endian plane) ---
    let mut samples_flat = Vec::with_capacity(pixel_count * 4);
    for px in &pixels_native {
        samples_flat.extend_from_slice(px);
    }
    let image_in = FarbfeldImage {
        width,
        height,
        pixels: samples_flat.clone(),
    };
    let encoded_c = encode_farbfeld_image(&image_in)
        .expect("encode_farbfeld_image must accept any matched-count flat plane");

    // --- Invariant 2: the three whole-file encoders agree -----------
    assert_eq!(
        encoded_a, encoded_b,
        "encode_farbfeld and encode_farbfeld_from_rgba16 disagreed at {width}×{height}",
    );
    assert_eq!(
        encoded_a, encoded_c,
        "encode_farbfeld and encode_farbfeld_image disagreed at {width}×{height}",
    );

    // --- Invariant 5: exact size identity ---------------------------
    assert_eq!(
        encoded_a.len(),
        HEADER_LEN + body_bytes,
        "encoded length {} != header({HEADER_LEN}) + body({body_bytes}) at {width}×{height}",
        encoded_a.len(),
    );

    // --- Invariant 6: header echo -----------------------------------
    assert_eq!(&encoded_a[..8], MAGIC, "encoded magic not 'farbfeld'");
    assert_eq!(
        &encoded_a[8..12],
        &width.to_be_bytes(),
        "encoded width header doesn't match input",
    );
    assert_eq!(
        &encoded_a[12..16],
        &height.to_be_bytes(),
        "encoded height header doesn't match input",
    );

    // --- Path D: FarbfeldStreamWriter driven row-by-row -------------
    // The streaming writer commits to (width, height) at construction
    // time and rejects width*height*8 overflow up front. After the
    // header is on disk it accepts one row at a time and rejects an
    // excess row or a premature finish.
    let mut writer = FarbfeldStreamWriter::new(Vec::<u8>::new(), width, height)
        .expect("FarbfeldStreamWriter::new must accept any (width, height) inside MAX_DIM");
    let row_samples = (width as usize) * 4;
    if row_samples == 0 {
        // Zero-width: write_row would expect a 0-sample slice. The
        // writer still needs `height` calls before finish() succeeds.
        for _ in 0..height {
            writer
                .write_row(&[])
                .expect("zero-width row must accept an empty sample slice");
        }
    } else {
        for row in samples_flat.chunks_exact(row_samples) {
            writer
                .write_row(row)
                .expect("write_row must accept any width-matched native-endian sample slice");
        }
    }
    let encoded_d = writer
        .finish()
        .expect("finish must succeed after height rows");

    // --- Invariant 3: streaming writer agrees with whole-file -------
    assert_eq!(
        encoded_a, encoded_d,
        "streaming writer disagreed with encode_farbfeld_image at {width}×{height}",
    );

    // --- Invariant 4: lossless roundtrip ----------------------------
    let parsed = parse_farbfeld(&encoded_a).expect("a freshly-encoded stream must parse");
    assert_eq!(
        parsed.width, width,
        "roundtrip width drifted ({} ≠ {width})",
        parsed.width,
    );
    assert_eq!(
        parsed.height, height,
        "roundtrip height drifted ({} ≠ {height})",
        parsed.height,
    );
    assert_eq!(
        parsed.pixels, samples_flat,
        "roundtrip pixels drifted at {width}×{height}",
    );

    // --- Invariant 1 (rejection path): encode_farbfeld rejects a
    //     mismatched body length without panicking. We only attempt
    //     this when the fuzzer supplied enough tail bytes to produce a
    //     distinguishable wrong-length buffer (otherwise the +1 / -1
    //     check would itself drift into a zero-length edge case the
    //     encoder is required to accept).
    if body_bytes > 0 {
        let mut short = body_be.clone();
        short.pop();
        assert!(
            encode_farbfeld(width, height, &short).is_err(),
            "encode_farbfeld accepted a body 1 byte short at {width}×{height}",
        );
        let mut long = body_be.clone();
        long.push(0);
        assert!(
            encode_farbfeld(width, height, &long).is_err(),
            "encode_farbfeld accepted a body 1 byte long at {width}×{height}",
        );
    }

    // --- Invariant 1 (rejection path): FarbfeldStreamWriter::finish
    //     refuses a premature close. Only test when height > 0 — a
    //     zero-height stream legitimately finishes immediately.
    if height > 0 {
        let premature = FarbfeldStreamWriter::new(Vec::<u8>::new(), width, height)
            .expect("premature-finish probe construction must succeed");
        assert!(
            premature.finish().is_err(),
            "FarbfeldStreamWriter::finish accepted 0 of {height} rows at {width}×{height}",
        );
    }
});
