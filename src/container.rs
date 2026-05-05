//! farbfeld container: one single-image file becomes one [`Packet`] on
//! stream `0`. Mirrors the same shape as `oxideav-pbm` / `oxideav-bmp`
//! (single-frame image containers) — farbfeld has no animation or
//! multi-frame layout to worry about.
//!
//! Lives behind the `registry` feature: the container types are all
//! defined by `oxideav-core`, so a standalone build (no framework dep)
//! has nothing meaningful to expose here.

use std::io::{Read, SeekFrom, Write};

use oxideav_core::{
    CodecId, CodecParameters, CodecResolver, Error, MediaType, Packet, PixelFormat, Result,
    StreamInfo, TimeBase,
};
use oxideav_core::{
    ContainerRegistry, Demuxer, Muxer, ProbeData, ProbeScore, ReadSeek, WriteSeek, MAX_PROBE_SCORE,
};

use crate::parser::{parse_farbfeld, MAGIC};

pub fn register(reg: &mut ContainerRegistry) {
    reg.register_demuxer("farbfeld", open_demuxer);
    reg.register_muxer("farbfeld", open_muxer);
    reg.register_extension("ff", "farbfeld");
    reg.register_extension("farbfeld", "farbfeld");
    reg.register_probe("farbfeld", probe);
}

fn probe(data: &ProbeData) -> ProbeScore {
    if data.buf.len() >= 8 && &data.buf[..8] == MAGIC {
        return MAX_PROBE_SCORE;
    }
    if matches!(data.ext, Some("ff") | Some("farbfeld")) {
        oxideav_core::PROBE_SCORE_EXTENSION
    } else {
        0
    }
}

pub fn open_demuxer(
    mut input: Box<dyn ReadSeek>,
    _codecs: &dyn CodecResolver,
) -> Result<Box<dyn Demuxer>> {
    input.seek(SeekFrom::Start(0))?;
    let mut buf = Vec::new();
    input.read_to_end(&mut buf)?;
    // Validate the header eagerly so the demuxer can publish accurate
    // (width, height) on the StreamInfo before the decoder even runs.
    let parsed = parse_farbfeld(&buf)?;
    let mut params = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
    params.width = Some(parsed.width);
    params.height = Some(parsed.height);
    params.pixel_format = Some(PixelFormat::Rgba64Le);
    let stream = StreamInfo {
        index: 0,
        params,
        time_base: TimeBase::new(1, 1),
        start_time: Some(0),
        duration: None,
    };
    Ok(Box::new(FarbfeldDemuxer {
        streams: vec![stream],
        data: Some(buf),
    }))
}

struct FarbfeldDemuxer {
    streams: Vec<StreamInfo>,
    data: Option<Vec<u8>>,
}

impl Demuxer for FarbfeldDemuxer {
    fn format_name(&self) -> &str {
        "farbfeld"
    }
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }
    fn next_packet(&mut self) -> Result<Packet> {
        match self.data.take() {
            Some(bytes) => {
                let mut pkt = Packet::new(0, TimeBase::new(1, 1), bytes);
                pkt.pts = Some(0);
                pkt.dts = Some(0);
                pkt.flags.keyframe = true;
                Ok(pkt)
            }
            None => Err(Error::Eof),
        }
    }
}

pub fn open_muxer(output: Box<dyn WriteSeek>, streams: &[StreamInfo]) -> Result<Box<dyn Muxer>> {
    if streams.len() != 1 {
        return Err(Error::invalid(
            "farbfeld muxer: expected exactly one video stream",
        ));
    }
    if streams[0].params.media_type != MediaType::Video {
        return Err(Error::invalid("farbfeld muxer: stream must be video"));
    }
    Ok(Box::new(FarbfeldMuxer { output }))
}

struct FarbfeldMuxer {
    output: Box<dyn WriteSeek>,
}

impl Muxer for FarbfeldMuxer {
    fn format_name(&self) -> &str {
        "farbfeld"
    }
    fn write_header(&mut self) -> Result<()> {
        Ok(())
    }
    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        // The encoder produces a complete farbfeld file in a single
        // packet — write it through unchanged.
        self.output.write_all(&packet.data)?;
        Ok(())
    }
    fn write_trailer(&mut self) -> Result<()> {
        Ok(())
    }
}
