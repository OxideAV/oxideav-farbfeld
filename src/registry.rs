//! `oxideav-core` integration layer for `oxideav-farbfeld`.
//!
//! Gated behind the default-on `registry` feature so image-library
//! consumers can depend on `oxideav-farbfeld` with `default-features =
//! false` and skip the `oxideav-core` dependency entirely.
//!
//! Exposes:
//! * [`register`] — the unified `RuntimeContext` entry point the
//!   umbrella `oxideav` crate (via `oxideav_meta::register_all` and the
//!   [`oxideav_core::register!`] macro) calls during framework
//!   initialisation; [`register_codecs`] / [`register_containers`] /
//!   [`register_registries`] are the per-registry pieces.
//! * `From<FarbfeldImage> for VideoFrame` and
//!   [`FarbfeldImage::from_video_frame`] — the frame bridge (one packed
//!   `Rgba64Le` plane + the colour-signal side-channel), plus the 1:1
//!   [`FarbfeldPixelFormat`] ↔ `PixelFormat` and [`ColorInfo`] ↔
//!   `ColorSignal` mappings.
//! * The `From<FarbfeldError> for oxideav_core::Error` conversion that
//!   lets the trait-side `Decoder` / `Encoder` impls (in
//!   `crate::decoder` / `crate::encoder_trait`) bubble bitstream errors
//!   up through the framework error type.

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry, ColorPrimaries,
    ColorSignal, ContainerRegistry, MatrixCoefficients, PixelFormat, RuntimeContext,
    TransferCharacteristics, VideoFrame, VideoPlane,
};

use crate::container;
use crate::error::{FarbfeldError, Result};
use crate::image::{ColorInfo, ColorRange, FarbfeldImage, FarbfeldPixelFormat};

/// Convert a [`FarbfeldError`] into the framework-shared
/// `oxideav_core::Error` so trait impls in this crate can use `?` on
/// errors returned by the framework-free decode/encode functions.
impl From<FarbfeldError> for oxideav_core::Error {
    fn from(e: FarbfeldError) -> Self {
        match e {
            FarbfeldError::InvalidData(s) => oxideav_core::Error::InvalidData(s),
            FarbfeldError::Unsupported(s) => oxideav_core::Error::Unsupported(s),
            FarbfeldError::LimitExceeded(s) => oxideav_core::Error::ResourceExhausted(s),
            FarbfeldError::Io(e) => oxideav_core::Error::Io(e),
        }
    }
}

// ---- pixel formats --------------------------------------------------------

/// The 1:1 name mapping from [`FarbfeldPixelFormat`] to the framework
/// enum.
pub fn to_core_pixel_format(pf: FarbfeldPixelFormat) -> PixelFormat {
    match pf {
        FarbfeldPixelFormat::Rgba64Le => PixelFormat::Rgba64Le,
    }
}

/// Map a framework pixel format to [`FarbfeldPixelFormat`];
/// [`FarbfeldError::Unsupported`] for every layout but `Rgba64Le`.
pub fn from_core_pixel_format(pf: PixelFormat) -> Result<FarbfeldPixelFormat> {
    match pf {
        PixelFormat::Rgba64Le => Ok(FarbfeldPixelFormat::Rgba64Le),
        other => Err(FarbfeldError::unsupported(format!(
            "farbfeld: pixel format {other:?} not supported (Rgba64Le only)"
        ))),
    }
}

impl From<FarbfeldPixelFormat> for PixelFormat {
    fn from(pf: FarbfeldPixelFormat) -> Self {
        to_core_pixel_format(pf)
    }
}

impl TryFrom<PixelFormat> for FarbfeldPixelFormat {
    type Error = FarbfeldError;
    fn try_from(pf: PixelFormat) -> Result<Self> {
        from_core_pixel_format(pf)
    }
}

// ---- colour signalling ----------------------------------------------------

/// [`ColorInfo`] as the framework's [`ColorSignal`] (code points map
/// 1:1).
pub fn to_color_signal(c: &ColorInfo) -> ColorSignal {
    let range = match c.range {
        ColorRange::Unspecified => oxideav_core::ColorRange::Unspecified,
        ColorRange::Limited => oxideav_core::ColorRange::Limited,
        ColorRange::Full => oxideav_core::ColorRange::Full,
    };
    ColorSignal::new(
        range,
        ColorPrimaries(c.primaries),
        TransferCharacteristics(c.transfer),
        MatrixCoefficients(c.matrix),
    )
}

/// The inverse of [`to_color_signal`].
pub fn from_color_signal(s: &ColorSignal) -> ColorInfo {
    let range = match s.range {
        oxideav_core::ColorRange::Limited => ColorRange::Limited,
        oxideav_core::ColorRange::Full => ColorRange::Full,
        _ => ColorRange::Unspecified,
    };
    ColorInfo::new(range, s.primaries.0, s.transfer.0, s.matrix.0)
}

// ---- frame bridge ---------------------------------------------------------

/// [`FarbfeldImage`] → `VideoFrame`, moving the plane out of the image:
/// one packed `Rgba64Le` plane, plus the colour-signal side-channel
/// **only** when the image's `color` is a caller-set description that
/// differs from the crate's sRGB convention. The file carries no colour
/// tag and the format defines no colour semantics, so a decoded image
/// (always [`ColorInfo::farbfeld_default`]) yields a frame with no
/// colour signal; the convention stays on the standalone `ColorInfo`
/// (`IMAGE_CRATE_API` stamping ruling).
pub(crate) fn image_into_video_frame(mut image: FarbfeldImage, pts: Option<i64>) -> VideoFrame {
    let stride = image.stride();
    let data = if image.planes.is_empty() {
        Vec::new()
    } else {
        std::mem::take(&mut image.planes[0].data)
    };
    let mut frame = VideoFrame {
        pts,
        planes: vec![VideoPlane { stride, data }],
    };
    let c = image.color;
    let specified = c.primaries != ColorInfo::UNSPECIFIED
        || c.transfer != ColorInfo::UNSPECIFIED
        || c.range == ColorRange::Limited;
    if specified && c != ColorInfo::farbfeld_default() {
        frame.set_color_signal(to_color_signal(&c));
    }
    frame
}

impl From<FarbfeldImage> for VideoFrame {
    /// The pixel plane (`pts` `None`); a colour-signal side-channel only
    /// for a caller-set `color` other than the sRGB convention.
    fn from(image: FarbfeldImage) -> Self {
        image_into_video_frame(image, None)
    }
}

impl From<&FarbfeldImage> for VideoFrame {
    fn from(image: &FarbfeldImage) -> Self {
        image_into_video_frame(image.clone(), None)
    }
}

impl FarbfeldImage {
    /// Rebuild an image from a framework frame and the stream
    /// parameters that describe it: `width`, `height` and
    /// `pixel_format` (`Rgba64Le`) are required; the frame's first
    /// image plane becomes the pixel plane (geometry validated by
    /// [`FarbfeldImage::new`], stride padding kept); the frame's
    /// colour-signal side-channel, refined over `params.color_signal`,
    /// becomes `color` when it specifies anything (else the sRGB
    /// convention stands).
    ///
    /// Errors are the crate's own: [`FarbfeldError::InvalidData`] for
    /// missing parameters or a plane too short for the geometry,
    /// [`FarbfeldError::Unsupported`] for any other pixel format.
    pub fn from_video_frame(frame: &VideoFrame, params: &CodecParameters) -> Result<Self> {
        let width = params
            .width
            .ok_or_else(|| FarbfeldError::invalid("farbfeld: width missing in CodecParameters"))?;
        let height = params
            .height
            .ok_or_else(|| FarbfeldError::invalid("farbfeld: height missing in CodecParameters"))?;
        let pix = from_core_pixel_format(params.pixel_format.ok_or_else(|| {
            FarbfeldError::invalid("farbfeld: pixel_format missing in CodecParameters")
        })?)?;
        let plane = frame
            .image_planes()
            .first()
            .ok_or_else(|| FarbfeldError::invalid("farbfeld: frame has no planes"))?;
        let mut img = FarbfeldImage::packed(width, height, pix, plane.stride, plane.data.clone())?;
        let sig = frame
            .color_signal()
            .unwrap_or_default()
            .or(params.color_signal);
        if !sig.is_unspecified() {
            img.color = from_color_signal(&sig);
        }
        Ok(img)
    }
}

impl TryFrom<(&VideoFrame, &CodecParameters)> for FarbfeldImage {
    type Error = FarbfeldError;
    fn try_from((frame, params): (&VideoFrame, &CodecParameters)) -> Result<Self> {
        FarbfeldImage::from_video_frame(frame, params)
    }
}

// ---- registration ---------------------------------------------------------

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

/// Register codec and container into two separately held registries
/// (the pre-contract shape of `register`; [`register`] now takes the
/// [`RuntimeContext`]).
pub fn register_registries(codecs: &mut CodecRegistry, containers: &mut ContainerRegistry) {
    register_codecs(codecs);
    register_containers(containers);
}

/// Unified entry point: install every codec and container provided by
/// `oxideav-farbfeld` into a [`RuntimeContext`].
///
/// Also wired into `oxideav_meta::register_all` via the
/// [`oxideav_core::register!`] macro below.
pub fn register(ctx: &mut RuntimeContext) {
    register_registries(&mut ctx.codecs, &mut ctx.containers);
}

/// Pre-contract name of [`register`].
#[deprecated(note = "use oxideav_farbfeld::register (IMAGE_CRATE_API)")]
pub fn register_runtime(ctx: &mut RuntimeContext) {
    register(ctx);
}

oxideav_core::register!("farbfeld", register);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Plane;

    #[test]
    fn oxideav_entry_installs_codec_and_container() {
        let mut ctx = RuntimeContext::new();
        __oxideav_entry(&mut ctx);
        assert!(
            ctx.codecs.decoder_ids().next().is_some(),
            "__oxideav_entry should install codec decoder factories"
        );
        assert_eq!(
            ctx.containers.container_for_extension("farbfeld"),
            Some("farbfeld"),
            "__oxideav_entry should install the .farbfeld extension hint"
        );
        assert_eq!(
            ctx.containers.container_for_extension("ff"),
            Some("farbfeld")
        );
    }

    #[test]
    #[allow(deprecated)]
    fn register_variants_agree() {
        let mut a = RuntimeContext::new();
        register(&mut a);
        let mut b = RuntimeContext::new();
        register_runtime(&mut b);
        let mut codecs = CodecRegistry::new();
        let mut containers = ContainerRegistry::new();
        register_registries(&mut codecs, &mut containers);
        let ids = |r: &CodecRegistry| r.decoder_ids().cloned().collect::<Vec<_>>();
        assert_eq!(ids(&a.codecs), ids(&b.codecs));
        assert_eq!(ids(&a.codecs), ids(&codecs));
        assert_eq!(containers.container_for_extension("ff"), Some("farbfeld"));
    }

    #[test]
    fn pixel_format_and_colour_maps_round_trip() {
        assert_eq!(
            to_core_pixel_format(FarbfeldPixelFormat::Rgba64Le),
            PixelFormat::Rgba64Le
        );
        assert_eq!(
            from_core_pixel_format(PixelFormat::Rgba64Le).unwrap(),
            FarbfeldPixelFormat::Rgba64Le
        );
        assert!(matches!(
            FarbfeldPixelFormat::try_from(PixelFormat::Rgba),
            Err(FarbfeldError::Unsupported(_))
        ));
        for c in [
            ColorInfo::srgb(),
            ColorInfo::unspecified(),
            ColorInfo::new(ColorRange::Limited, 9, 16, 9),
        ] {
            assert_eq!(from_color_signal(&to_color_signal(&c)), c);
        }
    }

    #[test]
    fn frame_bridge_round_trips_with_padding_and_colour() {
        let samples: Vec<u16> = (0..2 * 3 * 4).map(|i| i as u16 * 0x0101).collect();
        let tight = FarbfeldImage::from_rgba16(2, 3, &samples).unwrap();
        let mut padded = Vec::new();
        for row in tight.as_bytes().unwrap().chunks_exact(16) {
            padded.extend_from_slice(row);
            padded.extend_from_slice(&[0; 8]);
        }
        let img = FarbfeldImage::new(
            2,
            3,
            FarbfeldPixelFormat::Rgba64Le,
            vec![Plane::new(24, padded)],
        )
        .unwrap()
        .with_color(ColorInfo::new(ColorRange::Full, 9, 16, 0));
        let frame = VideoFrame::from(&img);
        assert_eq!(frame.planes[0].stride, 24);
        assert_eq!(frame.color_signal().unwrap().primaries.0, 9);

        let mut params = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
        params.width = Some(2);
        params.height = Some(3);
        params.pixel_format = Some(PixelFormat::Rgba64Le);
        let back = FarbfeldImage::from_video_frame(&frame, &params).unwrap();
        assert_eq!(back, img);
        let back2 = FarbfeldImage::try_from((&frame, &params)).unwrap();
        assert_eq!(back2, img);

        // A decoded image carries the sRGB convention, which is NOT
        // stamped: the file has no colour tag and the format defines no
        // colour semantics. The convention is restored on the way back.
        let frame = VideoFrame::from(tight.clone());
        assert!(frame.color_signal().is_none());
        let back = FarbfeldImage::from_video_frame(&frame, &params).unwrap();
        assert_eq!(back, tight);
        assert_eq!(back.color, ColorInfo::farbfeld_default());

        // Unspecified colour is not stamped and keeps the convention.
        let frame = VideoFrame::from(tight.clone().with_color(ColorInfo::unspecified()));
        assert!(frame.color_signal().is_none());
        assert_eq!(
            FarbfeldImage::from_video_frame(&frame, &params)
                .unwrap()
                .color,
            ColorInfo::farbfeld_default()
        );

        // Missing / wrong parameters are the crate's errors.
        let mut p = params.clone();
        p.pixel_format = Some(PixelFormat::Rgba);
        assert!(matches!(
            FarbfeldImage::from_video_frame(&frame, &p),
            Err(FarbfeldError::Unsupported(_))
        ));
        p.pixel_format = None;
        assert!(matches!(
            FarbfeldImage::from_video_frame(&frame, &p),
            Err(FarbfeldError::InvalidData(_))
        ));
        let mut p = params.clone();
        p.height = Some(4);
        assert!(matches!(
            FarbfeldImage::from_video_frame(&frame, &p),
            Err(FarbfeldError::InvalidData(_))
        ));
        let empty = VideoFrame {
            pts: None,
            planes: vec![],
        };
        assert!(matches!(
            FarbfeldImage::from_video_frame(&empty, &params),
            Err(FarbfeldError::InvalidData(_))
        ));
    }

    #[test]
    fn error_conversion_keeps_variants() {
        let e: oxideav_core::Error = FarbfeldError::limit("x").into();
        assert!(matches!(e, oxideav_core::Error::ResourceExhausted(_)));
        let e: oxideav_core::Error = FarbfeldError::unsupported("x").into();
        assert!(matches!(e, oxideav_core::Error::Unsupported(_)));
        let e: oxideav_core::Error = FarbfeldError::invalid("x").into();
        assert!(matches!(e, oxideav_core::Error::InvalidData(_)));
        let e: oxideav_core::Error = FarbfeldError::Io(std::io::Error::other("x")).into();
        assert!(matches!(e, oxideav_core::Error::Io(_)));
    }
}
