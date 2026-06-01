//! Hard-asserted self-roundtrip + bit-exact byte compare.
//!
//! The farbfeld spec is small enough that "round 1 = 100% spec coverage"
//! is achievable. These tests:
//!
//! * cover every spec field (magic, width, height, RGBA u16 BE samples);
//! * confirm parser → encoder → parser is the identity for arbitrary
//!   pixel data;
//! * confirm encoder output is byte-exact against a manually-constructed
//!   reference file (i.e. matches the on-disk layout literally);
//! * confirm parser rejects every malformed input variant the spec
//!   admits as a failure mode.

use oxideav_farbfeld::{
    encode_farbfeld, encode_farbfeld_from_rgba16, encode_farbfeld_image, parse_farbfeld,
    FarbfeldImage, BYTES_PER_PIXEL, HEADER_LEN, MAGIC,
};

/// Build a synthetic farbfeld file by hand — the parser/encoder under
/// test are NOT used here. Mirrors the on-disk byte layout from
/// `docs/image/farbfeld/farbfeld-format.md` exactly.
fn synthesise_reference(width: u32, height: u32, samples_be: &[[u16; 4]]) -> Vec<u8> {
    assert_eq!(samples_be.len(), (width as usize) * (height as usize));
    let mut buf = Vec::with_capacity(HEADER_LEN + samples_be.len() * BYTES_PER_PIXEL);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&width.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
    for px in samples_be {
        for chan in px {
            buf.extend_from_slice(&chan.to_be_bytes());
        }
    }
    buf
}

/// Generate a deterministic test image with non-trivial pixel data.
fn make_test_pixels(w: u32, h: u32) -> Vec<[u16; 4]> {
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            // Spread values across the full u16 range so byte order
            // bugs are visible.
            out.push([
                (i.wrapping_mul(0x0123) & 0xFFFF) as u16,
                (i.wrapping_mul(0x4567) & 0xFFFF) as u16,
                (i.wrapping_mul(0x89AB) & 0xFFFF) as u16,
                (i.wrapping_mul(0xCDEF) & 0xFFFF) as u16,
            ]);
        }
    }
    out
}

#[test]
fn parser_decodes_synthesised_reference_byte_exact() {
    let pixels = make_test_pixels(7, 5);
    let reference = synthesise_reference(7, 5, &pixels);
    let img = parse_farbfeld(&reference).unwrap();
    assert_eq!(img.width, 7);
    assert_eq!(img.height, 5);
    let mut expected_flat = Vec::with_capacity(7 * 5 * 4);
    for px in &pixels {
        expected_flat.extend_from_slice(px);
    }
    assert_eq!(img.pixels, expected_flat);
}

#[test]
fn encoder_byte_exact_against_synthesised_reference() {
    let pixels = make_test_pixels(4, 3);
    let reference = synthesise_reference(4, 3, &pixels);
    let encoded = encode_farbfeld_from_rgba16(4, 3, &pixels).unwrap();
    assert_eq!(
        encoded, reference,
        "encoder output must match hand-built reference byte for byte"
    );
}

#[test]
fn full_roundtrip_decode_encode_decode_for_various_sizes() {
    for &(w, h) in &[
        (0u32, 0u32),
        (1, 1),
        (1, 7),
        (7, 1),
        (3, 5),
        (16, 16),
        (64, 33),
    ] {
        let pixels = make_test_pixels(w, h);
        let original = synthesise_reference(w, h, &pixels);
        let img = parse_farbfeld(&original).expect("decode");
        let re_encoded = encode_farbfeld_image(&img).expect("re-encode");
        assert_eq!(
            re_encoded, original,
            "decode+encode must be the identity for {w}×{h}"
        );
        let img2 = parse_farbfeld(&re_encoded).expect("re-decode");
        assert_eq!(img, img2, "decode+encode+decode must be the identity");
    }
}

#[test]
fn encode_farbfeld_raw_body_byte_exact() {
    // 2×1 with explicit BE bytes — verifies the raw-body entry point
    // does NOT byte-swap anything (spec says the body is BE on disk).
    let body_be: [u8; 16] = [
        0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, // pixel 0
        0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54, 0x32, 0x10, // pixel 1
    ];
    let bytes = encode_farbfeld(2, 1, &body_be).unwrap();
    assert_eq!(&bytes[..8], MAGIC);
    assert_eq!(&bytes[8..12], &2u32.to_be_bytes());
    assert_eq!(&bytes[12..16], &1u32.to_be_bytes());
    assert_eq!(&bytes[16..], &body_be[..]);
}

#[test]
fn parser_rejects_every_malformed_variant() {
    // 1) empty
    assert!(parse_farbfeld(&[]).is_err());
    // 2) header truncated (15 bytes)
    assert!(parse_farbfeld(&[0u8; 15]).is_err());
    // 3) wrong magic
    let mut bad_magic = vec![0u8; 16];
    bad_magic[..8].copy_from_slice(b"FARBFELD");
    assert!(parse_farbfeld(&bad_magic).is_err());
    // 4) body shorter than announced
    let mut short_body = Vec::from(&b"farbfeld"[..]);
    short_body.extend_from_slice(&2u32.to_be_bytes());
    short_body.extend_from_slice(&2u32.to_be_bytes());
    short_body.extend_from_slice(&[0u8; 16]); // need 32 bytes (2×2×8)
    assert!(parse_farbfeld(&short_body).is_err());
    // 5) trailing garbage
    let mut over = Vec::from(&b"farbfeld"[..]);
    over.extend_from_slice(&1u32.to_be_bytes());
    over.extend_from_slice(&1u32.to_be_bytes());
    over.extend_from_slice(&[0u8; 8]); // correct
    over.push(0xFF); // junk
    assert!(parse_farbfeld(&over).is_err());
}

#[test]
fn farbfeld_image_new_validates_buffer_length() {
    // Correct length succeeds.
    assert!(FarbfeldImage::new(2, 3, vec![0u16; 24]).is_some());
    // Wrong length is rejected.
    assert!(FarbfeldImage::new(2, 3, vec![0u16; 23]).is_none());
    assert!(FarbfeldImage::new(2, 3, vec![0u16; 25]).is_none());
    // Zero dimension accepts zero-length buffer.
    assert!(FarbfeldImage::new(0, 0, vec![]).is_some());
}

#[test]
fn known_byte_pattern_roundtrip() {
    // Hand-built single-pixel red image — bit-exact byte compare.
    // R=0xFFFF, G=0x0000, B=0x0000, A=0xFFFF
    let expected: Vec<u8> = vec![
        b'f', b'a', b'r', b'b', b'f', b'e', b'l', b'd', // magic
        0, 0, 0, 1, // width = 1
        0, 0, 0, 1, // height = 1
        0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, // RGBA
    ];
    let pixels = [[0xFFFFu16, 0x0000, 0x0000, 0xFFFF]];
    let actual = encode_farbfeld_from_rgba16(1, 1, &pixels).unwrap();
    assert_eq!(actual, expected);

    let parsed = parse_farbfeld(&expected).unwrap();
    assert_eq!(parsed.width, 1);
    assert_eq!(parsed.height, 1);
    assert_eq!(parsed.pixels, [0xFFFFu16, 0x0000, 0x0000, 0xFFFF]);
}
