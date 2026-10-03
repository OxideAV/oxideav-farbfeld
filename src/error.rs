//! Crate-local error type used by `oxideav-farbfeld`'s standalone (no
//! `oxideav-core`) public API.
//!
//! When the `registry` feature is enabled, [`FarbfeldError`] gains a
//! `From<FarbfeldError> for oxideav_core::Error` impl (defined in
//! `crate::registry`) so the trait-side surface (`Decoder` /
//! `Encoder`) can keep returning `oxideav_core::Result<T>` while the
//! underlying parse/encode functions stay framework-free.

use core::fmt;

/// `Result` alias scoped to `oxideav-farbfeld`. Standalone (no
/// `oxideav-core`) callers see this; framework callers convert via the
/// gated `From<FarbfeldError> for oxideav_core::Error` impl.
pub type Result<T> = core::result::Result<T, FarbfeldError>;

/// The contract name for [`FarbfeldError`].
pub type Error = FarbfeldError;

/// Error variants returned by `oxideav-farbfeld`'s standalone API.
///
/// farbfeld is a fixed-layout format with no optional parts, so the
/// decoder fails only on malformed bytes (wrong magic, short header,
/// body length not equal to `width × height × 8`), on a
/// [`crate::DecodeOptions`] limit, on an image whose byte count the
/// platform cannot address, or on a failing `Read` / `Write`. The
/// encoder fails on caller-supplied images whose geometry does not
/// match their pixel buffer.
#[derive(Debug)]
#[non_exhaustive]
pub enum FarbfeldError {
    /// Input bytes don't match the spec — wrong magic, truncated body,
    /// trailing bytes past the announced body, or a caller-assembled
    /// image / row buffer whose length disagrees with its geometry.
    InvalidData(String),
    /// The input is well-formed but this crate cannot handle it: a
    /// pixel layout farbfeld cannot carry, or `width × height × 8`
    /// overflowing `usize` on this host.
    Unsupported(String),
    /// A [`crate::DecodeOptions`] limit (dimensions / pixels / bytes)
    /// would be exceeded; nothing was allocated.
    LimitExceeded(String),
    /// A read / write on a caller-supplied stream failed
    /// ([`crate::decode_from`] / [`crate::encode_to`] and the
    /// [`crate::FarbfeldStreamReader`] / [`crate::FarbfeldStreamWriter`]
    /// pair). A premature end of input is reported as
    /// [`FarbfeldError::InvalidData`] (truncated file), not `Io`.
    Io(std::io::Error),
}

impl FarbfeldError {
    /// Construct a [`FarbfeldError::InvalidData`] from a stringy message.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }

    /// Construct a [`FarbfeldError::Unsupported`] from a stringy message.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    /// Construct a [`FarbfeldError::LimitExceeded`] from a stringy message.
    pub fn limit(msg: impl Into<String>) -> Self {
        Self::LimitExceeded(msg.into())
    }
}

impl From<std::io::Error> for FarbfeldError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Display for FarbfeldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(s) => write!(f, "invalid data: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
            Self::LimitExceeded(s) => write!(f, "limit exceeded: {s}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for FarbfeldError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_prefixes_each_variant() {
        assert_eq!(FarbfeldError::invalid("x").to_string(), "invalid data: x");
        assert_eq!(
            FarbfeldError::unsupported("x").to_string(),
            "unsupported: x"
        );
        assert_eq!(FarbfeldError::limit("x").to_string(), "limit exceeded: x");
        let io: FarbfeldError = std::io::Error::other("boom").into();
        assert!(matches!(io, FarbfeldError::Io(_)));
        assert_eq!(io.to_string(), "io: boom");
        assert!(std::error::Error::source(&io).is_some());
        assert!(std::error::Error::source(&FarbfeldError::invalid("x")).is_none());
    }
}
