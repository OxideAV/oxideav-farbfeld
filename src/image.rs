//! The standalone image types: the shapes every `oxideav-<format>`
//! image crate shares (`IMAGE_CRATE_API`), specialised for farbfeld.
//!
//! * [`FarbfeldImage`] — the native-layout image [`crate::decode`]
//!   returns and [`crate::encode`] consumes: dimensions, the (single)
//!   [`PixelFormat`] tag `Rgba64Le`, one packed [`Plane`],
//!   [`ColorInfo`] and (always empty for farbfeld) [`Metadata`].
//! * [`RgbImage`] / [`RgbaImage`] — the tightly packed 8-bit raw
//!   paths ([`crate::decode_rgb8`] / [`crate::decode_rgba8`],
//!   [`FarbfeldImage::to_rgb8`] / [`FarbfeldImage::to_rgba8`]).
//! * [`ImageInfo`] — what [`crate::info`] reads from the 16-byte
//!   header.
//!
//! The 16-bit sample view ([`crate::Rgba16Image`]) lives in
//! `crate::rgba16` and converts losslessly to and from
//! [`FarbfeldImage`].
//!
//! Defined here (rather than reusing `oxideav_core::VideoFrame`) so the
//! crate can be built with the default `registry` feature off — i.e.
//! without depending on `oxideav-core` at all. When the `registry`
//! feature is on the `crate::registry` module provides the
//! conversions used by the trait-side `Decoder` / `Encoder` impls.
//!
//! ## Native layout: the wire is big-endian, the plane little-endian
//!
//! farbfeld stores every sample as a big-endian `u16`. The contract
//! image carries the samples in `oxideav_core::PixelFormat::Rgba64Le`
//! order — the same `R, G, B, A` 16-bit quads, each sample
//! **little-endian** — because that is the layout core and the rest of
//! the fleet speak. The decoder byte-swaps on the way in and the encoder
//! on the way out; both swaps are exact, so `decode(encode(img)) == img`
//! holds byte for byte and the on-disk file is bit-identical across a
//! round trip.

use std::borrow::Cow;

use crate::error::{FarbfeldError, Result};
use crate::parser::{FarbfeldHeader, BYTES_PER_PIXEL, HEADER_LEN};
use crate::rgba16::Rgba16Image;

// ---------------------------------------------------------------------------
// Pixel format
// ---------------------------------------------------------------------------

/// Pixel layouts the standalone `oxideav-farbfeld` API can produce /
/// consume.
///
/// Variant names mirror `oxideav_core::PixelFormat` exactly, so the
/// `crate::registry` conversion layer is a 1:1 match. farbfeld has
/// exactly one layout: 16-bit RGBA, one packed plane, samples
/// little-endian in memory (big-endian on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum FarbfeldPixelFormat {
    /// 16-bit RGBA, 8 bytes per pixel, each sample little-endian.
    #[default]
    Rgba64Le,
}

/// The contract name for [`FarbfeldPixelFormat`].
pub type PixelFormat = FarbfeldPixelFormat;

impl FarbfeldPixelFormat {
    /// Bytes per pixel (always 8).
    pub const fn bytes_per_pixel(self) -> usize {
        BYTES_PER_PIXEL
    }

    /// `true` — the only layout carries alpha.
    pub const fn has_alpha(self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Plane / colour / metadata records
// ---------------------------------------------------------------------------

/// One pixel plane: `stride` bytes per row, `data` holding at least
/// `stride × (height − 1) + width × 8` bytes (rows may carry padding
/// past the visible width). farbfeld's layout is packed, so a
/// [`FarbfeldImage`] has exactly one.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Plane {
    /// Bytes per row.
    pub stride: usize,
    /// Row-major bytes.
    pub data: Vec<u8>,
}

impl Plane {
    /// Wrap a plane buffer with its row stride.
    pub fn new(stride: usize, data: Vec<u8>) -> Self {
        Self { stride, data }
    }
}

/// Nominal sample range (H.273 `VideoFullRangeFlag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorRange {
    /// No range was signalled.
    #[default]
    Unspecified,
    /// Limited (video / studio) range: `VideoFullRangeFlag == 0`.
    Limited,
    /// Full (PC) range: `VideoFullRangeFlag == 1`.
    Full,
}

/// Colour signalling of an image: the sample range plus the H.273
/// `ColourPrimaries` / `TransferCharacteristics` /
/// `MatrixCoefficients` code points (`2` = unspecified).
///
/// farbfeld carries no colour tag of any kind (the staged format
/// description records integer sample values only), so
/// [`crate::decode`] fills the crate's documented convention,
/// [`ColorInfo::farbfeld_default`]: sRGB — full range, BT.709 / sRGB
/// primaries (1), sRGB transfer (13), identity matrix (0). The encoder
/// cannot write any of it back; a caller-set `color` is accepted and
/// ignored on encode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ColorInfo {
    /// Sample range.
    pub range: ColorRange,
    /// H.273 `ColourPrimaries` code point (`1` = BT.709 / sRGB, `2` =
    /// unspecified).
    pub primaries: u8,
    /// H.273 `TransferCharacteristics` code point (`13` = sRGB, `2` =
    /// unspecified).
    pub transfer: u8,
    /// H.273 `MatrixCoefficients` code point (`0` = identity / RGB).
    pub matrix: u8,
}

impl ColorInfo {
    /// H.273 "unspecified" code point.
    pub const UNSPECIFIED: u8 = 2;
    /// H.273 `MatrixCoefficients` identity (RGB / GBR) code point.
    pub const MATRIX_IDENTITY: u8 = 0;
    /// H.273 `ColourPrimaries` BT.709 / sRGB code point.
    pub const PRIMARIES_BT709: u8 = 1;
    /// H.273 `TransferCharacteristics` IEC 61966-2-1 sRGB code point.
    pub const TRANSFER_SRGB: u8 = 13;

    /// Build a description from its four parts.
    pub const fn new(range: ColorRange, primaries: u8, transfer: u8, matrix: u8) -> Self {
        Self {
            range,
            primaries,
            transfer,
            matrix,
        }
    }

    /// Every field unspecified.
    pub const fn unspecified() -> Self {
        Self::new(
            ColorRange::Unspecified,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
        )
    }

    /// sRGB (IEC 61966-2-1): BT.709 primaries, sRGB transfer, identity
    /// matrix, full range.
    pub const fn srgb() -> Self {
        Self::new(
            ColorRange::Full,
            Self::PRIMARIES_BT709,
            Self::TRANSFER_SRGB,
            Self::MATRIX_IDENTITY,
        )
    }

    /// farbfeld's documented convention: [`ColorInfo::srgb`]. The
    /// format itself carries no colour tag.
    pub const fn farbfeld_default() -> Self {
        Self::srgb()
    }

    /// Set the range.
    pub fn with_range(mut self, range: ColorRange) -> Self {
        self.range = range;
        self
    }

    /// Set the primaries code point.
    pub fn with_primaries(mut self, primaries: u8) -> Self {
        self.primaries = primaries;
        self
    }

    /// Set the transfer code point.
    pub fn with_transfer(mut self, transfer: u8) -> Self {
        self.transfer = transfer;
        self
    }

    /// Set the matrix code point.
    pub fn with_matrix(mut self, matrix: u8) -> Self {
        self.matrix = matrix;
        self
    }

    /// `true` when both primaries and transfer are specified (`!= 2`).
    pub fn is_specified(&self) -> bool {
        self.primaries != Self::UNSPECIFIED && self.transfer != Self::UNSPECIFIED
    }
}

impl Default for ColorInfo {
    /// [`ColorInfo::farbfeld_default`].
    fn default() -> Self {
        Self::farbfeld_default()
    }
}

/// The metadata blobs every image crate surfaces: an ICC profile, an
/// Exif payload, an XMP packet and a file gamma.
///
/// farbfeld has no metadata mechanism of any kind, so every field is
/// `None` on a decoded image and the encoder ignores (cannot carry)
/// whatever a caller sets. The type exists so [`FarbfeldImage`] has the
/// same shape as every other image crate's image.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Metadata {
    /// ICC profile bytes. Always `None` from the decoder.
    pub icc: Option<Vec<u8>>,
    /// Exif payload. Always `None` from the decoder.
    pub exif: Option<Vec<u8>>,
    /// XMP packet. Always `None` from the decoder.
    pub xmp: Option<Vec<u8>>,
    /// File gamma. Always `None` from the decoder.
    pub gamma: Option<f32>,
}

impl Metadata {
    /// Empty metadata.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or clear) the ICC profile.
    pub fn with_icc(mut self, icc: impl Into<Option<Vec<u8>>>) -> Self {
        self.icc = icc.into();
        self
    }

    /// Set (or clear) the Exif payload.
    pub fn with_exif(mut self, exif: impl Into<Option<Vec<u8>>>) -> Self {
        self.exif = exif.into();
        self
    }

    /// Set (or clear) the XMP packet.
    pub fn with_xmp(mut self, xmp: impl Into<Option<Vec<u8>>>) -> Self {
        self.xmp = xmp.into();
        self
    }

    /// Set (or clear) the file gamma.
    pub fn with_gamma(mut self, gamma: impl Into<Option<f32>>) -> Self {
        self.gamma = gamma.into();
        self
    }

    /// `true` when no field is set.
    pub fn is_empty(&self) -> bool {
        self.icc.is_none() && self.exif.is_none() && self.xmp.is_none() && self.gamma.is_none()
    }
}

// ---------------------------------------------------------------------------
// FarbfeldImage
// ---------------------------------------------------------------------------

/// Decoded farbfeld image in its native layout, as returned by
/// [`crate::decode`] and consumed by [`crate::encode`].
///
/// `planes` holds exactly one packed `Rgba64Le` plane (stride `width ×
/// 8` from the decoder, samples little-endian — see the module docs);
/// `color` is the sRGB convention ([`ColorInfo::farbfeld_default`]);
/// `metadata` is always empty (farbfeld has none). farbfeld has no
/// palette, so there is no `palette` field. Zero dimensions are legal
/// (a 16-byte file).
///
/// Construct with [`FarbfeldImage::new`] / [`FarbfeldImage::packed`] /
/// [`FarbfeldImage::from_rgb8`] / [`FarbfeldImage::from_rgba8`] /
/// [`FarbfeldImage::from_rgba16`], which validate the plane geometry
/// so an inconsistent image cannot exist and [`FarbfeldImage::to_rgb8`]
/// / [`FarbfeldImage::to_rgba8`] are infallible.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct FarbfeldImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Native pixel layout (always `Rgba64Le`).
    pub format: PixelFormat,
    /// Pixel planes — exactly one for farbfeld.
    pub planes: Vec<Plane>,
    /// Colour signalling (range + H.273 code points): the sRGB
    /// convention, since the file carries none.
    pub color: ColorInfo,
    /// ICC / Exif / XMP / gamma — always empty for farbfeld.
    pub metadata: Metadata,
}

impl FarbfeldImage {
    /// Assemble an image from its geometry, layout and planes (exactly
    /// one for farbfeld). Colour is [`ColorInfo::farbfeld_default`]
    /// (sRGB) and metadata empty; the `with_*` builders fill those in.
    ///
    /// Validates the geometry and returns [`FarbfeldError::InvalidData`]
    /// when there is not exactly one plane, when the plane's `stride`
    /// is below `width × 8`, or when its `data` is shorter than `stride
    /// × (height − 1) + width × 8` (nothing is required for `height ==
    /// 0`). [`FarbfeldError::Unsupported`] when the geometry overflows
    /// `usize`.
    pub fn new(width: u32, height: u32, format: PixelFormat, planes: Vec<Plane>) -> Result<Self> {
        if planes.len() != 1 {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld: expected exactly one packed plane, got {}",
                planes.len()
            )));
        }
        let row_bytes = (width as usize)
            .checked_mul(format.bytes_per_pixel())
            .ok_or_else(|| FarbfeldError::unsupported("farbfeld: row size overflows usize"))?;
        let plane = &planes[0];
        if plane.stride < row_bytes {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld: stride {} below row size {row_bytes}",
                plane.stride
            )));
        }
        let needed = if height == 0 {
            0
        } else {
            plane
                .stride
                .checked_mul(height as usize - 1)
                .and_then(|n| n.checked_add(row_bytes))
                .ok_or_else(|| FarbfeldError::unsupported("farbfeld: plane size overflows usize"))?
        };
        if plane.data.len() < needed {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld: plane holds {} bytes, {width}×{height} geometry needs {needed}",
                plane.data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            format,
            planes,
            color: ColorInfo::farbfeld_default(),
            metadata: Metadata::default(),
        })
    }

    /// One packed plane with an explicit row stride (`stride ≥ width ×
    /// 8`). Same validation as [`Self::new`].
    pub fn packed(
        width: u32,
        height: u32,
        format: PixelFormat,
        stride: usize,
        data: Vec<u8>,
    ) -> Result<Self> {
        Self::new(width, height, format, vec![Plane::new(stride, data)])
    }

    /// Tightly packed `Rgba64Le` from `8 × width × height` bytes (more
    /// is tolerated; fewer is [`FarbfeldError::InvalidData`]).
    pub fn from_rgba64le(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        let stride = (width as usize)
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or_else(|| FarbfeldError::unsupported("farbfeld: row size overflows usize"))?;
        Self::packed(width, height, PixelFormat::Rgba64Le, stride, data)
    }

    /// Build from tightly packed 8-bit RGB (`3 × width × height` bytes;
    /// fewer is [`FarbfeldError::InvalidData`], more is tolerated).
    ///
    /// farbfeld is 16-bit only, so every 8-bit sample `v` is widened to
    /// `v × 257` (`v << 8 | v`: `0 → 0`, `255 → 65535`) and alpha is set
    /// to `65535`. The widening is the exact inverse of the high-byte
    /// reduction [`Self::to_rgb8`] applies, so `to_rgb8(from_rgb8(x)) ==
    /// x`.
    pub fn from_rgb8(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        Self::from_8bit(width, height, &data, 3)
    }

    /// Build from tightly packed 8-bit RGBA (`4 × width × height` bytes;
    /// fewer is [`FarbfeldError::InvalidData`], more is tolerated).
    /// Every sample, alpha included, is widened to `v × 257` (see
    /// [`Self::from_rgb8`]).
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        Self::from_8bit(width, height, &data, 4)
    }

    fn from_8bit(width: u32, height: u32, data: &[u8], bpp: usize) -> Result<Self> {
        let pixels = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| FarbfeldError::unsupported("farbfeld: pixel count overflows usize"))?;
        let need = pixels
            .checked_mul(bpp)
            .ok_or_else(|| FarbfeldError::unsupported("farbfeld: input size overflows usize"))?;
        if data.len() < need {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld: {width}×{height}×{bpp} needs {need} bytes, got {}",
                data.len()
            )));
        }
        let plane_len = pixels
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or_else(|| FarbfeldError::unsupported("farbfeld: plane size overflows usize"))?;
        let mut plane = vec![0u8; plane_len];
        for (src, dst) in data[..need]
            .chunks_exact(bpp)
            .zip(plane.chunks_exact_mut(BYTES_PER_PIXEL))
        {
            // `v × 257` in little-endian is simply `[v, v]`.
            dst[0] = src[0];
            dst[1] = src[0];
            dst[2] = src[1];
            dst[3] = src[1];
            dst[4] = src[2];
            dst[5] = src[2];
            let a = if bpp == 4 { src[3] } else { 0xFF };
            dst[6] = a;
            dst[7] = a;
        }
        Self::packed(
            width,
            height,
            PixelFormat::Rgba64Le,
            width as usize * BYTES_PER_PIXEL,
            plane,
        )
    }

    /// Build from native-endian 16-bit `[R, G, B, A]` samples
    /// (`4 × width × height` `u16`s, exactly; a mismatch is
    /// [`FarbfeldError::InvalidData`]). Lossless.
    pub fn from_rgba16(width: u32, height: u32, samples: &[u16]) -> Result<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| FarbfeldError::unsupported("farbfeld: sample count overflows usize"))?;
        if samples.len() != expected {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld: {width}×{height}×4 needs {expected} samples, got {}",
                samples.len()
            )));
        }
        let mut plane = vec![0u8; expected * 2];
        crate::encoder::encode_le_samples(samples, &mut plane);
        Self::packed(
            width,
            height,
            PixelFormat::Rgba64Le,
            width as usize * BYTES_PER_PIXEL,
            plane,
        )
    }

    /// Set the colour signalling. farbfeld cannot carry it; the encoder
    /// ignores it.
    pub fn with_color(mut self, color: ColorInfo) -> Self {
        self.color = color;
        self
    }

    /// Set the metadata. farbfeld cannot carry any of it; the encoder
    /// ignores it.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Native pixel layout (`Rgba64Le`).
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Number of bytes per pixel (8).
    pub fn bytes_per_pixel(&self) -> usize {
        self.format.bytes_per_pixel()
    }

    /// Row stride in bytes of the pixel plane.
    pub fn stride(&self) -> usize {
        self.planes.first().map(|p| p.stride).unwrap_or(0)
    }

    /// `true` — farbfeld always carries alpha.
    pub fn has_alpha(&self) -> bool {
        self.format.has_alpha()
    }

    /// `true` when the plane is tightly packed (`stride == width × 8`
    /// and no trailing bytes) — always the case for a decoder-produced
    /// image.
    pub fn is_tightly_packed(&self) -> bool {
        let row = self.width as usize * self.bytes_per_pixel();
        self.planes
            .first()
            .is_some_and(|p| p.stride == row && p.data.len() == row * self.height as usize)
    }

    /// The pixel bytes — `Some` for every farbfeld image (one packed
    /// plane). Includes row padding when the plane's stride exceeds
    /// `width × 8`.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        self.planes.first().map(|p| p.data.as_slice())
    }

    /// Consume the image and return its plane bytes.
    pub fn into_raw(self) -> Vec<u8> {
        let mut planes = self.planes.into_iter();
        let mut out = planes.next().map(|p| p.data).unwrap_or_default();
        for p in planes {
            out.extend_from_slice(&p.data);
        }
        out
    }

    /// Pixel bytes of the single plane (empty if none).
    pub(crate) fn data(&self) -> &[u8] {
        self.as_bytes().unwrap_or(&[])
    }

    /// The pixels as one tightly packed `width × height × 8` buffer: a
    /// borrow when the plane already is tightly packed, a repacked copy
    /// when it carries row padding.
    pub(crate) fn packed_pixels(&self) -> Cow<'_, [u8]> {
        let w = self.width as usize;
        let h = self.height as usize;
        let row = w * self.bytes_per_pixel();
        let stride = self.stride();
        let src = self.data();
        if stride == row && src.len() == row * h {
            return Cow::Borrowed(src);
        }
        let mut out = vec![0u8; row * h];
        if row > 0 {
            for (y, dst) in out.chunks_exact_mut(row).enumerate() {
                if let Some(s) = src.get(y * stride..y * stride + row) {
                    dst.copy_from_slice(s);
                }
            }
        }
        Cow::Owned(out)
    }

    /// The samples as a native-endian 16-bit view ([`Rgba16Image`]),
    /// row padding dropped. Lossless.
    pub fn to_rgba16(&self) -> Rgba16Image {
        let packed = self.packed_pixels();
        let mut data = vec![0u16; packed.len() / 2];
        for (chunk, slot) in packed.chunks_exact(2).zip(data.iter_mut()) {
            *slot = u16::from_le_bytes([chunk[0], chunk[1]]);
        }
        Rgba16Image {
            width: self.width,
            height: self.height,
            data,
        }
    }

    /// Tightly packed 8-bit RGBA, `4 × width` bytes per row.
    ///
    /// Each 16-bit sample reduces to its high byte (`v >> 8`, i.e.
    /// truncation: `65535 → 255`, `256 → 1`, `255 → 0`) — the same
    /// reduction the other 16-bit OxideAV image crates apply, and the
    /// exact inverse of the `× 257` widening in [`Self::from_rgba8`].
    /// No colour management is applied.
    pub fn to_rgba8(&self) -> Vec<u8> {
        self.reduce_to_8bit(4)
    }

    /// Tightly packed 8-bit RGB, `3 × width` bytes per row. Same
    /// high-byte reduction as [`Self::to_rgba8`]; alpha is dropped (no
    /// compositing: a transparent pixel keeps its colour samples).
    pub fn to_rgb8(&self) -> Vec<u8> {
        self.reduce_to_8bit(3)
    }

    fn reduce_to_8bit(&self, bpp: usize) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = vec![0u8; w * h * bpp];
        if w == 0 || h == 0 {
            return out;
        }
        let stride = self.stride();
        let src = self.data();
        let row_bytes = w * BYTES_PER_PIXEL;
        for y in 0..h {
            let Some(row) = src.get(y * stride..y * stride + row_bytes) else {
                break;
            };
            let dst = &mut out[y * w * bpp..(y + 1) * w * bpp];
            for (s, px) in row
                .chunks_exact(BYTES_PER_PIXEL)
                .zip(dst.chunks_exact_mut(bpp))
            {
                // Little-endian: the high byte of sample `i` is at
                // offset `2 * i + 1`.
                px[0] = s[1];
                px[1] = s[3];
                px[2] = s[5];
                if bpp == 4 {
                    px[3] = s[7];
                }
            }
        }
        out
    }
}

impl From<Rgba16Image> for FarbfeldImage {
    /// Lossless: the `u16` samples become one tightly packed `Rgba64Le`
    /// plane. Infallible because [`Rgba16Image`] already guarantees
    /// `data.len() == width × height × 4`.
    fn from(img: Rgba16Image) -> Self {
        let mut plane = vec![0u8; img.data.len() * 2];
        crate::encoder::encode_le_samples(&img.data, &mut plane);
        Self {
            width: img.width,
            height: img.height,
            format: PixelFormat::Rgba64Le,
            planes: vec![Plane::new(img.width as usize * BYTES_PER_PIXEL, plane)],
            color: ColorInfo::farbfeld_default(),
            metadata: Metadata::default(),
        }
    }
}

impl From<FarbfeldImage> for Rgba16Image {
    /// [`FarbfeldImage::to_rgba16`].
    fn from(img: FarbfeldImage) -> Self {
        img.to_rgba16()
    }
}

// ---------------------------------------------------------------------------
// RgbImage / RgbaImage
// ---------------------------------------------------------------------------

/// Tightly packed 8-bit RGB image: `width × height × 3` bytes,
/// row-major, no padding. What [`crate::decode_rgb8`] returns.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 3` bytes, R, G, B per pixel.
    pub data: Vec<u8>,
}

impl RgbImage {
    /// Wrap a packed RGB buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }
}

/// Tightly packed 8-bit RGBA image: `width × height × 4` bytes,
/// row-major, no padding. What [`crate::decode_rgba8`] returns.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbaImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes, R, G, B, A per pixel.
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// Wrap a packed RGBA buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }
}

// ---------------------------------------------------------------------------
// ImageInfo
// ---------------------------------------------------------------------------

/// What [`crate::info`] reads from the 16-byte header: dimensions, the
/// native layout, and the (fixed) alpha / frame / metadata facts of the
/// format.
///
/// `info` is header-only: the body is not inspected, so a well-formed
/// header on a truncated file still yields an `ImageInfo`; use
/// [`ImageInfo::file_len`] to check what the header announces against
/// what you hold.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImageInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Native layout — always `Rgba64Le`.
    pub format: PixelFormat,
    /// Number of images — always 1 (farbfeld has no animation).
    pub frames: u32,
    /// Always `true` (RGBA).
    pub has_alpha: bool,
    /// The sRGB convention ([`ColorInfo::farbfeld_default`]).
    pub color: ColorInfo,
    /// Always `false` — no ICC slot.
    pub has_icc: bool,
    /// Always `false` — no Exif slot.
    pub has_exif: bool,
    /// Always `false` — no XMP slot.
    pub has_xmp: bool,
}

impl ImageInfo {
    /// The info for a `width × height` farbfeld image (every other field
    /// is fixed by the format).
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: PixelFormat::Rgba64Le,
            frames: 1,
            has_alpha: true,
            color: ColorInfo::farbfeld_default(),
            has_icc: false,
            has_exif: false,
            has_xmp: false,
        }
    }

    /// Body bytes the header announces: `width × height × 8`. `None`
    /// when the product overflows `u64`.
    pub fn body_len(&self) -> Option<u64> {
        u64::from(self.width)
            .checked_mul(u64::from(self.height))?
            .checked_mul(BYTES_PER_PIXEL as u64)
    }

    /// Total file size the header announces: `16 + width × height ×
    /// 8`. `None` on `u64` overflow.
    pub fn file_len(&self) -> Option<u64> {
        self.body_len()?.checked_add(HEADER_LEN as u64)
    }

    /// The raw header view, for callers of the streaming reader.
    /// `Err` ([`FarbfeldError::Unsupported`]) when `width × height × 8`
    /// does not fit this host's `usize`.
    pub fn header(&self) -> Result<FarbfeldHeader> {
        FarbfeldHeader::new(self.width, self.height)
    }
}

impl From<&FarbfeldHeader> for ImageInfo {
    fn from(h: &FarbfeldHeader) -> Self {
        Self::new(h.width, h.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Vec<u16> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let base = (y * w + x) as u16;
                v.extend_from_slice(&[
                    base.wrapping_mul(1000),
                    base.wrapping_mul(2000).wrapping_add(0x1234),
                    0xFFFF - base,
                    0x8000 ^ base,
                ]);
            }
        }
        v
    }

    #[test]
    fn new_validates_geometry_and_accepts_zero_dimensions() {
        assert!(FarbfeldImage::new(1, 1, PixelFormat::Rgba64Le, vec![]).is_err());
        assert!(matches!(
            FarbfeldImage::new(
                1,
                1,
                PixelFormat::Rgba64Le,
                vec![Plane::new(8, vec![0; 8]), Plane::new(8, vec![0; 8])]
            ),
            Err(FarbfeldError::InvalidData(_))
        ));
        assert!(matches!(
            FarbfeldImage::packed(2, 1, PixelFormat::Rgba64Le, 8, vec![0; 16]),
            Err(FarbfeldError::InvalidData(_))
        ));
        assert!(matches!(
            FarbfeldImage::packed(2, 2, PixelFormat::Rgba64Le, 16, vec![0; 31]),
            Err(FarbfeldError::InvalidData(_))
        ));
        // Last row needs only `row_bytes`, not `stride`.
        assert!(FarbfeldImage::packed(2, 2, PixelFormat::Rgba64Le, 20, vec![0; 36]).is_ok());
        let z = FarbfeldImage::packed(0, 0, PixelFormat::Rgba64Le, 0, vec![]).unwrap();
        assert!(z.is_tightly_packed());
        assert!(z.to_rgba8().is_empty());
        let z = FarbfeldImage::packed(0, 7, PixelFormat::Rgba64Le, 0, vec![]).unwrap();
        assert_eq!(z.to_rgba16().data.len(), 0);
        let z = FarbfeldImage::packed(5, 0, PixelFormat::Rgba64Le, 40, vec![]).unwrap();
        assert!(z.to_rgb8().is_empty());
        assert!(z.is_tightly_packed());
    }

    #[test]
    fn from_rgba16_round_trips_through_to_rgba16() {
        let samples = gradient(5, 3);
        let img = FarbfeldImage::from_rgba16(5, 3, &samples).unwrap();
        assert_eq!(img.format(), PixelFormat::Rgba64Le);
        assert_eq!(img.stride(), 40);
        assert!(img.is_tightly_packed());
        assert_eq!(img.as_bytes().unwrap().len(), 120);
        // Little-endian in the plane.
        assert_eq!(&img.as_bytes().unwrap()[..2], &samples[0].to_le_bytes()[..]);
        let back = img.to_rgba16();
        assert_eq!(back.data, samples);
        assert_eq!(FarbfeldImage::from(back.clone()), img);
        assert_eq!(Rgba16Image::from(img.clone()), back);
        assert!(matches!(
            FarbfeldImage::from_rgba16(5, 3, &samples[1..]),
            Err(FarbfeldError::InvalidData(_))
        ));
    }

    #[test]
    fn eight_bit_widening_and_reduction_are_inverse() {
        let rgb: Vec<u8> = (0..=255u8).flat_map(|v| [v, 255 - v, v ^ 0x55]).collect();
        let img = FarbfeldImage::from_rgb8(256, 1, rgb.clone()).unwrap();
        let s = img.to_rgba16();
        assert_eq!(s.data[0..4], [0, 255 * 257, 0x55 * 257, 0xFFFF]);
        assert_eq!(s.data[4 * 255..4 * 256], [0xFFFF, 0, 0xAA * 257, 0xFFFF]);
        assert_eq!(img.to_rgb8(), rgb);
        let rgba_expect: Vec<u8> = rgb
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
        assert_eq!(img.to_rgba8(), rgba_expect);

        let rgba: Vec<u8> = (0..=255u8)
            .flat_map(|v| [v, v / 2, v / 3, 255 - v])
            .collect();
        let img = FarbfeldImage::from_rgba8(256, 1, rgba.clone()).unwrap();
        assert_eq!(img.to_rgba8(), rgba);
        let rgb_expect: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| p[..3].to_vec()).collect();
        assert_eq!(img.to_rgb8(), rgb_expect);
        assert!(matches!(
            FarbfeldImage::from_rgb8(2, 2, vec![0; 11]),
            Err(FarbfeldError::InvalidData(_))
        ));
        assert!(matches!(
            FarbfeldImage::from_rgba8(2, 2, vec![0; 15]),
            Err(FarbfeldError::InvalidData(_))
        ));
        // Extra bytes are tolerated.
        assert!(FarbfeldImage::from_rgba8(1, 1, vec![0; 5]).is_ok());
    }

    #[test]
    fn reduction_is_high_byte_truncation() {
        let img = FarbfeldImage::from_rgba16(
            4,
            1,
            &[
                0xFFFF, 0x0100, 0x00FF, 0x8000, //
                0x1234, 0xABCD, 0x0001, 0xFF00, //
                0, 0, 0, 0, //
                0x7FFF, 0x8001, 0xFFFE, 0x0080,
            ],
        )
        .unwrap();
        assert_eq!(
            img.to_rgba8(),
            [255, 1, 0, 0x80, 0x12, 0xAB, 0, 0xFF, 0, 0, 0, 0, 0x7F, 0x80, 0xFF, 0]
        );
    }

    #[test]
    fn padded_planes_repack_and_reduce_by_visible_width() {
        // 2×2 with 4 bytes of row padding.
        let mut data = Vec::new();
        for y in 0..2u16 {
            for x in 0..2u16 {
                let v = (y * 2 + x) * 0x1111;
                for c in 0..4u16 {
                    data.extend_from_slice(&(v + c).to_le_bytes());
                }
            }
            data.extend_from_slice(&[0xEE; 4]);
        }
        let img = FarbfeldImage::packed(2, 2, PixelFormat::Rgba64Le, 20, data).unwrap();
        assert!(!img.is_tightly_packed());
        let packed = img.packed_pixels();
        assert_eq!(packed.len(), 32);
        assert!(!packed.contains(&0xEE));
        let s = img.to_rgba16();
        assert_eq!(s.data.len(), 16);
        assert_eq!(s.data[4..8], [0x1111, 0x1112, 0x1113, 0x1114]);
        assert_eq!(img.to_rgb8().len(), 12);
        assert_eq!(img.to_rgba8()[4..8], [0x11, 0x11, 0x11, 0x11]);
        assert_eq!(img.clone().into_raw().len(), 40);
    }

    #[test]
    fn color_and_metadata_defaults() {
        let img = FarbfeldImage::from_rgba16(1, 1, &[1, 2, 3, 4]).unwrap();
        assert_eq!(img.color, ColorInfo::srgb());
        assert_eq!(img.color, ColorInfo::default());
        assert!(img.color.is_specified());
        assert!(!ColorInfo::unspecified().is_specified());
        assert!(img.metadata.is_empty());
        assert!(img.has_alpha());
        assert_eq!(img.bytes_per_pixel(), 8);
        let c = ColorInfo::unspecified()
            .with_range(ColorRange::Limited)
            .with_primaries(9)
            .with_transfer(16)
            .with_matrix(9);
        assert_eq!(c, ColorInfo::new(ColorRange::Limited, 9, 16, 9));
        let m = Metadata::new()
            .with_icc(vec![1])
            .with_exif(vec![2])
            .with_xmp(vec![3])
            .with_gamma(0.45455);
        assert!(!m.is_empty());
        let img = img.with_color(c).with_metadata(m.clone());
        assert_eq!(img.color, c);
        assert_eq!(img.metadata, m);
    }

    #[test]
    fn image_info_fixed_fields_and_lengths() {
        let i = ImageInfo::new(3, 4);
        assert_eq!(i.format, PixelFormat::Rgba64Le);
        assert_eq!(i.frames, 1);
        assert!(i.has_alpha && !i.has_icc && !i.has_exif && !i.has_xmp);
        assert_eq!(i.color, ColorInfo::farbfeld_default());
        assert_eq!(i.body_len(), Some(96));
        assert_eq!(i.file_len(), Some(112));
        assert_eq!(i.header().unwrap().body_len, 96);
        let big = ImageInfo::new(u32::MAX, u32::MAX);
        assert_eq!(big.body_len(), None);
        assert_eq!(big.file_len(), None);
        assert_eq!(ImageInfo::new(0, 9).file_len(), Some(16));
    }

    #[test]
    fn raw_records() {
        let r = RgbImage::new(1, 1, vec![1, 2, 3]);
        assert_eq!(r.as_bytes(), &[1, 2, 3]);
        assert_eq!(r.into_raw(), vec![1, 2, 3]);
        let r = RgbaImage::new(1, 1, vec![1, 2, 3, 4]);
        assert_eq!(r.as_bytes(), &[1, 2, 3, 4]);
        assert_eq!(r.into_raw(), vec![1, 2, 3, 4]);
    }
}
