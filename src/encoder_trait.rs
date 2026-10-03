//! `oxideav-core` `Encoder` trait implementation for farbfeld.
//!
//! Gated behind the `registry` feature. A thin adapter over the
//! standalone [`crate::encode`]: accepts one `Rgba64Le` video frame per
//! `send_frame` call (rebuilt into a [`FarbfeldImage`] through
//! [`FarbfeldImage::from_video_frame`], which validates the plane
//! against the declared dimensions and keeps stride padding out of the
//! file) and emits one complete farbfeld file as a keyframe packet.

use crate::image::FarbfeldImage;
use crate::options::EncodeOptions;

use oxideav_core::Encoder;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase};

/// Factory registered with the codec registry. `width` / `height` are
/// taken from `params`; the output pixel format is always `Rgba64Le`
/// (the pipeline converts to it).
pub fn make_encoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Encoder>> {
    let mut out_params = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
    out_params.width = params.width;
    out_params.height = params.height;
    out_params.pixel_format = Some(PixelFormat::Rgba64Le);
    out_params.color_signal = params.color_signal;
    Ok(Box::new(FarbfeldEncoder {
        codec_id: CodecId::new(crate::CODEC_ID_STR),
        out_params,
        pending: None,
        eof: false,
    }))
}

struct FarbfeldEncoder {
    codec_id: CodecId,
    out_params: CodecParameters,
    pending: Option<Vec<u8>>,
    eof: bool,
}

impl Encoder for FarbfeldEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.out_params
    }

    fn send_frame(&mut self, frame: &Frame) -> oxideav_core::Result<()> {
        let vf = match frame {
            Frame::Video(v) => v,
            _ => {
                return Err(oxideav_core::Error::invalid(
                    "farbfeld encoder: expected video frame",
                ))
            }
        };
        let image = FarbfeldImage::from_video_frame(vf, &self.out_params)?;
        self.pending = Some(crate::api::encode(&image, &EncodeOptions::default())?);
        Ok(())
    }

    fn receive_packet(&mut self) -> oxideav_core::Result<Packet> {
        match self.pending.take() {
            Some(bytes) => {
                let mut pkt = Packet::new(0, TimeBase::new(1, 1), bytes);
                pkt.flags.keyframe = true;
                Ok(pkt)
            }
            None => {
                if self.eof {
                    Err(oxideav_core::Error::Eof)
                } else {
                    Err(oxideav_core::Error::NeedMore)
                }
            }
        }
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
}
