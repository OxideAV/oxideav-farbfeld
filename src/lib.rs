//! Pure-Rust farbfeld reader/writer.
//!
//! farbfeld is a minimalist lossless image format. The bytes-on-disk
//! description used by this crate lives in the workspace at
//! `docs/image/farbfeld/farbfeld-format.md` (independently authored
//! factual prose, not a mirror or paraphrase of the upstream man page);
//! in summary:
//!
//! ```text
//!   bytes  field
//!   -----  -----------------------------
//!       8  magic = ASCII "farbfeld"
//!       4  width  (u32 big-endian)
//!       4  height (u32 big-endian)
//!     8·N  pixels: width*height rows of 4×u16 BE = R, G, B, A
//! ```
//!
//! There is no compression, no per-pixel metadata, no animation —
//! every pixel is exactly four 16-bit channels in big-endian on disk,
//! laid out in row-major scan order.
//!
//! ## Standalone use (the image-crate API contract)
//!
//! The root exposes the vocabulary every `oxideav-<format>` image crate
//! shares (`IMAGE_CRATE_API`): [`probe`], [`info`], [`decode`] /
//! [`decode_with`] / [`decode_rgb8`] / [`decode_rgba8`] /
//! [`decode_from`], [`encode`] / [`encode_rgb8`] / [`encode_rgba8`] /
//! [`encode_to`], with [`FarbfeldImage`], [`RgbImage`] / [`RgbaImage`],
//! [`ImageInfo`], [`DecodeOptions`], [`EncodeOptions`], [`PixelFormat`]
//! and [`Error`]. It builds with `default-features = false` and no
//! `oxideav-core`.
//!
//! ```
//! use oxideav_farbfeld::{EncodeOptions, PixelFormat};
//!
//! // Round-trip a 2×1 RGBA image through encode → decode.
//! let rgba: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 128];
//! let bytes = oxideav_farbfeld::encode_rgba8(2, 1, &rgba, &EncodeOptions::default()).unwrap();
//! assert!(oxideav_farbfeld::probe(&bytes));
//!
//! let info = oxideav_farbfeld::info(&bytes).unwrap();           // header only
//! assert_eq!((info.width, info.height, info.format), (2, 1, PixelFormat::Rgba64Le));
//!
//! let img = oxideav_farbfeld::decode(&bytes).unwrap();          // native layout: Rgba64Le
//! assert_eq!(img.as_bytes().unwrap().len(), 2 * 1 * 8);         // one packed plane
//! assert_eq!(img.to_rgba8(), rgba);                             // high byte of each sample
//! assert_eq!(oxideav_farbfeld::decode_rgb8(&bytes).unwrap().data, [255, 0, 0, 0, 255, 0]);
//! ```
//!
//! ### Native layout
//!
//! farbfeld has exactly one layout: 16-bit RGBA. The wire is
//! big-endian; the contract image carries the samples as
//! `PixelFormat::Rgba64Le` (little-endian, the layout `oxideav-core`
//! speaks), byte-swapped on decode and encode — exactly, so
//! `decode(encode(img)) == img` byte for byte. 8-bit paths widen
//! `v → v × 257` on encode and take the high byte (`v >> 8`) on decode,
//! which are exact inverses. The native-endian `u16` view is
//! [`Rgba16Image`] via [`decode_rgba16`] / [`encode_rgba16`] /
//! [`FarbfeldImage::to_rgba16`].
//!
//! ### Colour and metadata
//!
//! The format carries no colour tag and no metadata slot. `color` is
//! the crate's documented convention, sRGB
//! ([`ColorInfo::farbfeld_default`]); `metadata` is always empty.
//!
//! ## Streaming
//!
//! [`FarbfeldStreamReader`] / [`FarbfeldStreamWriter`] decode / encode
//! one row at a time without holding the whole image in memory, and
//! [`decode_from`] / [`encode_to`] are built on them (the header and
//! the [`DecodeOptions`] limits are checked before the plane is
//! allocated; rows are read with a bounded `Read::take`).
//!
//! ## Framework use
//!
//! The default `registry` Cargo feature pulls in `oxideav-core` and
//! exposes `register(&mut RuntimeContext)`, `register_codecs` /
//! `register_containers`, the `make_decoder` / `make_encoder`
//! factories, and the frame bridge (`From<FarbfeldImage> for
//! VideoFrame`, `FarbfeldImage::from_video_frame`). The framework
//! `Decoder` / `Encoder` call the standalone functions above — one
//! implementation.

pub mod api;
#[cfg(feature = "registry")]
pub mod container;
#[cfg(feature = "registry")]
pub mod decoder;
pub mod encoder;
#[cfg(feature = "registry")]
pub mod encoder_trait;
pub mod error;
pub mod image;
pub mod options;
pub mod parser;
#[cfg(feature = "registry")]
pub mod registry;
pub mod rgba16;
pub mod stream;

/// Codec id for farbfeld image frames.
pub const CODEC_ID_STR: &str = "farbfeld";

// ---- the image-crate API contract ------------------------------------------
pub use api::{
    decode, decode_from, decode_from_with, decode_rgb8, decode_rgba16, decode_rgba16_with,
    decode_rgba8, decode_with, encode, encode_rgb8, encode_rgba16, encode_rgba8, encode_to, info,
    probe,
};
pub use error::{Error, FarbfeldError, Result};
pub use image::{
    ColorInfo, ColorRange, FarbfeldImage, FarbfeldPixelFormat, ImageInfo, Metadata, PixelFormat,
    Plane, RgbImage, RgbaImage,
};
pub use options::{DecodeOptions, EncodeOptions};

// ---- farbfeld depth APIs ----------------------------------------------------
pub use parser::{FarbfeldHeader, BYTES_PER_PIXEL, HEADER_LEN, MAGIC};
pub use rgba16::{Pixels, Rgba16Image, Rows, RowsMut, CHANNELS_PER_PIXEL};
pub use stream::{FarbfeldStreamReader, FarbfeldStreamWriter};

// ---- deprecated pre-contract entry points (one release) --------------------
#[allow(deprecated)]
pub use encoder::{encode_farbfeld, encode_farbfeld_from_rgba16, encode_farbfeld_image};
#[allow(deprecated)]
pub use parser::{parse_farbfeld, parse_farbfeld_header, peek_farbfeld_dimensions};

// ---- framework adapter -----------------------------------------------------
#[cfg(feature = "registry")]
pub use decoder::make_decoder;
#[cfg(feature = "registry")]
pub use encoder_trait::make_encoder;
#[cfg(feature = "registry")]
#[allow(deprecated)]
pub use registry::register_runtime;
#[cfg(feature = "registry")]
pub use registry::{register, register_codecs, register_containers, register_registries};

#[cfg(feature = "registry")]
#[doc(hidden)]
pub use registry::__oxideav_entry;
