//! Pure-Rust farbfeld reader/writer.
//!
//! farbfeld is suckless's minimalist lossless image format. The
//! `farbfeld(5)` man page is the entire spec; in summary:
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
//! ## Standalone vs registry-integrated
//!
//! The default `registry` Cargo feature pulls in `oxideav-core` and
//! exposes the framework `Decoder` / `Encoder` trait surface plus a
//! [`registry::register`] entry point. Disable the feature
//! (`default-features = false`) for an `oxideav-core`-free build that
//! still exposes the standalone [`parse_farbfeld`] /
//! [`encode_farbfeld`] / [`encode_farbfeld_from_rgba16`] /
//! [`encode_farbfeld_image`] API and the crate-local
//! [`FarbfeldImage`] / [`FarbfeldError`] types.
//!
//! ## Example
//!
//! ```
//! use oxideav_farbfeld::{encode_farbfeld_from_rgba16, parse_farbfeld};
//!
//! let pixels = [[0xFFFF, 0x0000, 0x0000, 0xFFFF]];
//! let bytes = encode_farbfeld_from_rgba16(1, 1, &pixels).unwrap();
//! let img = parse_farbfeld(&bytes).unwrap();
//! assert_eq!(img.width, 1);
//! assert_eq!(img.height, 1);
//! assert_eq!(img.pixels, [0xFFFF, 0x0000, 0x0000, 0xFFFF]);
//! ```

#[cfg(feature = "registry")]
pub mod container;
#[cfg(feature = "registry")]
pub mod decoder;
pub mod encoder;
#[cfg(feature = "registry")]
pub mod encoder_trait;
pub mod error;
pub mod image;
pub mod parser;
#[cfg(feature = "registry")]
pub mod registry;

/// Codec id for farbfeld image frames.
pub const CODEC_ID_STR: &str = "farbfeld";

pub use encoder::{encode_farbfeld, encode_farbfeld_from_rgba16, encode_farbfeld_image};
pub use error::{FarbfeldError, Result};
pub use image::FarbfeldImage;
pub use parser::{parse_farbfeld, BYTES_PER_PIXEL, HEADER_LEN, MAGIC};

#[cfg(feature = "registry")]
pub use registry::{register, register_codecs, register_containers};
