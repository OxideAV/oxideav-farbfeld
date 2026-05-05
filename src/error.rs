//! Crate-local error type used by `oxideav-farbfeld`'s standalone (no
//! `oxideav-core`) public API.
//!
//! When the `registry` feature is enabled, [`FarbfeldError`] gains a
//! `From<FarbfeldError> for oxideav_core::Error` impl (defined in
//! [`crate::registry`]) so the trait-side surface (`Decoder` /
//! `Encoder`) can keep returning `oxideav_core::Result<T>` while the
//! underlying parse/encode functions stay framework-free.

use core::fmt;

/// `Result` alias scoped to `oxideav-farbfeld`. Standalone (no
/// `oxideav-core`) callers see this; framework callers convert via the
/// gated `From<FarbfeldError> for oxideav_core::Error` impl.
pub type Result<T> = core::result::Result<T, FarbfeldError>;

/// Error variants returned by `oxideav-farbfeld`'s standalone API.
///
/// The format is so simple it has only two failure modes:
/// * a malformed or truncated byte stream — surfaced as
///   [`FarbfeldError::InvalidData`];
/// * a caller-supplied (width, height) that would overflow the address
///   space — surfaced as [`FarbfeldError::InvalidData`] too, since the
///   underlying byte stream would be unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FarbfeldError {
    /// Input bytes don't match the spec — wrong magic, truncated body,
    /// dimensions that would overflow `usize`, or a row/pixel count
    /// mismatch.
    InvalidData(String),
}

impl FarbfeldError {
    /// Construct a [`FarbfeldError::InvalidData`] from a stringy message.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }
}

impl fmt::Display for FarbfeldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(s) => write!(f, "invalid data: {s}"),
        }
    }
}

impl std::error::Error for FarbfeldError {}
