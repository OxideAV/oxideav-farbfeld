//! `oxideav-core` integration layer for `oxideav-farbfeld`.
//!
//! Gated behind the default-on `registry` feature so image-library
//! consumers can depend on `oxideav-farbfeld` with `default-features =
//! false` and skip the `oxideav-core` dependency entirely.
//!
//! Exposes:
//! * [`register`] / [`register_codecs`] / [`register_containers`] — the
//!   `CodecRegistry` / `ContainerRegistry` entry points the umbrella
//!   `oxideav` crate calls during framework initialisation.
//! * The `From<FarbfeldError> for oxideav_core::Error` conversion that
//!   lets the trait-side `Decoder` / `Encoder` impls (in
//!   [`crate::decoder`] / [`crate::encoder_trait`]) bubble bitstream
//!   errors up through the framework error type.

use oxideav_core::ContainerRegistry;
use oxideav_core::{CodecCapabilities, CodecId, PixelFormat};
use oxideav_core::{CodecInfo, CodecRegistry};

use crate::container;
use crate::error::FarbfeldError;

impl From<FarbfeldError> for oxideav_core::Error {
    fn from(e: FarbfeldError) -> Self {
        match e {
            FarbfeldError::InvalidData(s) => oxideav_core::Error::InvalidData(s),
        }
    }
}

/// Register the farbfeld codec into the supplied [`CodecRegistry`].
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("farbfeld_sw")
        .with_intra_only(true)
        .with_lossless(true)
        // farbfeld u32 dimension fields permit anything up to u32::MAX,
        // but the umbrella registry caps at u16::MAX which is plenty
        // for any realistic raster.
        .with_max_size(65535, 65535)
        .with_pixel_formats(vec![PixelFormat::Rgba64Le]);
    reg.register(
        CodecInfo::new(CodecId::new(crate::CODEC_ID_STR))
            .capabilities(caps)
            .decoder(crate::decoder::make_decoder)
            .encoder(crate::encoder_trait::make_encoder),
    );
}

/// Register the farbfeld container demuxer + muxer + extension + probe
/// into the supplied [`ContainerRegistry`].
pub fn register_containers(reg: &mut ContainerRegistry) {
    container::register(reg);
}

/// Combined registration for callers that just want everything wired up
/// in one call.
pub fn register(codecs: &mut CodecRegistry, containers: &mut ContainerRegistry) {
    register_codecs(codecs);
    register_containers(containers);
}
