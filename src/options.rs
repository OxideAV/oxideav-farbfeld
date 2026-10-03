//! Decode-side limits ([`DecodeOptions`]) and the (empty) encode-side
//! knob record ([`EncodeOptions`]) of the standalone API.

use crate::error::{FarbfeldError, Result};
use crate::parser::BYTES_PER_PIXEL;

/// Limits and strictness for [`crate::decode_with`] /
/// [`crate::decode_from_with`].
///
/// Every limit is checked against the 16-byte header **before** any
/// pixel buffer is allocated, so a hostile header fails with
/// [`FarbfeldError::LimitExceeded`] instead of committing memory. The
/// defaults are: no dimension / pixel-count limit, decoded plane capped
/// at [`DecodeOptions::DEFAULT_MAX_BYTES`] (1 GiB), `strict = false`.
///
/// `strict` is accepted for contract uniformity but has no effect on
/// farbfeld: the format has no advisory ("should") rules — the magic
/// and the exact body length are mandatory and are enforced in both
/// modes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeOptions {
    /// Reject images wider than this (pixels).
    pub max_width: Option<u32>,
    /// Reject images taller than this (pixels).
    pub max_height: Option<u32>,
    /// Reject images with more than this many pixels (`width ×
    /// height`).
    pub max_pixels: Option<u64>,
    /// Reject images whose decoded plane would exceed this many bytes
    /// (`width × height × 8` — farbfeld decodes 1:1 to `Rgba64Le`).
    pub max_bytes: Option<u64>,
    /// No effect for farbfeld (see the type docs); kept for the
    /// contract.
    pub strict: bool,
}

impl DecodeOptions {
    /// Default [`Self::max_bytes`]: 1 GiB of decoded plane.
    pub const DEFAULT_MAX_BYTES: u64 = 1 << 30;

    /// The defaults (see the type docs).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or lift with `None`) the width limit.
    pub fn with_max_width(mut self, max_width: impl Into<Option<u32>>) -> Self {
        self.max_width = max_width.into();
        self
    }

    /// Set (or lift with `None`) the height limit.
    pub fn with_max_height(mut self, max_height: impl Into<Option<u32>>) -> Self {
        self.max_height = max_height.into();
        self
    }

    /// Set (or lift with `None`) the pixel-count limit.
    pub fn with_max_pixels(mut self, max_pixels: impl Into<Option<u64>>) -> Self {
        self.max_pixels = max_pixels.into();
        self
    }

    /// Set (or lift with `None`) the decoded-bytes limit.
    pub fn with_max_bytes(mut self, max_bytes: impl Into<Option<u64>>) -> Self {
        self.max_bytes = max_bytes.into();
        self
    }

    /// Set strict mode (no effect for farbfeld; see the type docs).
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Lift every limit (`max_*` all `None`).
    pub fn unlimited(mut self) -> Self {
        self.max_width = None;
        self.max_height = None;
        self.max_pixels = None;
        self.max_bytes = None;
        self
    }

    /// Check a header's geometry against the limits. The decoded plane
    /// is `width × height × 8` bytes; a product that overflows `u64`
    /// exceeds any finite byte limit.
    pub(crate) fn check(&self, width: u32, height: u32) -> Result<()> {
        if let Some(m) = self.max_width {
            if width > m {
                return Err(FarbfeldError::limit(format!(
                    "farbfeld: width {width} exceeds max_width {m}"
                )));
            }
        }
        if let Some(m) = self.max_height {
            if height > m {
                return Err(FarbfeldError::limit(format!(
                    "farbfeld: height {height} exceeds max_height {m}"
                )));
            }
        }
        let pixels = u64::from(width) * u64::from(height);
        if let Some(m) = self.max_pixels {
            if pixels > m {
                return Err(FarbfeldError::limit(format!(
                    "farbfeld: {pixels} pixels exceed max_pixels {m}"
                )));
            }
        }
        if let Some(m) = self.max_bytes {
            match pixels.checked_mul(BYTES_PER_PIXEL as u64) {
                Some(bytes) if bytes <= m => {}
                Some(bytes) => {
                    return Err(FarbfeldError::limit(format!(
                        "farbfeld: decoded plane of {bytes} bytes exceeds max_bytes {m}"
                    )))
                }
                None => {
                    return Err(FarbfeldError::limit(format!(
                        "farbfeld: decoded plane of {pixels} × {BYTES_PER_PIXEL} bytes exceeds max_bytes {m}"
                    )))
                }
            }
        }
        Ok(())
    }
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            max_width: None,
            max_height: None,
            max_pixels: None,
            max_bytes: Some(Self::DEFAULT_MAX_BYTES),
            strict: false,
        }
    }
}

/// Encoder knobs for [`crate::encode`] / [`crate::encode_rgb8`] /
/// [`crate::encode_rgba8`] / [`crate::encode_rgba16`].
///
/// farbfeld has none: the file is the header plus the raw big-endian
/// sample array, with no compression level, no colour tag and no
/// metadata slot. The record exists so the crate has the same call
/// shape as every other OxideAV image crate; it is `#[non_exhaustive]`
/// so a field can be added without a breaking change. Construct it with
/// [`EncodeOptions::default`] / [`EncodeOptions::new`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct EncodeOptions {}

impl EncodeOptions {
    /// The (only) options: none.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_fire_in_order() {
        let o = DecodeOptions::default()
            .with_max_width(10u32)
            .with_max_height(10u32)
            .with_max_pixels(50u64)
            .with_max_bytes(400u64);
        assert!(o.check(5, 5).is_ok());
        assert!(matches!(
            o.check(11, 1),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        assert!(matches!(
            o.check(1, 11),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        assert!(matches!(
            o.check(8, 8),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        // 7 × 7 = 49 pixels (under max_pixels) × 8 = 392 bytes (under
        // max_bytes); 10 × 5 = 50 pixels × 8 = 400 bytes is exactly at
        // both limits; 10 × 5 with max_bytes 399 trips the byte cap.
        assert!(o.check(7, 7).is_ok());
        assert!(o.check(10, 5).is_ok());
        assert!(matches!(
            o.clone().with_max_bytes(399u64).check(10, 5),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        assert!(o.unlimited().check(u32::MAX, u32::MAX).is_ok());
    }

    #[test]
    fn default_caps_bytes_only_and_overflow_counts_as_exceeded() {
        let d = DecodeOptions::default();
        assert_eq!(d.max_width, None);
        assert_eq!(d.max_height, None);
        assert_eq!(d.max_pixels, None);
        assert_eq!(d.max_bytes, Some(1 << 30));
        assert!(!d.strict);
        // u32::MAX² × 8 overflows u64 — still a clean LimitExceeded.
        assert!(matches!(
            d.check(u32::MAX, u32::MAX),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        assert!(matches!(
            DecodeOptions::new()
                .with_max_bytes(u64::MAX)
                .check(u32::MAX, u32::MAX),
            Err(FarbfeldError::LimitExceeded(_))
        ));
        assert!(d.with_strict(true).strict);
    }

    #[test]
    fn encode_options_are_empty_and_default() {
        assert_eq!(EncodeOptions::new(), EncodeOptions::default());
    }
}
