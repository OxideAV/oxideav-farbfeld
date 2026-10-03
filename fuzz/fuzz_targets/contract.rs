#![no_main]

//! The image-crate API contract surface over arbitrary attacker bytes:
//! `probe` / `info` / `decode` / `decode_with` / `decode_rgb8` /
//! `decode_rgba8` / `decode_from`, plus the `encode` / `encode_to` /
//! `encode_rgb8` / `encode_rgba8` round trips on every input that
//! decodes.
//!
//! Invariants asserted on every input:
//! * nothing panics;
//! * `probe` is `true` for every input `info` or `decode` accepts;
//! * `info` succeeds whenever `decode` does and agrees on the
//!   dimensions; `info().file_len()` equals the input length;
//! * `decode` / `decode_with(default)` / `decode_from` agree on their
//!   verdict and on the image; `decode_rgba16` carries the same samples;
//! * a decoded image is tightly packed `Rgba64Le`, `to_rgba8` /
//!   `to_rgb8` are the high bytes, `decode_rgb8` / `decode_rgba8` match
//!   them;
//! * `encode` and `encode_to` reproduce the input byte for byte;
//! * `encode_rgba8(to_rgba8())` decodes back to the same 8-bit pixels
//!   (the `× 257` / `>> 8` pair is an exact inverse);
//! * a hostile limit (`max_bytes = 0`) is `LimitExceeded` for any image
//!   with pixels, `Ok` for an empty one.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use oxideav_farbfeld::{
    decode, decode_from, decode_rgb8, decode_rgba16, decode_rgba8, decode_with, encode,
    encode_rgb8, encode_rgba8, encode_to, info, probe, DecodeOptions, EncodeOptions,
    FarbfeldError, PixelFormat,
};

fuzz_target!(|data: &[u8]| {
    let probed = probe(data);
    let info_r = info(data);
    let decoded = decode(data);
    let decoded_with = decode_with(data, &DecodeOptions::default());
    let streamed = decode_from(Cursor::new(data));
    let samples = decode_rgba16(data);

    assert_eq!(
        decoded.is_ok(),
        decoded_with.is_ok(),
        "decode and decode_with(default) must agree"
    );
    assert_eq!(
        decoded.is_ok(),
        samples.is_ok(),
        "decode and decode_rgba16 must agree"
    );
    if info_r.is_ok() {
        assert!(probed, "info accepted bytes probe rejected");
    }

    let Ok(img) = decoded else {
        // `decode_from` may accept where `decode` rejects only for
        // trailing bytes (it stops after the announced body).
        if let Ok(s) = streamed {
            let i = info(data).expect("decode_from accepted bytes info rejected");
            let announced = i.file_len().expect("decode_from accepted an unaddressable body");
            assert!(
                (data.len() as u64) > announced,
                "decode_from accepted an input decode rejected for a reason other than trailing bytes"
            );
            assert_eq!(decode(&data[..announced as usize]).unwrap(), s);
        }
        return;
    };

    assert!(probed, "decode accepted bytes probe rejected");
    let i = info_r.expect("decode accepted bytes info rejected");
    assert_eq!((i.width, i.height), (img.width(), img.height()));
    assert_eq!(i.file_len(), Some(data.len() as u64));
    assert_eq!(i.format, PixelFormat::Rgba64Le);
    assert_eq!(img.format(), PixelFormat::Rgba64Le);
    assert!(img.is_tightly_packed());
    assert_eq!(img.planes.len(), 1);
    assert_eq!(img.stride(), img.width() as usize * 8);
    assert_eq!(decoded_with.unwrap(), img);
    assert_eq!(
        streamed.expect("decode accepted bytes decode_from rejected"),
        img
    );
    let samples = samples.unwrap();
    assert_eq!(img.to_rgba16(), samples);

    // 8-bit reductions are the high bytes.
    let rgba8 = img.to_rgba8();
    let rgb8 = img.to_rgb8();
    let expect_rgba: Vec<u8> = samples.data.iter().map(|v| (v >> 8) as u8).collect();
    assert_eq!(rgba8, expect_rgba);
    let expect_rgb: Vec<u8> = expect_rgba
        .chunks_exact(4)
        .flat_map(|p| p[..3].to_vec())
        .collect();
    assert_eq!(rgb8, expect_rgb);
    assert_eq!(decode_rgba8(data).unwrap().data, rgba8);
    assert_eq!(decode_rgb8(data).unwrap().data, rgb8);

    // Lossless round trips.
    let opts = EncodeOptions::default();
    assert_eq!(encode(&img, &opts).unwrap().as_slice(), data);
    let mut out = Vec::new();
    encode_to(&img, &opts, &mut out).unwrap();
    assert_eq!(out.as_slice(), data);

    // 8-bit paths are exact inverses.
    let w = img.width();
    let h = img.height();
    let back = encode_rgba8(w, h, &rgba8, &opts).unwrap();
    assert_eq!(decode_rgba8(&back).unwrap().data, rgba8);
    let back = encode_rgb8(w, h, &rgb8, &opts).unwrap();
    assert_eq!(decode_rgb8(&back).unwrap().data, rgb8);

    // Limits fire before allocation.
    let r = decode_with(data, &DecodeOptions::default().with_max_bytes(0u64));
    if w == 0 || h == 0 {
        assert!(r.is_ok());
    } else {
        assert!(matches!(r, Err(FarbfeldError::LimitExceeded(_))));
    }
});
