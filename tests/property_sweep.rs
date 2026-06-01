//! Property-style sweep over the farbfeld encode / decode / streaming
//! surface.
//!
//! Round 205 (depth-mode property tests): the crate is feature-complete
//! against the byte-layout description in
//! `docs/image/farbfeld/farbfeld-format.md`, has unit tests, a streaming
//! reader/writer pair, DoS-hardening regressions, a `cargo-fuzz` decode
//! target, an opaque-process cross-validator, and a Criterion bench
//! suite. Per the workspace "saturated → fuzz / bench / profile /
//! property" convention this round adds a deterministic property-style
//! sweep that exercises hundreds of pseudo-random `(width, height,
//! pixels)` triples per scenario and asserts the semantic invariants
//! the spec mandates, *across* the whole input space rather than at the
//! hand-picked points the unit tests cover.
//!
//! The sweep avoids introducing a new dev-dep (no proptest /
//! quickcheck) and instead uses a deterministic xorshift32 PRNG seeded
//! per scenario, so any failure is reproducible from the seed printed
//! in the assertion message and the test stays offline / no-net /
//! no-extra-build-cost.
//!
//! ## Invariants under test
//!
//! For every randomly generated `(width, height, pixels)`:
//!
//! 1. **Lossless roundtrip.** `parse_farbfeld(encode_*(w, h, px))`
//!    returns an `Ok(FarbfeldImage)` whose `(width, height, pixels)`
//!    equals the input. This is the spec's primary guarantee — the
//!    format is uncompressed and bit-exact.
//!
//! 2. **Exact size.** The encoded stream is exactly `16 + w*h*8` bytes
//!    (no compression, no padding, no metadata). Anything else would
//!    break the spec's fixed total-length identity.
//!
//! 3. **Header bytes echo input.** Bytes `0..8` are the ASCII magic
//!    `farbfeld`, bytes `8..12` / `12..16` are width / height
//!    big-endian. These are structural and must hold for every well-
//!    formed input.
//!
//! 4. **Encoder determinism.** Encoding the same input twice produces
//!    byte-identical output. There is no `HashMap` iteration order or
//!    wall-clock state in the codec; this catches accidental
//!    introductions.
//!
//! 5. **Three encoder entry points agree.** `encode_farbfeld_image`,
//!    `encode_farbfeld_from_rgba16`, and `encode_farbfeld` (with the
//!    pixels pre-serialised to big-endian) all produce byte-identical
//!    output for the same logical pixel data. Drift between them
//!    would mean the framework consumer and the standalone consumer
//!    are getting different files.
//!
//! 6. **Streaming agrees with whole-file.** `FarbfeldStreamReader` over
//!    the encoded bytes yields a flat sample buffer identical to
//!    `parse_farbfeld(...).pixels`, and `FarbfeldStreamWriter` driven
//!    one row at a time produces a byte stream identical to
//!    `encode_farbfeld_image`. Drift between the two API shapes would
//!    silently break callers picking either path.
//!
//! 7. **Idempotent re-encode.** `encode(decode(encode(px))) ==
//!    encode(px)` byte-for-byte. The format is bijective per the spec
//!    so this is by construction, but it's the cheapest way to spot a
//!    regression that introduced a non-canonical sample reordering.
//!
//! 8. **Header peek matches whole-file decode.** `parse_farbfeld_header`
//!    on the first 16 bytes reports the same `(width, height)` and a
//!    `body_len` matching `pixels.len() * 2`. Drift would mean an
//!    early-reject sandbox couldn't trust the cheap peek path.
//!
//! Each invariant runs across multiple seeds and shape distributions
//! (tiny widths, square images, zero dimensions, tall+narrow,
//! wide+short) so the sweep covers both the per-pixel arithmetic and
//! the header/dimension handling in the same pass.

use std::io::Cursor;

use oxideav_farbfeld::{
    encode_farbfeld, encode_farbfeld_from_rgba16, encode_farbfeld_image, parse_farbfeld,
    parse_farbfeld_header, FarbfeldImage, FarbfeldStreamReader, FarbfeldStreamWriter,
    BYTES_PER_PIXEL, HEADER_LEN, MAGIC,
};

// ---------------------------------------------------------------------------
// Deterministic PRNG
// ---------------------------------------------------------------------------

/// Minimal xorshift32 PRNG — same family the bench inputs use, kept
/// inline so the sweep stays offline / no-extra-dep.
struct XorShift32 {
    state: u32,
}

impl XorShift32 {
    fn new(seed: u32) -> Self {
        // xorshift32 with a 0 state is a fixed point at 0; the caller
        // never picks 0 but defend against it.
        Self {
            state: if seed == 0 { 0x1234_5678 } else { seed },
        }
    }

    fn next_u32(&mut self) -> u32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 17;
        self.state ^= self.state << 5;
        self.state
    }

    fn next_u16(&mut self) -> u16 {
        self.next_u32() as u16
    }

    /// Uniform integer in `0..=max`, inclusive.
    fn next_in(&mut self, max: u32) -> u32 {
        if max == u32::MAX {
            self.next_u32()
        } else {
            self.next_u32() % (max + 1)
        }
    }
}

// ---------------------------------------------------------------------------
// Shape distributions
// ---------------------------------------------------------------------------

/// A `(width, height)` distribution. Each variant picks dimensions from
/// a different region of the input space so the sweep exercises both
/// the typical-tile path and the off-axis edge cases.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// 1..=8 in each dimension — the per-pixel arithmetic gets the most
    /// scrutiny here because the whole image fits in a few cache lines
    /// and any sample-misalignment bug pops out immediately.
    Tiny,
    /// Square `n × n` with `n ∈ 1..=24` — exercises row-major scan
    /// order when row and column extents are equal.
    Square,
    /// Tall and narrow: `w ∈ 1..=4`, `h ∈ 1..=64`. Catches off-by-row
    /// bugs in row-major iteration.
    TallNarrow,
    /// Wide and short: `w ∈ 1..=64`, `h ∈ 1..=4`. The complement of
    /// `TallNarrow` — catches the symmetric off-by-column bug.
    WideShort,
    /// Zero dimension on one or both axes. The spec admits zero-area
    /// images (header-only files); the encoder must produce a 16-byte
    /// file and the streaming reader/writer must round-trip them.
    ZeroAxis,
    /// Medium 24..=48 in each dimension — exercises the multi-KB-body
    /// path where the per-row scratch buffer reuse in the streaming
    /// reader matters.
    Medium,
}

impl Shape {
    fn pick(self, rng: &mut XorShift32) -> (u32, u32) {
        match self {
            Shape::Tiny => (rng.next_in(7) + 1, rng.next_in(7) + 1),
            Shape::Square => {
                let n = rng.next_in(23) + 1;
                (n, n)
            }
            Shape::TallNarrow => (rng.next_in(3) + 1, rng.next_in(63) + 1),
            Shape::WideShort => (rng.next_in(63) + 1, rng.next_in(3) + 1),
            Shape::ZeroAxis => {
                // 4 sub-cases: 0×0, 0×k, k×0, equally likely.
                match rng.next_in(3) {
                    0 => (0, 0),
                    1 => (0, rng.next_in(31) + 1),
                    2 => (rng.next_in(31) + 1, 0),
                    _ => (0, 0),
                }
            }
            Shape::Medium => (rng.next_in(24) + 24, rng.next_in(24) + 24),
        }
    }
}

// ---------------------------------------------------------------------------
// Pixel generation
// ---------------------------------------------------------------------------

/// Build `width * height` random `[R, G, B, A] u16` pixels, spreading
/// values across the full `0..=65535` range so byte-order bugs are
/// visible.
fn random_pixels(width: u32, height: u32, rng: &mut XorShift32) -> Vec<[u16; 4]> {
    let n = (width as usize) * (height as usize);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push([
            rng.next_u16(),
            rng.next_u16(),
            rng.next_u16(),
            rng.next_u16(),
        ]);
    }
    out
}

/// Pack the per-pixel `[u16; 4]` representation into the flat row-major
/// `Vec<u16>` that `FarbfeldImage` carries.
fn flatten(pixels: &[[u16; 4]]) -> Vec<u16> {
    let mut out = Vec::with_capacity(pixels.len() * 4);
    for px in pixels {
        out.extend_from_slice(px);
    }
    out
}

/// Pack the per-pixel `[u16; 4]` representation into a pre-serialised
/// big-endian byte body suitable for `encode_farbfeld`.
fn flatten_be(pixels: &[[u16; 4]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() * BYTES_PER_PIXEL);
    for px in pixels {
        for &c in px {
            out.extend_from_slice(&c.to_be_bytes());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Invariant checks (one per pair, run on every sample)
// ---------------------------------------------------------------------------

/// Assert all seven (or eight, including the header-peek) invariants on
/// one `(width, height, pixels)` triple. Returns nothing — panics with
/// the seed in the message on first failure so the caller can replay.
fn check_all_invariants(seed: u32, label: &str, width: u32, height: u32, pixels: &[[u16; 4]]) {
    let flat = flatten(pixels);
    let body_be = flatten_be(pixels);

    // (1) + (3) + (5) — encode_farbfeld_image is the canonical path.
    let encoded_image = encode_farbfeld_image(&FarbfeldImage {
        width,
        height,
        pixels: flat.clone(),
    })
    .unwrap_or_else(|e| {
        panic!("seed={seed} label={label} encode_farbfeld_image failed on {width}×{height}: {e}")
    });
    // (5a) encode_farbfeld_from_rgba16 must agree.
    let encoded_rgba16 = encode_farbfeld_from_rgba16(width, height, pixels).unwrap_or_else(|e| {
        panic!(
            "seed={seed} label={label} encode_farbfeld_from_rgba16 failed on {width}×{height}: {e}"
        )
    });
    assert_eq!(
        encoded_image, encoded_rgba16,
        "seed={seed} label={label}: encode_farbfeld_image vs encode_farbfeld_from_rgba16 disagree at {width}×{height}",
    );
    // (5b) encode_farbfeld (raw BE body in) must agree.
    let encoded_raw = encode_farbfeld(width, height, &body_be).unwrap_or_else(|e| {
        panic!("seed={seed} label={label} encode_farbfeld failed on {width}×{height}: {e}")
    });
    assert_eq!(
        encoded_image, encoded_raw,
        "seed={seed} label={label}: encode_farbfeld_image vs encode_farbfeld (raw-body) disagree at {width}×{height}",
    );

    // (2) Exact size = 16 + w*h*8.
    let expected_len = HEADER_LEN + (width as usize) * (height as usize) * BYTES_PER_PIXEL;
    assert_eq!(
        encoded_image.len(),
        expected_len,
        "seed={seed} label={label}: encoded length {} != 16 + w*h*8 = {expected_len} for {width}×{height}",
        encoded_image.len(),
    );

    // (3) Header bytes literally echo input.
    assert_eq!(
        &encoded_image[..8],
        MAGIC,
        "seed={seed} label={label}: magic mismatch at {width}×{height}",
    );
    assert_eq!(
        &encoded_image[8..12],
        &width.to_be_bytes()[..],
        "seed={seed} label={label}: width bytes mismatch at {width}×{height}",
    );
    assert_eq!(
        &encoded_image[12..16],
        &height.to_be_bytes()[..],
        "seed={seed} label={label}: height bytes mismatch at {width}×{height}",
    );

    // (4) Encoder determinism — encoding twice must be byte-identical.
    let encoded_again = encode_farbfeld_image(&FarbfeldImage {
        width,
        height,
        pixels: flat.clone(),
    })
    .expect("second encode must succeed on the same input");
    assert_eq!(
        encoded_image, encoded_again,
        "seed={seed} label={label}: encoder is not deterministic at {width}×{height}",
    );

    // (1) + (7) Lossless roundtrip + idempotent re-encode.
    let decoded = parse_farbfeld(&encoded_image).unwrap_or_else(|e| {
        panic!("seed={seed} label={label} parse_farbfeld rejected our own output at {width}×{height}: {e}")
    });
    assert_eq!(
        decoded.width, width,
        "seed={seed} label={label}: decoded width mismatch",
    );
    assert_eq!(
        decoded.height, height,
        "seed={seed} label={label}: decoded height mismatch",
    );
    assert_eq!(
        decoded.pixels, flat,
        "seed={seed} label={label}: roundtrip mutated pixels at {width}×{height}",
    );

    let re_encoded = encode_farbfeld_image(&decoded).expect("re-encode of decoded image succeeds");
    assert_eq!(
        re_encoded, encoded_image,
        "seed={seed} label={label}: encode(decode(encode())) is not idempotent at {width}×{height}",
    );

    // (8) Header peek matches whole-file decode.
    let peek = parse_farbfeld_header(&encoded_image[..HEADER_LEN])
        .expect("header peek must succeed on a well-formed file");
    assert_eq!(
        peek.width, width,
        "seed={seed} label={label}: header-peek width disagrees with whole-file decode",
    );
    assert_eq!(
        peek.height, height,
        "seed={seed} label={label}: header-peek height disagrees with whole-file decode",
    );
    assert_eq!(
        peek.body_len,
        flat.len() * 2,
        "seed={seed} label={label}: header-peek body_len disagrees with decoded sample count",
    );

    // (6) Streaming reader matches whole-file decode.
    let mut reader = FarbfeldStreamReader::new(Cursor::new(encoded_image.clone()))
        .expect("streaming reader accepts our own output");
    let streamed = reader
        .read_all_rows()
        .expect("streaming reader drains its own output");
    assert_eq!(
        streamed, flat,
        "seed={seed} label={label}: stream read disagrees with whole-file decode at {width}×{height}",
    );

    // (6) Streaming writer matches whole-file encode.
    let mut writer = FarbfeldStreamWriter::new(Vec::new(), width, height)
        .expect("streaming writer accepts the same dimensions");
    let row_samples = (width as usize) * 4;
    if row_samples == 0 {
        // zero-width: every row carries no body, but write_row is still
        // called height times to honour the row-count contract.
        for _ in 0..height {
            writer.write_row(&[]).expect("zero-width row write");
        }
    } else {
        for row_idx in 0..(height as usize) {
            let off = row_idx * row_samples;
            writer
                .write_row(&flat[off..off + row_samples])
                .expect("write_row succeeds on a valid row");
        }
    }
    let stream_written = writer.finish().expect("stream writer finishes cleanly");
    assert_eq!(
        stream_written, encoded_image,
        "seed={seed} label={label}: stream write disagrees with whole-file encode at {width}×{height}",
    );
}

// ---------------------------------------------------------------------------
// Per-shape sweeps. Each scenario runs the same invariant suite under
// a different distribution so a regression visible only on (say)
// zero-axis files surfaces in its own test name.
// ---------------------------------------------------------------------------

const ITERS_PER_SCENARIO: u32 = 96;

fn sweep_shape(shape: Shape, seed: u32, label: &str) {
    let mut rng = XorShift32::new(seed);
    for i in 0..ITERS_PER_SCENARIO {
        let (w, h) = shape.pick(&mut rng);
        let pixels = random_pixels(w, h, &mut rng);
        check_all_invariants(seed.wrapping_add(i), label, w, h, &pixels);
    }
}

#[test]
fn property_sweep_tiny() {
    sweep_shape(Shape::Tiny, 0xF00D_BEEF, "tiny");
}

#[test]
fn property_sweep_square() {
    sweep_shape(Shape::Square, 0x5EED_F00D, "square");
}

#[test]
fn property_sweep_tall_narrow() {
    sweep_shape(Shape::TallNarrow, 0xABCD_1234, "tall_narrow");
}

#[test]
fn property_sweep_wide_short() {
    sweep_shape(Shape::WideShort, 0x1234_ABCD, "wide_short");
}

#[test]
fn property_sweep_zero_axis() {
    sweep_shape(Shape::ZeroAxis, 0xDEAD_C0DE, "zero_axis");
}

#[test]
fn property_sweep_medium() {
    sweep_shape(Shape::Medium, 0xC0DE_F00D, "medium");
}

// ---------------------------------------------------------------------------
// Cross-scenario sanity checks that don't fit neatly inside one shape
// distribution. These run once each and exercise the parser's malformed-
// input refusal surface under random perturbations of the encoded byte
// stream.
// ---------------------------------------------------------------------------

#[test]
fn property_sweep_arbitrary_bytes_never_panic() {
    // Round 76 added a `cargo-fuzz` decode target that drives this same
    // invariant under libFuzzer's mutator. Repeat a deterministic
    // mirror here so a developer running `cargo test -p
    // oxideav-farbfeld` (without `cargo-fuzz`) still gets the
    // never-panic guarantee under random byte streams.
    let mut rng = XorShift32::new(0xBADC_0FFE);
    for _ in 0..256 {
        let len = (rng.next_in(64) as usize) + (rng.next_in(64) as usize);
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            bytes.push((rng.next_u32() & 0xFF) as u8);
        }
        // We don't care whether they parse — we care that they don't
        // panic on either entry point.
        let _ = parse_farbfeld(&bytes);
        let _ = parse_farbfeld_header(&bytes);
        let _ = FarbfeldStreamReader::new(Cursor::new(&bytes[..]));
    }
}

#[test]
fn property_sweep_corrupted_magic_always_rejected() {
    // Encode a valid file, then flip one byte inside the 8-byte magic
    // and confirm both the whole-file decoder and the streaming reader
    // reject it. Repeat under a deterministic PRNG so any new
    // accidentally-permissive magic check surfaces here.
    let mut rng = XorShift32::new(0xFEE1_DEAD);
    for _ in 0..64 {
        let (w, h) = (rng.next_in(8) + 1, rng.next_in(8) + 1);
        let pixels = random_pixels(w, h, &mut rng);
        let mut bytes = encode_farbfeld_image(&FarbfeldImage {
            width: w,
            height: h,
            pixels: flatten(&pixels),
        })
        .unwrap();
        let pos = (rng.next_in(7)) as usize;
        bytes[pos] = bytes[pos].wrapping_add(1);
        // Either the parse rejects (expected) OR the wrap happened to
        // land on the same byte (extremely unlikely under wrapping_add
        // of 1 on an ASCII byte, but defend against it explicitly).
        if &bytes[..8] != MAGIC {
            assert!(
                parse_farbfeld(&bytes).is_err(),
                "parse_farbfeld must reject a corrupted-magic file at {w}×{h}",
            );
            assert!(
                FarbfeldStreamReader::new(Cursor::new(&bytes[..])).is_err(),
                "FarbfeldStreamReader must reject a corrupted-magic file at {w}×{h}",
            );
        }
    }
}

#[test]
fn property_sweep_trailing_garbage_always_rejected() {
    // Spec says the file is exactly `16 + w*h*8` bytes long. Appending
    // a single byte must fail the whole-file parser.
    let mut rng = XorShift32::new(0xC0FF_EE5A);
    for _ in 0..64 {
        let (w, h) = (rng.next_in(8) + 1, rng.next_in(8) + 1);
        let pixels = random_pixels(w, h, &mut rng);
        let mut bytes = encode_farbfeld_image(&FarbfeldImage {
            width: w,
            height: h,
            pixels: flatten(&pixels),
        })
        .unwrap();
        bytes.push((rng.next_u32() & 0xFF) as u8);
        assert!(
            parse_farbfeld(&bytes).is_err(),
            "parse_farbfeld must reject trailing garbage at {w}×{h}",
        );
    }
}

#[test]
fn property_sweep_truncated_body_always_rejected() {
    // Drop the last byte of an otherwise well-formed file. Both the
    // whole-file decoder and the streaming reader (driven to drain
    // every row) must reject it as truncated.
    let mut rng = XorShift32::new(0xDEAD_BEEF);
    for _ in 0..64 {
        // Pick dimensions that produce at least one body byte we can
        // safely drop.
        let (w, h) = (rng.next_in(7) + 1, rng.next_in(7) + 1);
        let pixels = random_pixels(w, h, &mut rng);
        let mut bytes = encode_farbfeld_image(&FarbfeldImage {
            width: w,
            height: h,
            pixels: flatten(&pixels),
        })
        .unwrap();
        bytes.pop();
        assert!(
            parse_farbfeld(&bytes).is_err(),
            "parse_farbfeld must reject a truncated body at {w}×{h}",
        );
        // The streaming reader accepts the header (it's intact), but
        // must fail to drain every row.
        let mut reader = FarbfeldStreamReader::new(Cursor::new(&bytes[..]))
            .expect("header is intact, construction succeeds");
        assert!(
            reader.read_all_rows().is_err(),
            "FarbfeldStreamReader::read_all_rows must reject a truncated body at {w}×{h}",
        );
    }
}
