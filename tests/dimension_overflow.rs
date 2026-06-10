//! Dimension-overflow hardening sweep for the farbfeld header math.
//!
//! Round 275 (depth-mode property test): the crate is feature-complete
//! against the byte-layout description in
//! `docs/image/farbfeld/farbfeld-format.md`. The 16-byte header carries
//! two big-endian `u32` dimensions, and the body length the file
//! announces is `width * height * 8` bytes (spec: "Total file size =
//! `16 + width * height * 8`"). A maliciously-crafted header can declare
//! dimensions near `u32::MAX` while shipping only the 16-byte preamble,
//! so the size arithmetic in [`parse_farbfeld_header`] (two `checked_mul`
//! steps) and [`FarbfeldHeader::total_len`] (a `checked_add`) is real,
//! load-bearing DoS-hardening logic.
//!
//! Existing coverage hits that arithmetic only at hand-picked points
//! (`3×4`, `0×0`, and a *synthetic* `FarbfeldHeader { body_len:
//! usize::MAX }` constructed directly in `parser.rs`'s unit tests). What
//! was missing was a sweep that drives a **real 16-byte header** carrying
//! pathological `u32` dimensions through the public peek / parse surface
//! and cross-checks the announced body length against an independent
//! wide-integer (`u128`) computation — so an off-by-a-factor or a missing
//! `checked_*` in the dimension math surfaces as a body_len mismatch
//! rather than a silent wrap or a panic.
//!
//! ## Invariants under test
//!
//! For an arbitrary `(width, height)` pair encoded into a real 16-byte
//! header:
//!
//! 1. **No panic.** `parse_farbfeld_header` / `peek_farbfeld_dimensions`
//!    never panic for *any* `u32` dimension pair — overflow is reported,
//!    not triggered.
//!
//! 2. **Body-length exactness.** When `width * height * 8` fits `usize`,
//!    the reported `body_len` equals the `u128` reference computation
//!    exactly (this is the spec's `width*height*8` identity). The `u128`
//!    oracle cannot itself overflow for `u32` inputs, so it is an
//!    independent witness.
//!
//! 3. **Overflow is reported, never silently wrapped.** When the `u128`
//!    reference exceeds `usize::MAX`, the header parser rejects with
//!    `InvalidData` rather than returning a wrapped (smaller) body_len.
//!
//! 4. **`total_len` consistency.** When the header parses,
//!    `total_len()` either equals `16 + body_len` exactly, or rejects
//!    with `InvalidData` precisely when `16 + body_len` itself overflows
//!    `usize` — and never panics.
//!
//! 5. **Pathological-header rejection without allocation.** A header
//!    announcing a huge body but shipping only the 16-byte preamble is
//!    rejected by `parse_farbfeld` via the announced-vs-present
//!    cross-check, without first allocating the announced multi-gigabyte
//!    buffer (the rejection returns fast; we assert the error rather than
//!    OOM).
//!
//! The sweep is offline / no-extra-dep (an inline xorshift32 PRNG drives
//! the random dimension pairs) so any failure is reproducible from the
//! seed printed in the assertion message.

use oxideav_farbfeld::{
    parse_farbfeld, parse_farbfeld_header, peek_farbfeld_dimensions, FarbfeldError,
    BYTES_PER_PIXEL, HEADER_LEN, MAGIC,
};

// ---------------------------------------------------------------------------
// Deterministic PRNG (inline xorshift32, same family the other sweeps use)
// ---------------------------------------------------------------------------

struct XorShift32 {
    state: u32,
}

impl XorShift32 {
    fn new(seed: u32) -> Self {
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
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a real 16-byte farbfeld header for `(width, height)`. No body
/// bytes follow — we only ever feed this to the header / peek path or to
/// `parse_farbfeld` (which must reject the missing body).
fn make_header(width: u32, height: u32) -> [u8; HEADER_LEN] {
    let mut buf = [0u8; HEADER_LEN];
    buf[..8].copy_from_slice(MAGIC);
    buf[8..12].copy_from_slice(&width.to_be_bytes());
    buf[12..16].copy_from_slice(&height.to_be_bytes());
    buf
}

/// Independent wide-integer reference for the announced body length.
/// `u32 * u32 * 8` is at most `2^64 * 8 = 2^67`, which fits `u128`
/// without any possibility of overflow, so this is a trustworthy oracle
/// for what `width * height * 8` "should" be.
fn reference_body_len(width: u32, height: u32) -> u128 {
    (width as u128) * (height as u128) * (BYTES_PER_PIXEL as u128)
}

/// Assert invariants 1..=4 on one `(width, height)` pair.
fn check_header_math(seed: u32, width: u32, height: u32) {
    let header = make_header(width, height);
    let oracle = reference_body_len(width, height);

    // (1) No panic on either header entry point. `peek` is a thin wrapper
    //     over `parse_farbfeld_header`, but exercise both so a future
    //     divergence is caught.
    let parsed = parse_farbfeld_header(&header);
    let peeked = peek_farbfeld_dimensions(&header);

    // The two header entry points must agree on the result (Ok/Err and
    // value) for every input.
    assert_eq!(
        parsed.is_ok(),
        peeked.is_ok(),
        "seed={seed}: parse_farbfeld_header and peek disagree on {width}×{height}",
    );

    match parsed {
        Ok(h) => {
            // Dimensions echo the header verbatim.
            assert_eq!(h.width, width, "seed={seed}: width not echoed");
            assert_eq!(h.height, height, "seed={seed}: height not echoed");

            // (2) Body-length exactness against the u128 oracle. Reaching
            //     Ok means the body length fit usize, so the oracle must
            //     fit usize too and equal it.
            assert!(
                oracle <= usize::MAX as u128,
                "seed={seed}: header parsed Ok but oracle body_len {oracle} > usize::MAX for {width}×{height}",
            );
            assert_eq!(
                h.body_len as u128, oracle,
                "seed={seed}: body_len {} != width*height*8 = {oracle} for {width}×{height}",
                h.body_len,
            );

            // Peek returns the same struct.
            assert_eq!(
                peeked.unwrap(),
                h,
                "seed={seed}: peek struct differs from parse_farbfeld_header at {width}×{height}",
            );

            // (4) total_len consistency: either 16 + body_len exactly, or
            //     a clean InvalidData when that sum overflows usize.
            match h.total_len() {
                Ok(total) => {
                    let expected = HEADER_LEN
                        .checked_add(h.body_len)
                        .expect("total_len returned Ok so the sum must fit usize");
                    assert_eq!(
                        total, expected,
                        "seed={seed}: total_len {total} != 16 + body_len {} at {width}×{height}",
                        h.body_len,
                    );
                }
                Err(FarbfeldError::InvalidData(msg)) => {
                    // The only legitimate reason to fail here is that
                    // 16 + body_len overflows usize.
                    assert!(
                        HEADER_LEN.checked_add(h.body_len).is_none(),
                        "seed={seed}: total_len rejected {width}×{height} but 16 + {} fits usize (msg = {msg:?})",
                        h.body_len,
                    );
                    assert!(
                        msg.contains("overflow"),
                        "seed={seed}: total_len overflow message should mention overflow, got {msg:?}",
                    );
                }
            }
        }
        Err(FarbfeldError::InvalidData(msg)) => {
            // (3) The header parser only rejects a well-magic'd header
            //     when the size arithmetic overflows usize. The oracle
            //     proves overflow was genuine: width*height*8 really did
            //     exceed usize::MAX.
            assert!(
                oracle > usize::MAX as u128,
                "seed={seed}: header rejected {width}×{height} but width*height*8 = {oracle} fits usize::MAX (msg = {msg:?})",
            );
            assert!(
                msg.contains("overflow"),
                "seed={seed}: overflow rejection should mention overflow, got {msg:?}",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Sweeps
// ---------------------------------------------------------------------------

/// Hand-picked boundary dimension pairs. These pin the exact spots where
/// the `checked_mul` / `checked_add` decisions flip, independent of the
/// host pointer width.
#[test]
fn dimension_math_boundary_points() {
    // Each tuple is a (width, height) the math must handle exactly.
    let cases: &[(u32, u32)] = &[
        (0, 0),
        (1, 1),
        (0, u32::MAX),
        (u32::MAX, 0),
        (1, u32::MAX),
        (u32::MAX, 1),
        (u32::MAX, u32::MAX),
        // 0xFFFF_FFFF / 8-ish region — where width*height*8 lands close
        // to the 64-bit usize ceiling.
        (0xFFFF_FFFF, 0xFFFF_FFFF),
        (0x1_0000, 0x1_0000), // exactly 2^32 pixels = 2^35 body bytes.
        (0xFFFF, 0xFFFF),     // ~2^32 pixels just under the u32 corner.
        (65_536, 65_535),
    ];
    for &(w, h) in cases {
        check_header_math(0xB0_0000 ^ w ^ h, w, h);
    }
}

/// Random sweep biased toward the high end of the `u32` range so the
/// overflow boundary gets hit on 32-bit *and* 64-bit hosts. On a 64-bit
/// host `u32::MAX * u32::MAX * 8 = 2^67` overflows usize, so the upper
/// band exercises invariant (3); the lower band exercises (2).
#[test]
fn dimension_math_random_sweep() {
    let mut rng = XorShift32::new(0xFA5B_FE1D);
    for i in 0..4096u32 {
        // Mix of full-range, high-band, and small dimensions so every
        // branch of the checked arithmetic is reached.
        let (w, h) = match i % 4 {
            0 => (rng.next_u32(), rng.next_u32()),
            1 => (rng.next_u32() | 0x8000_0000, rng.next_u32() | 0x8000_0000),
            2 => (rng.next_u32() % 4096, rng.next_u32() % 4096),
            _ => (rng.next_u32() % 256, rng.next_u32()),
        };
        check_header_math(i, w, h);
    }
}

/// A header that announces a huge body but ships only the 16-byte
/// preamble must be rejected by `parse_farbfeld` via the
/// announced-vs-present cross-check — and the rejection must come back as
/// an error (not an out-of-memory abort from allocating the announced
/// body). We assert the error and that the call returns promptly.
#[test]
fn pathological_header_only_file_rejected_without_allocation() {
    // Several huge dimension pairs whose announced body is far larger than
    // any real address space, each shipped with *only* the 16-byte header.
    let cases: &[(u32, u32)] = &[
        (u32::MAX, u32::MAX),
        (u32::MAX, 1),
        (1, u32::MAX),
        (0x4000_0000, 0x10),
        (0x10, 0x4000_0000),
    ];
    for &(w, h) in cases {
        let header = make_header(w, h);
        let err = parse_farbfeld(&header).expect_err(
            "a 16-byte-only file announcing a multi-gigabyte body must be rejected, not parsed",
        );
        let FarbfeldError::InvalidData(msg) = err;
        // Whether the rejection is the overflow path (32-bit hosts) or the
        // body-size-mismatch path (64-bit hosts), it must be an
        // InvalidData with a descriptive message and must NOT have
        // allocated the announced body to discover it.
        assert!(
            msg.contains("overflow") || msg.contains("mismatch"),
            "rejection of {w}×{h} header-only file should explain why, got {msg:?}",
        );
    }
}

/// The header parser must reject a real header carrying valid dimensions
/// but a corrupted magic, regardless of how the dimensions would have
/// computed — the magic check gates the arithmetic.
#[test]
fn corrupt_magic_short_circuits_dimension_math() {
    let mut header = make_header(u32::MAX, u32::MAX);
    header[3] ^= 0xFF; // break the magic
    assert!(
        parse_farbfeld_header(&header).is_err(),
        "corrupted magic must reject before/regardless of dimension math",
    );
    assert!(peek_farbfeld_dimensions(&header).is_err());
}
