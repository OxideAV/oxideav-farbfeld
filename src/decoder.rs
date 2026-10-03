//! `oxideav-core` `Decoder` trait implementation for farbfeld.
//!
//! Gated behind the `registry` feature. A thin adapter over the
//! standalone [`crate::decode`]: the decoder accepts one complete
//! farbfeld file per packet and emits one [`VideoFrame`] per packet in
//! the native `Rgba64Le` layout (the big-endian wire samples
//! byte-swapped to little-endian). No colour-signal side-channel: the
//! file carries no colour tag, so the crate's sRGB convention stays on
//! the standalone [`crate::ColorInfo`] and is not stamped on the frame.

use crate::image::FarbfeldImage;
use crate::registry::image_into_video_frame;

use oxideav_core::Decoder;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, VideoFrame};

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
        // The standalone contract path, default limits (1 GiB plane).
        let image: FarbfeldImage = crate::api::decode(&packet.data)?;
        // farbfeld carries no timestamp of its own; thread the
        // surrounding `Packet`'s `pts` onto the produced frame.
        self.pending = Some(image_into_video_frame(image, packet.pts));
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

    /// Clear all carry-over state so the decoder can resume from a new
    /// stream position after a container seek.
    ///
    /// The trait's default `reset` is "flush-then-drain", but our
    /// `flush` latches `eof = true` — so the default would leave the
    /// decoder permanently end-of-stream, returning `Eof` on the next
    /// `receive_frame` instead of `NeedMore`. Override it to drop any
    /// buffered frame and clear the eof latch, restoring the
    /// freshly-constructed state (codec id untouched) so the next
    /// `send_packet` decodes as if it were the first.
    fn reset(&mut self) -> oxideav_core::Result<()> {
        self.pending = None;
        self.eof = false;
        Ok(())
    }
}
