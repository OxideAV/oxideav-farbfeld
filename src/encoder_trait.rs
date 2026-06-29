//! `oxideav-core` `Encoder` trait implementation for farbfeld.
//!
//! Gated behind the `registry` feature. Accepts one
//! [`oxideav_core::PixelFormat::Rgba64Le`] video frame per `send_frame`
//! call and emits one complete farbfeld file as a packet.

use crate::encoder::swap_pairs_le_to_be;
use crate::parser::{BYTES_PER_PIXEL, HEADER_LEN, MAGIC};

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
        // exceed `width * 8` (e.g. arena-aligned plane); only the first
        // `width * 8` bytes of each row carry samples, the rest is pad
        // that must never reach disk.
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
        // Reject a plane that can't supply `height` full rows at the
        // declared stride before indexing into it below.
        let needed = if height == 0 {
            0
        } else {
            (height as usize - 1)
                .checked_mul(plane.stride)
                .and_then(|n| n.checked_add(row_bytes))
                .ok_or_else(|| {
                    oxideav_core::Error::invalid("farbfeld encoder: plane extent overflow")
                })?
        };
        if plane.data.len() < needed {
            return Err(oxideav_core::Error::invalid(format!(
                "farbfeld encoder: plane data {} bytes too short for {height} rows of stride {}",
                plane.data.len(),
                plane.stride
            )));
        }

        // Build the complete farbfeld file in one allocation: write the
        // 16-byte header, then swap each LE source row directly into its
        // contiguous slot in the body — skipping the stride pad and the
        // intermediate `body_be` Vec + the re-copy `encode_farbfeld`
        // would have done. `swap_pairs_le_to_be` is the SIMD-friendly
        // LE->BE byte-order transform shared with the rest of the crate.
        let mut out = vec![0u8; HEADER_LEN + body_len];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&width.to_be_bytes());
        out[12..16].copy_from_slice(&height.to_be_bytes());
        for y in 0..height as usize {
            let src = &plane.data[y * plane.stride..y * plane.stride + row_bytes];
            let dst_lo = HEADER_LEN + y * row_bytes;
            let dst = &mut out[dst_lo..dst_lo + row_bytes];
            swap_pairs_le_to_be(src, dst);
        }
        self.pending = Some(out);
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
