//! Black-box cross-validation against ImageMagick's farbfeld encoder
//! and decoder.
//!
//! Per the round-77 dispatch and the workspace clean-room policy,
//! ImageMagick is invoked as an opaque process — its source is not
//! consulted. We feed our encoder's output to `magick`, and we
//! consume `magick`'s encoder output through our decoder.
//!
//! These tests degrade to a no-op when `magick` is not on `PATH` so
//! the suite still passes on machines that don't have ImageMagick
//! installed. The fall-back is a runtime guard, not `#[ignore]`.

use std::io::Write;
use std::process::{Command, Stdio};

use oxideav_farbfeld::{encode_farbfeld_from_rgba16, parse_farbfeld};

fn magick_available() -> bool {
    Command::new("magick")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run `magick` with the given argv, feeding `stdin_bytes` on stdin
/// and returning the captured stdout. The command must exit zero.
fn magick_run(args: &[&str], stdin_bytes: &[u8]) -> Vec<u8> {
    let mut child = Command::new("magick")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("magick spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_bytes)
        .expect("magick stdin");
    let out = child.wait_with_output().expect("magick wait");
    assert!(
        out.status.success(),
        "magick exit {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
    );
    out.stdout
}

#[test]
fn our_encoder_decodes_through_magick() {
    if !magick_available() {
        eprintln!("magick not on PATH — skipping cross-validation");
        return;
    }
    // Build a 4×3 image with a known pattern.
    let mut pixels: Vec<[u16; 4]> = Vec::new();
    for y in 0..3u16 {
        for x in 0..4u16 {
            pixels.push([
                x.wrapping_mul(0x4000),
                y.wrapping_mul(0x4000),
                ((x as u32 * y as u32) as u16).wrapping_mul(0x1111),
                0xFFFF,
            ]);
        }
    }
    let ours = encode_farbfeld_from_rgba16(4, 3, &pixels).unwrap();

    // Round-trip through magick: farbfeld -> farbfeld (forces a
    // full decode + re-encode on the magick side).
    let magick_out = magick_run(&["farbfeld:-", "farbfeld:-"], &ours);

    // The re-encoded bytes must be parseable and pixel-identical.
    let parsed = parse_farbfeld(&magick_out).expect("magick output is a valid farbfeld");
    assert_eq!(parsed.width, 4);
    assert_eq!(parsed.height, 3);
    let mut flat: Vec<u16> = Vec::with_capacity(4 * 3 * 4);
    for px in &pixels {
        flat.extend_from_slice(px);
    }
    assert_eq!(
        parsed.pixels, flat,
        "magick round-trip must preserve every u16 sample exactly",
    );
}

#[test]
fn magick_encoded_image_decodes_through_us() {
    if !magick_available() {
        eprintln!("magick not on PATH — skipping cross-validation");
        return;
    }
    // Build a PPM (P6) input — minimal, no compression — and ask
    // magick to encode it as farbfeld.
    let width = 6u32;
    let height = 4u32;
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    // Deterministic 8-bit RGB pattern.
    for y in 0..height {
        for x in 0..width {
            ppm.push(((x * 40) & 0xFF) as u8);
            ppm.push(((y * 60) & 0xFF) as u8);
            ppm.push((((x + y) * 30) & 0xFF) as u8);
        }
    }

    let ff_bytes = magick_run(&["ppm:-", "farbfeld:-"], &ppm);
    // Decode magick's output with our parser — every dimension and
    // pixel-count assertion is a real cross-validation.
    let parsed = parse_farbfeld(&ff_bytes).expect("our parser accepts magick output");
    assert_eq!(parsed.width, width);
    assert_eq!(parsed.height, height);
    assert_eq!(parsed.pixels.len() as u32, width * height * 4);
    // Alpha channel must be 0xFFFF for every pixel — PPM has no
    // alpha, magick fills it with opaque.
    for pixel_idx in 0..(width * height) as usize {
        let alpha = parsed.pixels[pixel_idx * 4 + 3];
        assert_eq!(alpha, 0xFFFF, "pixel {pixel_idx} alpha = 0x{alpha:04X}");
    }
}

#[test]
fn magick_byte_exact_self_roundtrip_through_our_parser() {
    if !magick_available() {
        eprintln!("magick not on PATH — skipping cross-validation");
        return;
    }
    // Build a deterministic farbfeld with our encoder, feed it
    // through magick farbfeld -> farbfeld twice, parse each output,
    // and require the pixel buffers to be identical at every stage.
    let pixels: Vec<[u16; 4]> = (0..8u16)
        .flat_map(|y| {
            (0..5u16).map(move |x| {
                [
                    (x as u32 * 7919) as u16,
                    (y as u32 * 7919) as u16,
                    ((x as u32 ^ y as u32) * 7919) as u16,
                    0xC0DE,
                ]
            })
        })
        .collect();
    let ours = encode_farbfeld_from_rgba16(5, 8, &pixels).unwrap();
    let baseline = parse_farbfeld(&ours).unwrap();

    let pass1 = magick_run(&["farbfeld:-", "farbfeld:-"], &ours);
    let p1 = parse_farbfeld(&pass1).unwrap();
    assert_eq!(p1.pixels, baseline.pixels);

    let pass2 = magick_run(&["farbfeld:-", "farbfeld:-"], &pass1);
    let p2 = parse_farbfeld(&pass2).unwrap();
    assert_eq!(p2.pixels, baseline.pixels);
}
