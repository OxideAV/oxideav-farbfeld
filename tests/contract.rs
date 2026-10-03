//! Image-crate API contract conformance (`IMAGE_CRATE_API`) and the
//! exact round-trip property over random 16-bit images.
//!
//! Every test is standalone (no `oxideav-core`): it runs in the
//! `ci-standalone` job too. The property sweep is seeded and
//! deterministic; a failure prints the seed and shape.

use std::io::Cursor;

use oxideav_farbfeld::{
    decode, decode_from, decode_from_with, decode_rgb8, decode_rgba16, decode_rgba8, decode_with,
    encode, encode_rgb8, encode_rgba16, encode_rgba8, encode_to, info, probe, ColorInfo,
    DecodeOptions, EncodeOptions, FarbfeldError, FarbfeldImage, Metadata, PixelFormat, Plane,
    HEADER_LEN, MAGIC,
};

/// xorshift64* — small, deterministic, good enough to cover every bit
/// of every 16-bit sample over the sweep.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random samples with a deliberately rich bit-level distribution:
/// uniform words, plus the extremes (`0`, `0xFFFF`, `0x00FF`, `0x0100`,
/// `0x7FFF`, `0x8000`) that make byte-order or rounding slips visible.
fn random_samples(rng: &mut Rng, n: usize) -> Vec<u16> {
    const EDGES: [u16; 6] = [0, 0xFFFF, 0x00FF, 0x0100, 0x7FFF, 0x8000];
    (0..n)
        .map(|_| match rng.below(8) {
            0 => EDGES[rng.below(EDGES.len() as u64) as usize],
            _ => rng.next() as u16,
        })
        .collect()
}

fn reference_file(w: u32, h: u32, samples: &[u16]) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEADER_LEN + samples.len() * 2);
    v.extend_from_slice(MAGIC);
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_be_bytes());
    }
    v
}

/// The property: for a random `w × h` image of random 16-bit samples,
/// every path of the contract agrees and the file round trip is
/// bit-identical.
fn check_round_trip(seed: u64, w: u32, h: u32) {
    let mut rng = Rng(seed | 1);
    let n = (w as usize) * (h as usize) * 4;
    let samples = random_samples(&mut rng, n);
    let file = reference_file(w, h, &samples);
    let ctx = format!("seed={seed:#x} {w}×{h}");

    // probe / info
    assert!(probe(&file), "{ctx}: probe");
    let i = info(&file).unwrap();
    assert_eq!((i.width, i.height, i.frames), (w, h, 1), "{ctx}: info");
    assert_eq!(i.file_len(), Some(file.len() as u64), "{ctx}: file_len");

    // decode: native layout is the LE byte-swap of the wire.
    let img = decode(&file).unwrap();
    assert_eq!((img.width(), img.height()), (w, h), "{ctx}");
    assert_eq!(img.format(), PixelFormat::Rgba64Le, "{ctx}");
    assert!(img.is_tightly_packed(), "{ctx}");
    let plane = img.as_bytes().unwrap();
    assert_eq!(plane.len(), n * 2, "{ctx}: plane length");
    for (k, s) in samples.iter().enumerate() {
        assert_eq!(
            [plane[2 * k], plane[2 * k + 1]],
            s.to_le_bytes(),
            "{ctx}: sample {k} not little-endian in the plane"
        );
    }
    assert_eq!(img.color, ColorInfo::srgb(), "{ctx}");
    assert_eq!(img.metadata, Metadata::default(), "{ctx}");

    // 16-bit depth path and conversions are lossless.
    let s16 = decode_rgba16(&file).unwrap();
    assert_eq!(s16.data, samples, "{ctx}: decode_rgba16");
    assert_eq!(img.to_rgba16(), s16, "{ctx}: to_rgba16");
    assert_eq!(
        FarbfeldImage::from(s16.clone()),
        img,
        "{ctx}: From<Rgba16Image>"
    );
    assert_eq!(
        FarbfeldImage::from_rgba16(w, h, &samples).unwrap(),
        img,
        "{ctx}: from_rgba16"
    );

    // Every encode path reproduces the file bit for bit.
    let opts = EncodeOptions::default();
    assert_eq!(encode(&img, &opts).unwrap(), file, "{ctx}: encode");
    assert_eq!(
        encode_rgba16(w, h, &samples, &opts).unwrap(),
        file,
        "{ctx}: encode_rgba16"
    );
    let mut streamed = Vec::new();
    encode_to(&img, &opts, &mut streamed).unwrap();
    assert_eq!(streamed, file, "{ctx}: encode_to");
    assert_eq!(
        decode_from(Cursor::new(&file)).unwrap(),
        img,
        "{ctx}: decode_from"
    );
    assert_eq!(
        decode_with(&file, &DecodeOptions::default().unlimited()).unwrap(),
        img,
        "{ctx}: decode_with(unlimited)"
    );
    assert_eq!(
        decode(&encode(&img, &opts).unwrap()).unwrap(),
        img,
        "{ctx}: decode(encode(img))"
    );

    // A padded plane encodes to the same bytes.
    if w > 0 && h > 0 {
        let row = w as usize * 8;
        let pad = 1 + (rng.below(13) as usize);
        let mut padded = Vec::with_capacity((row + pad) * h as usize);
        for r in plane.chunks_exact(row) {
            padded.extend_from_slice(r);
            padded.extend((0..pad).map(|_| rng.next() as u8));
        }
        let p = FarbfeldImage::new(
            w,
            h,
            PixelFormat::Rgba64Le,
            vec![Plane::new(row + pad, padded)],
        )
        .unwrap();
        assert_eq!(encode(&p, &opts).unwrap(), file, "{ctx}: padded encode");
        let mut st = Vec::new();
        encode_to(&p, &opts, &mut st).unwrap();
        assert_eq!(st, file, "{ctx}: padded encode_to");
        assert_eq!(p.to_rgba16(), s16, "{ctx}: padded to_rgba16");
    }

    // 8-bit reductions: high byte, exactly; and the 8-bit encode paths
    // invert them exactly.
    let rgba8 = img.to_rgba8();
    let expect: Vec<u8> = samples.iter().map(|v| (v >> 8) as u8).collect();
    assert_eq!(rgba8, expect, "{ctx}: to_rgba8");
    let rgb8 = img.to_rgb8();
    let expect3: Vec<u8> = expect
        .chunks_exact(4)
        .flat_map(|p| p[..3].to_vec())
        .collect();
    assert_eq!(rgb8, expect3, "{ctx}: to_rgb8");
    assert_eq!(
        decode_rgba8(&file).unwrap().data,
        rgba8,
        "{ctx}: decode_rgba8"
    );
    assert_eq!(decode_rgb8(&file).unwrap().data, rgb8, "{ctx}: decode_rgb8");
    let f8 = encode_rgba8(w, h, &rgba8, &opts).unwrap();
    assert_eq!(
        decode_rgba8(&f8).unwrap().data,
        rgba8,
        "{ctx}: rgba8 inverse"
    );
    let f3 = encode_rgb8(w, h, &rgb8, &opts).unwrap();
    assert_eq!(decode_rgb8(&f3).unwrap().data, rgb8, "{ctx}: rgb8 inverse");
    // The widening is `v × 257`, alpha opaque for RGB input.
    let wide = decode_rgba16(&f3).unwrap().data;
    for (px, src) in wide.chunks_exact(4).zip(rgb8.chunks_exact(3)) {
        assert_eq!(px[0], src[0] as u16 * 257, "{ctx}");
        assert_eq!(px[1], src[1] as u16 * 257, "{ctx}");
        assert_eq!(px[2], src[2] as u16 * 257, "{ctx}");
        assert_eq!(px[3], 0xFFFF, "{ctx}");
    }

    // Truncation and trailing bytes are rejected on the whole-file path;
    // the streaming path stops after the body.
    if !file.is_empty() {
        assert!(matches!(
            decode(&file[..file.len() - 1]),
            Err(FarbfeldError::InvalidData(_))
        ));
    }
    let mut long = file.clone();
    long.push(0xAA);
    assert!(
        matches!(decode(&long), Err(FarbfeldError::InvalidData(_))),
        "{ctx}"
    );
    let mut cur = Cursor::new(&long);
    assert_eq!(
        decode_from(&mut cur).unwrap(),
        img,
        "{ctx}: decode_from ignores tail"
    );
    assert_eq!(
        cur.position() as usize,
        file.len(),
        "{ctx}: decode_from stops at body end"
    );

    // Limits: one pixel too few trips LimitExceeded, exactly enough passes.
    let pixels = u64::from(w) * u64::from(h);
    if pixels > 0 {
        let o = DecodeOptions::default().with_max_pixels(pixels - 1);
        assert!(
            matches!(decode_with(&file, &o), Err(FarbfeldError::LimitExceeded(_))),
            "{ctx}"
        );
        assert!(
            matches!(
                decode_from_with(Cursor::new(&file), &o),
                Err(FarbfeldError::LimitExceeded(_))
            ),
            "{ctx}"
        );
        let o = DecodeOptions::default().with_max_bytes(pixels * 8 - 1);
        assert!(
            matches!(decode_with(&file, &o), Err(FarbfeldError::LimitExceeded(_))),
            "{ctx}"
        );
    }
    let o = DecodeOptions::default()
        .with_max_width(w)
        .with_max_height(h)
        .with_max_pixels(pixels)
        .with_max_bytes(pixels * 8);
    assert_eq!(
        decode_with(&file, &o).unwrap(),
        img,
        "{ctx}: exact limits pass"
    );
}

#[test]
fn round_trip_property_random_images_16bit() {
    // Seed printed in every assertion; change it here to replay a case.
    let mut rng = Rng(0x5EED_FA7B_FE1D_0467);
    // Shape distributions: tiny, thin, square-ish, and the degenerate
    // zero-width / zero-height / zero-both files.
    let shapes: Vec<(u32, u32)> = (0..48)
        .map(|k| match k % 6 {
            0 => (1 + rng.below(4) as u32, 1 + rng.below(4) as u32),
            1 => (1, 1 + rng.below(64) as u32),
            2 => (1 + rng.below(64) as u32, 1),
            3 => (1 + rng.below(40) as u32, 1 + rng.below(40) as u32),
            4 => (1 + rng.below(9) as u32, 1 + rng.below(300) as u32),
            _ => (1 + rng.below(300) as u32, 1 + rng.below(9) as u32),
        })
        .chain([(0, 0), (0, 5), (5, 0), (0, 1 << 20), (1 << 20, 0)])
        .collect();
    for (w, h) in shapes {
        let seed = rng.next();
        check_round_trip(seed, w, h);
    }
}

#[test]
fn every_16bit_sample_value_survives_a_round_trip() {
    // 65536 pixels, every possible sample value in every channel slot.
    let w = 256u32;
    let h = 256u32;
    let samples: Vec<u16> = (0..w * h)
        .flat_map(|i| {
            let v = i as u16;
            [v, v.rotate_left(4), !v, v.wrapping_mul(3)]
        })
        .collect();
    let file = encode_rgba16(w, h, &samples, &EncodeOptions::default()).unwrap();
    assert_eq!(file.len(), HEADER_LEN + samples.len() * 2);
    assert_eq!(decode_rgba16(&file).unwrap().data, samples);
    let img = decode(&file).unwrap();
    assert_eq!(img.to_rgba16().data, samples);
    assert_eq!(encode(&img, &EncodeOptions::default()).unwrap(), file);
    let rgba8 = img.to_rgba8();
    for (k, s) in samples.iter().enumerate() {
        assert_eq!(rgba8[k], (s >> 8) as u8, "sample {k}");
    }
}

#[test]
fn contract_shape_is_present() {
    // The exact names and shapes the contract table lists, exercised
    // once each so a rename cannot slip past the type checker.
    let img = FarbfeldImage::from_rgba8(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    let _: u32 = img.width();
    let _: u32 = img.height();
    let _: PixelFormat = img.format();
    let _: Option<&[u8]> = img.as_bytes();
    let _: Vec<u8> = img.to_rgb8();
    let _: Vec<u8> = img.to_rgba8();
    let _: Vec<u8> = img.clone().into_raw();
    let bytes = encode(&img, &EncodeOptions::new()).unwrap();
    let _: oxideav_farbfeld::ImageInfo = info(&bytes).unwrap();
    let _: oxideav_farbfeld::RgbImage = decode_rgb8(&bytes).unwrap();
    let _: oxideav_farbfeld::RgbaImage = decode_rgba8(&bytes).unwrap();
    let e: oxideav_farbfeld::Error = FarbfeldError::invalid("x");
    assert!(matches!(e, FarbfeldError::InvalidData(_)));
    let _: FarbfeldError = std::io::Error::other("x").into();
    assert!(DecodeOptions::default().max_bytes.is_some());
    assert!(!DecodeOptions::default().strict);
    assert!(FarbfeldImage::from_rgb8(2, 2, vec![0; 11]).is_err());
}
