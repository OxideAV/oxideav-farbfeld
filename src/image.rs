//! Standalone image container returned by `oxideav-farbfeld`'s
//! framework-free decode API and accepted by the standalone encode API.
//!
//! Defined here (rather than reusing `oxideav_core::VideoFrame`) so the
//! crate can be built with the default `registry` feature off — i.e.
//! without depending on `oxideav-core` at all. When the `registry`
//! feature is on the [`crate::registry`] module bridges between this
//! type and `oxideav_core::VideoFrame`.
//!
//! Layout: every farbfeld pixel is exactly four 16-bit channels in
//! `R, G, B, A` order. The on-disk representation is big-endian
//! 16-bit-per-sample, so a fully decoded image carries
//! `width * height * 4` `u16` values in [`FarbfeldImage::pixels`] —
//! flat row-major, with channel order matching the file.

/// One decoded farbfeld frame, framework-free shape.
///
/// The pixel buffer is flat row-major: pixel `(x, y)` starts at index
/// `(y * width + x) * 4`, with channels `[R, G, B, A]` as native-endian
/// `u16`. Big-endian conversion happens at the parser/encoder boundary
/// so callers always see the architecture's native word order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FarbfeldImage {
    /// Picture width in pixels — copied verbatim from the file header.
    pub width: u32,
    /// Picture height in pixels — copied verbatim from the file header.
    pub height: u32,
    /// Flat row-major pixel buffer, `width * height * 4` entries, in
    /// `R, G, B, A` order. Each value is a 16-bit channel sample in
    /// native endian (the parser converts from big-endian on read; the
    /// encoder converts back to big-endian on write).
    pub pixels: Vec<u16>,
}

impl FarbfeldImage {
    /// Construct a [`FarbfeldImage`] from raw 16-bit RGBA samples.
    /// Returns `None` if `pixels.len() != width * height * 4` — the
    /// only invariant the type carries.
    pub fn new(width: u32, height: u32, pixels: Vec<u16>) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))?;
        if pixels.len() != expected {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels,
        })
    }
}
