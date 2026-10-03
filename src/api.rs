//! The root vocabulary of the image-crate API contract
//! (`IMAGE_CRATE_API`): `probe` / `info` / `decode*` / `encode*`, plus
//! farbfeld's 16-bit depth pair `decode_rgba16` / `encode_rgba16`.
//!
//! Every function here is framework-free (builds with
//! `default-features = false`) and is the single implementation the
//! registry `Decoder` / `Encoder` adapters call.

use std::io::{Read, Write};

use crate::encoder;
use crate::error::Result;
use crate::image::{FarbfeldImage, ImageInfo, RgbImage, RgbaImage};
use crate::options::{DecodeOptions, EncodeOptions};
use crate::parser;
use crate::rgba16::Rgba16Image;
use crate::stream;

/// `true` when `bytes` starts with the 8-byte magic `farbfeld`. Total,
/// allocation-free, `false` on short input. Says nothing about the
/// dimensions or the body — see [`info`].
pub fn probe(bytes: &[u8]) -> bool {
    parser::has_magic(bytes)
}

/// Header only: `width`, `height`, the native [`crate::PixelFormat`]
/// (`Rgba64Le`), `frames` (always 1), `has_alpha` (always `true`), the
/// sRGB colour convention and the (always absent) metadata flags. Reads
/// the 16-byte header and nothing else; accepts an input as short as
/// the header and never fails on geometry (a header announcing more
/// bytes than any host can hold still describes a valid file —
/// [`ImageInfo::file_len`] tells you how many).
///
/// Errors: [`crate::FarbfeldError::InvalidData`] for fewer than 16
/// bytes or a wrong magic.
pub fn info(bytes: &[u8]) -> Result<ImageInfo> {
    parser::read_dimensions(bytes).map(|(w, h)| ImageInfo::new(w, h))
}

/// Decode a complete farbfeld file into its native layout (one tightly
/// packed `Rgba64Le` plane, samples byte-swapped from the wire's
/// big-endian) with [`DecodeOptions::default`] (decoded plane capped at
/// 1 GiB). The input must be exactly `16 + width × height × 8` bytes:
/// a short body is truncated, a long one carries trailing bytes, both
/// [`crate::FarbfeldError::InvalidData`].
pub fn decode(bytes: &[u8]) -> Result<FarbfeldImage> {
    decode_with(bytes, &DecodeOptions::default())
}

/// [`decode`] with explicit limits. Every limit is checked against the
/// header before the plane is allocated
/// ([`crate::FarbfeldError::LimitExceeded`]).
pub fn decode_with(bytes: &[u8], opts: &DecodeOptions) -> Result<FarbfeldImage> {
    parser::decode_with(bytes, opts)
}

/// Decode straight to tightly packed 8-bit RGB (high byte of every
/// 16-bit sample, alpha dropped — see [`FarbfeldImage::to_rgb8`]),
/// default limits.
pub fn decode_rgb8(bytes: &[u8]) -> Result<RgbImage> {
    let img = decode(bytes)?;
    Ok(RgbImage::new(img.width, img.height, img.to_rgb8()))
}

/// Decode straight to tightly packed 8-bit RGBA (high byte of every
/// 16-bit sample — see [`FarbfeldImage::to_rgba8`]), default limits.
pub fn decode_rgba8(bytes: &[u8]) -> Result<RgbaImage> {
    let img = decode(bytes)?;
    Ok(RgbaImage::new(img.width, img.height, img.to_rgba8()))
}

/// Decode straight to native-endian 16-bit `[R, G, B, A]` samples
/// ([`Rgba16Image`]) — farbfeld's lossless depth path, default limits.
/// Same accept / reject verdict as [`decode`]; the samples are read
/// from the wire in one pass without an intermediate byte plane.
pub fn decode_rgba16(bytes: &[u8]) -> Result<Rgba16Image> {
    parser::decode_rgba16_with(bytes, &DecodeOptions::default())
}

/// [`decode_rgba16`] with explicit limits.
pub fn decode_rgba16_with(bytes: &[u8], opts: &DecodeOptions) -> Result<Rgba16Image> {
    parser::decode_rgba16_with(bytes, opts)
}

/// Decode from a reader — genuinely streaming, default limits. The
/// 16-byte header is read first and the limits checked against it;
/// only then is the plane allocated, and the body is pulled one row at
/// a time through the same bounded-`take` discipline as
/// [`crate::FarbfeldStreamReader`], so a header announcing a body the
/// reader cannot deliver fails on the first short row. The reader is
/// left positioned just past the announced body; bytes after it are not
/// inspected (unlike [`decode`], which rejects trailing bytes). A short
/// read is [`crate::FarbfeldError::InvalidData`], any other read failure
/// [`crate::FarbfeldError::Io`].
pub fn decode_from<R: Read>(r: R) -> Result<FarbfeldImage> {
    decode_from_with(r, &DecodeOptions::default())
}

/// [`decode_from`] with explicit limits.
pub fn decode_from_with<R: Read>(r: R, opts: &DecodeOptions) -> Result<FarbfeldImage> {
    stream::decode_reader(r, opts)
}

/// Encode `image` as a complete farbfeld file. The single `Rgba64Le`
/// plane is byte-swapped to the wire's big-endian (a plane with row
/// padding is written without it); `color` and `metadata` cannot be
/// carried and are ignored; `opts` has no fields. There is no
/// `Rgba64Le` image farbfeld cannot represent, so
/// [`crate::FarbfeldError::Unsupported`] is reserved for geometry that
/// overflows `usize`.
pub fn encode(image: &FarbfeldImage, _opts: &EncodeOptions) -> Result<Vec<u8>> {
    encoder::encode_image(image)
}

/// Encode tightly packed 8-bit RGB (`3 × width × height` bytes). farbfeld
/// is 16-bit RGBA only, so every sample is widened `v → v × 257` and
/// alpha set to `65535` ([`FarbfeldImage::from_rgb8`]); decoding the
/// result with [`decode_rgb8`] gives the input back exactly. A short
/// buffer is [`crate::FarbfeldError::InvalidData`].
pub fn encode_rgb8(width: u32, height: u32, rgb: &[u8], opts: &EncodeOptions) -> Result<Vec<u8>> {
    encode(
        &FarbfeldImage::from_rgb8(width, height, rgb.to_vec())?,
        opts,
    )
}

/// Encode tightly packed 8-bit RGBA (`4 × width × height` bytes), every
/// sample widened `v → v × 257` ([`FarbfeldImage::from_rgba8`]);
/// [`decode_rgba8`] of the result gives the input back exactly. A short
/// buffer is [`crate::FarbfeldError::InvalidData`].
pub fn encode_rgba8(width: u32, height: u32, rgba: &[u8], opts: &EncodeOptions) -> Result<Vec<u8>> {
    encode(
        &FarbfeldImage::from_rgba8(width, height, rgba.to_vec())?,
        opts,
    )
}

/// Encode native-endian 16-bit `[R, G, B, A]` samples (`4 × width ×
/// height` `u16`s, exactly) — farbfeld's lossless depth path, written
/// to the wire in one pass. A length mismatch is
/// [`crate::FarbfeldError::InvalidData`].
pub fn encode_rgba16(
    width: u32,
    height: u32,
    samples: &[u16],
    _opts: &EncodeOptions,
) -> Result<Vec<u8>> {
    encoder::encode_rgba16_samples(width, height, samples)
}

/// [`encode`] into a writer — genuinely streaming through
/// [`crate::FarbfeldStreamWriter`]: the header, then one byte-swapped
/// row at a time, never the whole file in memory. Byte-identical to
/// [`encode`]. Write failures surface as [`crate::FarbfeldError::Io`].
pub fn encode_to<W: Write>(image: &FarbfeldImage, _opts: &EncodeOptions, w: W) -> Result<()> {
    stream::encode_writer(image, w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{ColorInfo, PixelFormat};
    use crate::parser::{HEADER_LEN, MAGIC};
    use crate::FarbfeldError;

    fn samples(w: u32, h: u32) -> Vec<u16> {
        (0..(w * h * 4))
            .map(|i| (i.wrapping_mul(0x9E37) ^ (i >> 3)) as u16)
            .collect()
    }

    fn file(w: u32, h: u32, s: &[u16]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(MAGIC);
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        for x in s {
            v.extend_from_slice(&x.to_be_bytes());
        }
        v
    }

    #[test]
    fn probe_is_total_and_magic_only() {
        assert!(!probe(b""));
        assert!(!probe(b"farbfel"));
        assert!(probe(b"farbfeld"));
        assert!(probe(&file(0, 0, &[])));
        assert!(!probe(b"FARBFELD\0\0\0\0\0\0\0\0"));
        assert!(!probe(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn info_reads_header_only_and_never_fails_on_geometry() {
        let bytes = file(5, 3, &samples(5, 3));
        let i = info(&bytes[..HEADER_LEN]).unwrap();
        assert_eq!((i.width, i.height), (5, 3));
        assert_eq!(i.format, PixelFormat::Rgba64Le);
        assert_eq!(i.frames, 1);
        assert!(i.has_alpha);
        assert_eq!(i.color, ColorInfo::srgb());
        assert!(!i.has_icc && !i.has_exif && !i.has_xmp);
        assert_eq!(i.file_len(), Some(bytes.len() as u64));
        assert_eq!(info(&bytes).unwrap(), i);
        // Hostile header: u32::MAX × u32::MAX is still describable.
        let huge = file(u32::MAX, u32::MAX, &[]);
        let i = info(&huge).unwrap();
        assert_eq!((i.width, i.height), (u32::MAX, u32::MAX));
        assert_eq!(i.file_len(), None);
        assert!(matches!(
            info(b"farbfeld"),
            Err(FarbfeldError::InvalidData(_))
        ));
        assert!(matches!(
            info(&[0u8; 16]),
            Err(FarbfeldError::InvalidData(_))
        ));
    }

    #[test]
    fn decode_fills_native_layout_and_colour() {
        let s = samples(4, 3);
        let bytes = file(4, 3, &s);
        let img = decode(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (4, 3));
        assert_eq!(img.format(), PixelFormat::Rgba64Le);
        assert_eq!(img.planes.len(), 1);
        assert_eq!(img.planes[0].stride, 32);
        assert!(img.is_tightly_packed());
        assert_eq!(img.color, ColorInfo::farbfeld_default());
        assert!(img.metadata.is_empty());
        // Plane is little-endian: sample 0 low byte first.
        let b = img.as_bytes().unwrap();
        assert_eq!(&b[..2], &s[0].to_le_bytes());
        assert_eq!(img.to_rgba16().data, s);
        assert_eq!(decode_rgba16(&bytes).unwrap().data, s);
    }

    #[test]
    fn rgb8_and_rgba8_raw_paths_take_the_high_byte() {
        let s = samples(3, 2);
        let bytes = file(3, 2, &s);
        let rgba = decode_rgba8(&bytes).unwrap();
        assert_eq!((rgba.width, rgba.height), (3, 2));
        let expect: Vec<u8> = s.iter().map(|v| (v >> 8) as u8).collect();
        assert_eq!(rgba.as_bytes(), &expect[..]);
        let rgb = decode_rgb8(&bytes).unwrap();
        let expect3: Vec<u8> = expect
            .chunks_exact(4)
            .flat_map(|p| p[..3].to_vec())
            .collect();
        assert_eq!(rgb.into_raw(), expect3);
    }

    #[test]
    fn lossless_round_trip_pins_planes_colour_metadata_and_bytes() {
        let s = samples(7, 5);
        let bytes = file(7, 5, &s);
        let img = decode(&bytes).unwrap();
        let out = encode(&img, &EncodeOptions::default()).unwrap();
        assert_eq!(out, bytes, "file round trip is bit-identical");
        let back = decode(&out).unwrap();
        assert_eq!(back, img, "decode(encode(img)) == img");
        // From the 16-bit depth path too.
        assert_eq!(
            encode_rgba16(7, 5, &s, &EncodeOptions::default()).unwrap(),
            bytes
        );
        // Zero-size images are 16-byte files.
        for (w, h) in [(0, 0), (0, 9), (9, 0)] {
            let z = encode_rgba16(w, h, &[], &EncodeOptions::default()).unwrap();
            assert_eq!(z.len(), HEADER_LEN);
            let back = decode(&z).unwrap();
            assert_eq!((back.width, back.height), (w, h));
            assert_eq!(encode(&back, &EncodeOptions::default()).unwrap(), z);
        }
    }

    #[test]
    fn eight_bit_encode_paths_round_trip_exactly() {
        let rgb: Vec<u8> = (0..3 * 6 * 4).map(|i| (i * 7) as u8).collect();
        let bytes = encode_rgb8(6, 4, &rgb, &EncodeOptions::default()).unwrap();
        assert_eq!(bytes.len(), HEADER_LEN + 6 * 4 * 8);
        assert_eq!(decode_rgb8(&bytes).unwrap().data, rgb);
        // Alpha is opaque in the file.
        let s = decode_rgba16(&bytes).unwrap();
        assert!(s.data.iter().skip(3).step_by(4).all(|&a| a == 0xFFFF));
        assert_eq!(s.data[0], rgb[0] as u16 * 257);
        let rgba: Vec<u8> = (0..4 * 6 * 4).map(|i| (i * 13) as u8).collect();
        let bytes = encode_rgba8(6, 4, &rgba, &EncodeOptions::default()).unwrap();
        assert_eq!(decode_rgba8(&bytes).unwrap().data, rgba);
        assert!(matches!(
            encode_rgb8(2, 2, &[0; 11], &EncodeOptions::default()),
            Err(FarbfeldError::InvalidData(_))
        ));
        assert!(matches!(
            encode_rgba8(2, 2, &[0; 15], &EncodeOptions::default()),
            Err(FarbfeldError::InvalidData(_))
        ));
        assert!(matches!(
            encode_rgba16(2, 2, &[0; 15], &EncodeOptions::default()),
            Err(FarbfeldError::InvalidData(_))
        ));
    }

    #[test]
    fn encode_skips_row_padding() {
        let s = samples(2, 2);
        let tight = FarbfeldImage::from_rgba16(2, 2, &s).unwrap();
        let mut padded = Vec::new();
        for row in tight.as_bytes().unwrap().chunks_exact(16) {
            padded.extend_from_slice(row);
            padded.extend_from_slice(&[0xEE; 6]);
        }
        let img = FarbfeldImage::packed(2, 2, PixelFormat::Rgba64Le, 22, padded).unwrap();
        let a = encode(&tight, &EncodeOptions::default()).unwrap();
        let b = encode(&img, &EncodeOptions::default()).unwrap();
        assert_eq!(a, b);
        let mut c = Vec::new();
        encode_to(&img, &EncodeOptions::default(), &mut c).unwrap();
        assert_eq!(a, c);
        assert_eq!(decode(&b).unwrap(), tight);
    }

    #[test]
    fn decode_rejects_truncation_trailing_bytes_and_bad_magic() {
        let s = samples(3, 3);
        let bytes = file(3, 3, &s);
        assert!(matches!(
            decode(&bytes[..bytes.len() - 1]),
            Err(FarbfeldError::InvalidData(_))
        ));
        let mut long = bytes.clone();
        long.push(0);
        assert!(matches!(decode(&long), Err(FarbfeldError::InvalidData(_))));
        let mut bad = bytes.clone();
        bad[0] = b'F';
        assert!(matches!(decode(&bad), Err(FarbfeldError::InvalidData(_))));
        assert!(matches!(decode(&[]), Err(FarbfeldError::InvalidData(_))));
        assert!(matches!(
            decode_rgba16(&bytes[..20]),
            Err(FarbfeldError::InvalidData(_))
        ));
    }

    #[test]
    fn decode_with_limits_fire_before_allocation() {
        let bytes = file(8, 8, &samples(8, 8));
        let o = DecodeOptions::default().with_max_width(7u32);
        assert!(matches!(
            decode_with(&bytes, &o),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        let o = DecodeOptions::default().with_max_height(7u32);
        assert!(matches!(
            decode_with(&bytes, &o),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        let o = DecodeOptions::default().with_max_pixels(63u64);
        assert!(matches!(
            decode_with(&bytes, &o),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        let o = DecodeOptions::default().with_max_bytes(511u64);
        assert!(matches!(
            decode_with(&bytes, &o),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        assert!(matches!(
            decode_rgba16_with(&bytes, &o),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        let o = DecodeOptions::default()
            .with_max_pixels(64u64)
            .with_max_bytes(512u64);
        assert!(decode_with(&bytes, &o).is_ok());
        // A hostile header claiming 60000×60000 (28.8 GB) on a 16-byte
        // file trips the default 1 GiB cap without touching the
        // allocator; lifting the cap falls through to the length check.
        let hostile = file(60000, 60000, &[]);
        assert!(matches!(
            decode(&hostile),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        assert!(matches!(
            decode_with(&hostile, &DecodeOptions::default().unlimited()),
            Err(FarbfeldError::InvalidData(_))
        ));
        // Overflowing u64 is still a clean LimitExceeded under the default.
        let hostile = file(u32::MAX, u32::MAX, &[]);
        assert!(matches!(
            decode(&hostile),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        // And a clean Unsupported / InvalidData (never a panic) unlimited.
        assert!(decode_with(&hostile, &DecodeOptions::default().unlimited()).is_err());
    }

    #[test]
    fn decode_from_streams_and_encode_to_matches_encode() {
        let s = samples(6, 5);
        let bytes = file(6, 5, &s);
        let img = decode_from(std::io::Cursor::new(&bytes)).unwrap();
        assert_eq!(img, decode(&bytes).unwrap());
        let mut out = Vec::new();
        encode_to(&img, &EncodeOptions::default(), &mut out).unwrap();
        assert_eq!(out, bytes);
        // Limits apply before the plane is allocated.
        assert!(matches!(
            decode_from_with(
                std::io::Cursor::new(&bytes),
                &DecodeOptions::default().with_max_pixels(29u64)
            ),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        // Truncated body is InvalidData, not Io.
        assert!(matches!(
            decode_from(std::io::Cursor::new(&bytes[..bytes.len() - 3])),
            Err(FarbfeldError::InvalidData(_))
        ));
        // Trailing bytes after the body are left unread.
        let mut long = bytes.clone();
        long.extend_from_slice(b"tail");
        let mut cur = std::io::Cursor::new(&long);
        assert_eq!(decode_from(&mut cur).unwrap(), img);
        assert_eq!(cur.position() as usize, bytes.len());
        // Zero-width / zero-height stream.
        for (w, h) in [(0u32, 4u32), (4, 0), (0, 0)] {
            let z = file(w, h, &[]);
            let zi = decode_from(std::io::Cursor::new(&z)).unwrap();
            assert_eq!((zi.width, zi.height), (w, h));
            let mut o = Vec::new();
            encode_to(&zi, &EncodeOptions::default(), &mut o).unwrap();
            assert_eq!(o, z);
        }

        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("boom"))
            }
        }
        assert!(matches!(decode_from(Failing), Err(FarbfeldError::Io(_))));
        struct Sink;
        impl Write for Sink {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(matches!(
            encode_to(&img, &EncodeOptions::default(), Sink),
            Err(FarbfeldError::Io(_))
        ));
    }

    #[test]
    #[allow(deprecated)]
    fn deprecated_wrappers_agree_with_the_contract_path() {
        let s = samples(6, 4);
        let bytes = file(6, 4, &s);
        let old = crate::parse_farbfeld(&bytes).unwrap();
        assert_eq!(old, decode_rgba16(&bytes).unwrap());
        assert_eq!(FarbfeldImage::from(old.clone()), decode(&bytes).unwrap());
        assert_eq!(crate::encode_farbfeld_image(&old).unwrap(), bytes);
        let quads: Vec<[u16; 4]> = s
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        assert_eq!(
            crate::encode_farbfeld_from_rgba16(6, 4, &quads).unwrap(),
            bytes
        );
        assert_eq!(crate::encode_farbfeld(6, 4, &bytes[16..]).unwrap(), bytes);
        let h = crate::parse_farbfeld_header(&bytes).unwrap();
        assert_eq!(h, crate::peek_farbfeld_dimensions(&bytes[..16]).unwrap());
        assert_eq!(h, info(&bytes).unwrap().header().unwrap());
        // The old parser was unlimited.
        let big = file(12000, 12000, &[]);
        assert!(matches!(
            crate::parse_farbfeld(&big),
            Err(FarbfeldError::InvalidData(_))
        ));
    }
}
