#![no_main]

//! Framework-trait fuzz harness for `oxideav-farbfeld`.
//!
//! The `decode` / `encode` / `stream_io` targets drive the standalone
//! (no-`oxideav-core`) parser/encoder surface. This fourth target
//! exercises the `registry`-gated framework integration that those three
//! never touch:
//!
//! * the `oxideav_core::Decoder` impl (`decoder::make_decoder`), which
//!   parses one farbfeld file per packet and re-serialises the
//!   native-endian samples into the canonical little-endian
//!   `PixelFormat::Rgba64Le` plane;
//! * the `oxideav_core::Encoder` impl (`encoder_trait::make_encoder`),
//!   which swaps a little-endian RGBA64 plane (possibly with a padded
//!   stride) back into the on-disk big-endian body in a single pass;
//! * the container demuxer (`container::open_demuxer`), which validates
//!   the header eagerly and publishes `(width, height)`.
//!
//! Two independent surfaces are fuzzed from the same `data` slice:
//!
//! ## A. Decode path — attacker bytes
//!
//! `data` is fed verbatim as a packet to the framework decoder. It must
//! **never panic**. When it accepts, the input is a well-formed farbfeld
//! file, so:
//! * the emitted plane is exactly `height` rows of `width*8` bytes;
//! * the standalone `parse_farbfeld` agrees it is valid and reports the
//!   same dimensions;
//! * feeding the produced frame straight back into the framework encoder
//!   reproduces the original bytes exactly (farbfeld is lossless with one
//!   canonical serialisation). This pins the decoder LE conversion and
//!   the encoder one-pass LE->BE swap against each other.
//! The container demuxer is held to the same accept/reject verdict as the
//! whole-file parser.
//!
//! ## B. Encode path — attacker frames
//!
//! A prefix of `data` is consumed to synthesise an arbitrary
//! `(width, height, stride, plane_len)` frame whose plane bytes are the
//! remaining input (zero-padded). The framework encoder must **never
//! panic** on any of these — including planes too short for the declared
//! dimensions, strides smaller than `width*8`, and dimension products
//! that overflow. Whenever it *accepts*, the bytes it emits must parse
//! back cleanly and re-decode to a plane whose sample rows equal the
//! source rows (pad bytes excluded).

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

use oxideav_core::{
    CodecId, CodecParameters, Frame, NullCodecResolver, Packet, PixelFormat, ReadSeek, TimeBase,
    VideoFrame, VideoPlane,
};
use oxideav_farbfeld::{
    container, decoder::make_decoder, encoder_trait::make_encoder, parse_farbfeld, CODEC_ID_STR,
};

fuzz_target!(|data: &[u8]| {
    decode_surface(data);
    encode_surface(data);
});

/// Surface A: drive the framework decoder + demuxer over arbitrary bytes.
fn decode_surface(data: &[u8]) {
    let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    let mut dec = make_decoder(&params).expect("decoder factory is infallible");

    let mut pkt = Packet::new(0, TimeBase::new(1, 1), data.to_vec());
    pkt.flags.keyframe = true;

    // The decoder must never panic; send_packet may reject malformed
    // bytes, in which case there is nothing further to assert here.
    if dec.send_packet(&pkt).is_err() {
        // A rejected packet must mean the whole-file parser also rejects.
        // (Both share `parse_farbfeld`, so they cannot disagree, but the
        // demuxer's eager validation is fuzzed for the same verdict.)
        demuxer_rejects(data);
        return;
    }

    let frame = match dec.receive_frame() {
        Ok(Frame::Video(v)) => v,
        Ok(_) => panic!("farbfeld decoder must only emit video frames"),
        Err(e) => panic!("decoder accepted a packet but produced no frame: {e}"),
    };

    // A successful decode means the bytes are a valid farbfeld file.
    let img = parse_farbfeld(data)
        .expect("framework decoder accepted bytes the whole-file parser rejected");

    let stride = (img.width as usize) * 8;
    assert_eq!(frame.planes.len(), 1, "farbfeld decodes to a single plane");
    assert_eq!(frame.planes[0].stride, stride, "plane stride = width*8");
    assert_eq!(
        frame.planes[0].data.len(),
        stride * img.height as usize,
        "plane is exactly height rows of width*8 bytes",
    );

    // The demuxer must accept the same bytes and report the same dims.
    let cursor: Box<dyn ReadSeek> = Box::new(Cursor::new(data.to_vec()));
    let demux = container::open_demuxer(cursor, &NullCodecResolver)
        .expect("demuxer rejected bytes the decoder accepted");
    let streams = demux.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].params.width, Some(img.width));
    assert_eq!(streams[0].params.height, Some(img.height));

    // Re-encode the decoded frame through the framework encoder and
    // confirm a byte-exact round-trip (lossless, single canonical form).
    let mut enc_params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    enc_params.width = Some(img.width);
    enc_params.height = Some(img.height);
    enc_params.pixel_format = Some(PixelFormat::Rgba64Le);
    let mut enc = make_encoder(&enc_params).expect("encoder factory is infallible");
    enc.send_frame(&Frame::Video(frame))
        .expect("encoder rejected a frame the decoder just produced");
    let out = enc
        .receive_packet()
        .expect("encoder produced no packet for a valid frame");
    assert_eq!(
        out.data.as_slice(),
        data,
        "decode->encode framework round-trip was not byte-identical",
    );
}

/// Confirm the demuxer rejects bytes the whole-file parser rejects.
fn demuxer_rejects(data: &[u8]) {
    if parse_farbfeld(data).is_ok() {
        // Parser accepts but decoder rejected — impossible (shared path);
        // surface it loudly if it ever happens.
        panic!("whole-file parser accepted bytes the framework decoder rejected");
    }
    let cursor: Box<dyn ReadSeek> = Box::new(Cursor::new(data.to_vec()));
    if container::open_demuxer(cursor, &NullCodecResolver).is_ok() {
        panic!("demuxer accepted bytes the whole-file parser rejected");
    }
}

/// Surface B: drive the framework encoder over an arbitrary synthesised
/// frame whose dimensions / stride / plane length come from the input.
fn encode_surface(data: &[u8]) {
    // Need a few control bytes; if the input is too short, skip — surface
    // A already covered the empty/short cases.
    if data.len() < 5 {
        return;
    }
    // Keep the declared dimensions small so the encoder's `width*height*8`
    // body stays bounded (the no-panic contract holds for any value, but
    // we don't want the fuzzer wasting time on multi-GB allocations on
    // the rare input that passes every check). 0..=15 each.
    let width = (data[0] & 0x0F) as u32;
    let height = (data[1] & 0x0F) as u32;
    let row_bytes = (width as usize) * 8;
    // Stride is width*8 plus an attacker-chosen pad of 0..=7 extra bytes.
    let pad = (data[2] & 0x07) as usize;
    let stride = row_bytes + pad;

    // Plane bytes: the remaining input, optionally truncated by a control
    // byte so we frequently hit the "plane too short" branch.
    let body = &data[3..];
    let take = if body.is_empty() {
        0
    } else {
        // 0..=body.len(), so both "too short" and "long enough" occur.
        (data[3] as usize).min(body.len())
    };
    let mut plane = body[..take].to_vec();
    // Round the plane down to whole bytes; the encoder copies row slices,
    // so leave length arbitrary to exercise the bounds check.

    let frame = Frame::Video(VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride,
            data: std::mem::take(&mut plane),
        }],
    });

    let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    params.width = Some(width);
    params.height = Some(height);
    params.pixel_format = Some(PixelFormat::Rgba64Le);
    let mut enc = make_encoder(&params).expect("encoder factory is infallible");

    // Must never panic. Reject is fine (short plane / overflow / etc.).
    let Frame::Video(vf) = &frame else {
        unreachable!()
    };
    let src_plane = vf.planes[0].clone();
    if enc.send_frame(&frame).is_err() {
        return;
    }
    let pkt = enc
        .receive_packet()
        .expect("encoder accepted a frame but produced no packet");

    // Whatever it produced must be a valid farbfeld file of the declared
    // dimensions, and re-decoding it must reproduce the source rows
    // (excluding stride pad) byte-for-byte.
    let parsed = parse_farbfeld(&pkt.data).expect("encoder emitted an unparseable farbfeld file");
    assert_eq!(parsed.width, width);
    assert_eq!(parsed.height, height);

    // Compare each decoded row's BE body against the swapped source row.
    let body_be = &pkt.data[16..];
    for y in 0..height as usize {
        let src = &src_plane.data[y * stride..y * stride + row_bytes];
        let dst = &body_be[y * row_bytes..y * row_bytes + row_bytes];
        for (s, d) in src.chunks_exact(2).zip(dst.chunks_exact(2)) {
            // Encoder swaps LE [lo, hi] source into BE [hi, lo] on disk.
            assert_eq!(d[0], s[1], "BE high byte must equal LE low byte");
            assert_eq!(d[1], s[0], "BE low byte must equal LE high byte");
        }
    }
}
