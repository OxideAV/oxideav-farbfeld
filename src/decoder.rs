//! `oxideav-core` `Decoder` trait implementation for farbfeld.
//!
//! Gated behind the `registry` feature. The decoder accepts one
//! complete farbfeld file per packet and emits one [`oxideav_core::VideoFrame`]
//! per packet. Pixels are converted from the on-disk big-endian layout
//! to the framework canonical [`oxideav_core::PixelFormat::Rgba64Le`]
//! (little-endian) so the resulting `VideoPlane.data` is ready to feed
//! straight into image-conversion or display code without further byte
//! shuffling.

use crate::encoder::encode_le_samples;
use crate::parser::parse_farbfeld;

use oxideav_core::Decoder;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, VideoFrame, VideoPlane};

/// Factory registered with the codec registry. One packet per whole
/// farbfeld file; one frame per packet.
pub fn make_decoder(_params: &CodecParameters) -> oxideav_core::Result<Box<dyn Decoder>> {
    Ok(Box::new(FarbfeldDecoder {
        codec_id: CodecId::new(crate::CODEC_ID_STR),
        pending: None,
        eof: false,
    }))
}

struct FarbfeldDecoder {
    codec_id: CodecId,
    pending: Option<VideoFrame>,
    eof: bool,
}

impl Decoder for FarbfeldDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> oxideav_core::Result<()> {
        let image = parse_farbfeld(&packet.data)?;
        // Convert native-endian u16 samples to the canonical little-endian
        // byte layout expected by `PixelFormat::Rgba64Le`. Route through
        // the crate's shared SIMD-friendly `encode_le_samples` helper —
        // the same `iter().zip(chunks_exact_mut(2))` shape the
        // auto-vectoriser lifts into a single store — rather than a
        // per-sample `extend_from_slice` append, so the framework decode
        // path matches the standalone parser/encoder hot loops.
        let stride = (image.width as usize)
            .checked_mul(8)
            .ok_or_else(|| oxideav_core::Error::invalid("farbfeld: stride overflow"))?;
        let body_len = stride
            .checked_mul(image.height as usize)
            .ok_or_else(|| oxideav_core::Error::invalid("farbfeld: plane size overflow"))?;
        // `body_len == image.pixels.len() * 2` whenever the parser
        // succeeded (pixels = width*height*4 samples, body = ×2 bytes);
        // size the buffer to the on-disk body and fill it in one pass.
        let mut data = vec![0u8; body_len];
        encode_le_samples(&image.pixels, &mut data);
        self.pending = Some(VideoFrame {
            pts: packet.pts,
            planes: vec![VideoPlane { stride, data }],
        });
        Ok(())
    }

    fn receive_frame(&mut self) -> oxideav_core::Result<Frame> {
        match self.pending.take() {
            Some(f) => Ok(Frame::Video(f)),
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
