//! `oxideav-core` `Encoder` trait implementation for farbfeld.
//!
//! Gated behind the `registry` feature. Accepts one
//! [`oxideav_core::PixelFormat::Rgba64Le`] video frame per `send_frame`
//! call and emits one complete farbfeld file as a packet.

use crate::encoder::encode_farbfeld;
use crate::parser::BYTES_PER_PIXEL;

use oxideav_core::Encoder;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase};

pub fn make_encoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Encoder>> {
    let mut out_params = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
    out_params.width = params.width;
    out_params.height = params.height;
    out_params.pixel_format = Some(PixelFormat::Rgba64Le);
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
        let width = self.out_params.width.ok_or_else(|| {
            oxideav_core::Error::invalid("farbfeld encoder: width missing in CodecParameters")
        })?;
        let height = self.out_params.height.ok_or_else(|| {
            oxideav_core::Error::invalid("farbfeld encoder: height missing in CodecParameters")
        })?;
        if vf.planes.is_empty() {
            return Err(oxideav_core::Error::invalid(
                "farbfeld encoder: empty planes",
            ));
        }
        let plane = &vf.planes[0];

        // Caller hands us little-endian 16-bit RGBA — convert each
        // sample to big-endian for the on-disk body. Plane stride may
        // exceed `width * 8` (e.g. arena-aligned plane); copy
        // `width * 8` bytes per row to skip the trailing pad.
        let row_bytes = (width as usize)
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or_else(|| oxideav_core::Error::invalid("farbfeld encoder: row size overflow"))?;
        if plane.stride < row_bytes {
            return Err(oxideav_core::Error::invalid(format!(
                "farbfeld encoder: plane stride {} smaller than row width {row_bytes}",
                plane.stride
            )));
        }
        let body_len = row_bytes
            .checked_mul(height as usize)
            .ok_or_else(|| oxideav_core::Error::invalid("farbfeld encoder: body size overflow"))?;
        let mut body_be = Vec::with_capacity(body_len);
        for y in 0..height as usize {
            let row = &plane.data[y * plane.stride..y * plane.stride + row_bytes];
            // Each pair of LE bytes becomes a pair of BE bytes.
            for pair in row.chunks_exact(2) {
                let v = u16::from_le_bytes([pair[0], pair[1]]);
                body_be.extend_from_slice(&v.to_be_bytes());
            }
        }

        let bytes = encode_farbfeld(width, height, &body_be)?;
        self.pending = Some(bytes);
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
