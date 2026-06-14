//! Criterion micro-benchmarks for the farbfeld codec.
//!
//! Six bench groups exercise the public API across three image sizes —
//! `64×64` (16 KiB body), `256×256` (256 KiB body), `1024×1024` (4 MiB
//! body) — so the suite captures both the per-call constant cost
//! (header validation, allocation overhead) on the small fixture and the
//! per-byte throughput on the large one.
//!
//! Bench groups:
//!
//! 1. `parse_whole`             — `parse_farbfeld` on a pre-encoded buffer.
//! 2. `encode_raw_be`           — `encode_farbfeld` (pre-serialised BE body).
//! 3. `encode_from_rgba16`      — `encode_farbfeld_from_rgba16`
//!    (native-endian RGBA, per-channel BE swap on emit).
//! 4. `encode_image`            — `encode_farbfeld_image` (flat `[u16]`
//!    plane via `FarbfeldImage`).
//! 5. `stream_read_all_rows`    — `FarbfeldStreamReader::read_all_rows`
//!    against an `std::io::Cursor`-backed body.
//! 6. `stream_write_all_rows`   — `FarbfeldStreamWriter::write_row`
//!    looped to completion, finishing into a `Vec<u8>`.
//! 7. `stream_read_row_raw`     — `FarbfeldStreamReader::read_row_raw`
//!    looped row-by-row over the whole body. Symmetric counterpart to
//!    `stream_read_all_rows` that skips the per-sample BE → native
//!    decode, so the bench measures the pure row-bytes pump path the
//!    raw API exposes to proxies / hash-and-discard pipelines / re-mux
//!    forwarders.
//! 8. `stream_write_row_raw`    — `FarbfeldStreamWriter::write_row_raw`
//!    looped to completion against a pre-serialised BE body. Symmetric
//!    counterpart to `stream_write_all_rows` that forwards already-BE
//!    bytes verbatim without the per-sample BE swap on emit. Together
//!    with `stream_read_row_raw`, the two guard the round-11 perf claim
//!    that the raw pass-through path is faster than its native-endian
//!    sibling.
//! 9. `stream_skip_row`         — `FarbfeldStreamReader::skip_row`
//!    looped row-by-row over the whole body. This is the row-window
//!    decode path: the caller only wants rows N..M of a multi-gigapixel
//!    stream (thumbnail row, scan-line inspection, partial decode) and
//!    advances past the rest. `skip_row` runs the same bounded
//!    `Read::take` body-consume discipline as `read_row` / `read_row_raw`
//!    but performs neither the per-sample BE → native decode nor the
//!    verbatim byte copy into a caller slot, so this group is the
//!    floor for how fast the reader can walk a body it doesn't keep.
//!
//! Run all of them with:
//!
//! ```text
//! cargo bench -p oxideav-farbfeld
//! ```
//!
//! Or a single group with e.g. `cargo bench -p oxideav-farbfeld -- parse_whole`.
//!
//! Criterion picks the iteration count itself; each group uses
//! `Throughput::Bytes(body_len)` so the report includes a MiB/s figure
//! that's directly comparable across sizes.

use std::io::Cursor;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_farbfeld::{
    encode_farbfeld, encode_farbfeld_from_rgba16, encode_farbfeld_image, parse_farbfeld,
    FarbfeldImage, FarbfeldStreamReader, FarbfeldStreamWriter,
};

/// Image sizes covered by every group. The 4 MiB top end is large
/// enough to dominate over per-call header overhead while staying inside
/// the runner's budget; the 16 KiB low end keeps the constant-cost path
/// visible on the same chart.
const SIZES: &[(u32, u32)] = &[(64, 64), (256, 256), (1024, 1024)];

/// Build a deterministic RGBA u16 pixel plane of the requested size.
///
/// Pattern: `R = x ^ y`, `G = x.wrapping_mul(257)`,
/// `B = y.wrapping_mul(257)`, `A = 0xFFFF`. Spread enough to avoid all
/// channels being constant (which would make the BE swap suspiciously
/// well-cached) without depending on a PRNG.
fn pattern_pixels(width: u32, height: u32) -> Vec<[u16; 4]> {
    let mut out = Vec::with_capacity((width as usize) * (height as usize));
    for y in 0..height {
        for x in 0..width {
            out.push([
                (x as u16) ^ (y as u16),
                (x as u16).wrapping_mul(257),
                (y as u16).wrapping_mul(257),
                0xFFFF,
            ]);
        }
    }
    out
}

/// Flatten the `[[u16; 4]]` pixel plane into a flat row-major `Vec<u16>`
/// suitable for `FarbfeldImage`.
fn pattern_samples_flat(pixels: &[[u16; 4]]) -> Vec<u16> {
    let mut out = Vec::with_capacity(pixels.len() * 4);
    for px in pixels {
        out.extend_from_slice(px);
    }
    out
}

/// Pre-serialise the pixel plane into the on-disk big-endian RGBA u16
/// body (i.e. exactly the body half that `encode_farbfeld` accepts).
fn pattern_body_be(pixels: &[[u16; 4]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() * 8);
    for px in pixels {
        for chan in px {
            out.extend_from_slice(&chan.to_be_bytes());
        }
    }
    out
}

/// Encode the pixel plane into a full farbfeld byte stream once, for
/// use as the parse-side fixture.
fn pattern_full_stream(width: u32, height: u32, pixels: &[[u16; 4]]) -> Vec<u8> {
    encode_farbfeld_from_rgba16(width, height, pixels).expect("encode pattern fixture")
}

fn bench_parse_whole(c: &mut Criterion) {
    let mut g = c.benchmark_group("parse_whole");
    for &(w, h) in SIZES {
        let pixels = pattern_pixels(w, h);
        let stream = pattern_full_stream(w, h, &pixels);
        g.throughput(Throughput::Bytes(stream.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{w}x{h}")),
            &stream,
            |b, s| {
                b.iter(|| {
                    let img = parse_farbfeld(black_box(s)).expect("parse benches well-formed");
                    black_box(img);
                });
            },
        );
    }
    g.finish();
}

fn bench_encode_raw_be(c: &mut Criterion) {
    let mut g = c.benchmark_group("encode_raw_be");
    for &(w, h) in SIZES {
        let pixels = pattern_pixels(w, h);
        let body_be = pattern_body_be(&pixels);
        g.throughput(Throughput::Bytes(body_be.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{w}x{h}")),
            &(w, h, body_be),
            |b, (ww, hh, body)| {
                b.iter(|| {
                    let out = encode_farbfeld(black_box(*ww), black_box(*hh), black_box(body))
                        .expect("encode benches well-formed");
                    black_box(out);
                });
            },
        );
    }
    g.finish();
}

fn bench_encode_from_rgba16(c: &mut Criterion) {
    let mut g = c.benchmark_group("encode_from_rgba16");
    for &(w, h) in SIZES {
        let pixels = pattern_pixels(w, h);
        g.throughput(Throughput::Bytes((pixels.len() * 8) as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{w}x{h}")),
            &(w, h, pixels),
            |b, (ww, hh, px)| {
                b.iter(|| {
                    let out =
                        encode_farbfeld_from_rgba16(black_box(*ww), black_box(*hh), black_box(px))
                            .expect("encode_from_rgba16 benches well-formed");
                    black_box(out);
                });
            },
        );
    }
    g.finish();
}

fn bench_encode_image(c: &mut Criterion) {
    let mut g = c.benchmark_group("encode_image");
    for &(w, h) in SIZES {
        let pixels = pattern_pixels(w, h);
        let samples = pattern_samples_flat(&pixels);
        let img = FarbfeldImage {
            width: w,
            height: h,
            pixels: samples,
        };
        g.throughput(Throughput::Bytes((img.pixels.len() * 2) as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{w}x{h}")),
            &img,
            |b, im| {
                b.iter(|| {
                    let out = encode_farbfeld_image(black_box(im))
                        .expect("encode_image benches well-formed");
                    black_box(out);
                });
            },
        );
    }
    g.finish();
}

fn bench_stream_read_all_rows(c: &mut Criterion) {
    let mut g = c.benchmark_group("stream_read_all_rows");
    for &(w, h) in SIZES {
        let pixels = pattern_pixels(w, h);
        let stream = pattern_full_stream(w, h, &pixels);
        g.throughput(Throughput::Bytes(stream.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{w}x{h}")),
            &stream,
            |b, s| {
                b.iter(|| {
                    let cursor = Cursor::new(black_box(s.as_slice()));
                    let mut reader =
                        FarbfeldStreamReader::new(cursor).expect("stream reader header parses");
                    let out = reader
                        .read_all_rows()
                        .expect("stream reader body well-formed");
                    black_box(out);
                });
            },
        );
    }
    g.finish();
}

fn bench_stream_write_all_rows(c: &mut Criterion) {
    let mut g = c.benchmark_group("stream_write_all_rows");
    for &(w, h) in SIZES {
        let pixels = pattern_pixels(w, h);
        let samples = pattern_samples_flat(&pixels);
        let body_bytes = (samples.len() * 2) as u64;
        g.throughput(Throughput::Bytes(body_bytes));
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{w}x{h}")),
            &(w, h, samples),
            |b, (ww, hh, flat)| {
                let row_samples = (*ww as usize) * 4;
                b.iter(|| {
                    let mut writer = FarbfeldStreamWriter::new(Vec::new(), *ww, *hh)
                        .expect("stream writer header emits");
                    for row in flat.chunks_exact(row_samples) {
                        writer
                            .write_row(black_box(row))
                            .expect("write_row well-formed");
                    }
                    let out = writer.finish().expect("finish well-formed");
                    black_box(out);
                });
            },
        );
    }
    g.finish();
}

fn bench_stream_read_row_raw(c: &mut Criterion) {
    let mut g = c.benchmark_group("stream_read_row_raw");
    for &(w, h) in SIZES {
        let pixels = pattern_pixels(w, h);
        let stream = pattern_full_stream(w, h, &pixels);
        g.throughput(Throughput::Bytes(stream.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{w}x{h}")),
            &stream,
            |b, s| {
                let row_bytes = (w as usize) * 8;
                b.iter(|| {
                    let cursor = Cursor::new(black_box(s.as_slice()));
                    let mut reader =
                        FarbfeldStreamReader::new(cursor).expect("raw reader header parses");
                    // Re-use one row buffer across all `height` rows — that's
                    // the documented use shape for `read_row_raw` (the caller
                    // owns the buffer; the reader writes into it in place).
                    let mut row = vec![0u8; row_bytes];
                    while reader
                        .read_row_raw(black_box(&mut row))
                        .expect("raw reader body well-formed")
                    {
                        black_box(&row);
                    }
                });
            },
        );
    }
    g.finish();
}

fn bench_stream_write_row_raw(c: &mut Criterion) {
    let mut g = c.benchmark_group("stream_write_row_raw");
    for &(w, h) in SIZES {
        let pixels = pattern_pixels(w, h);
        let body_be = pattern_body_be(&pixels);
        g.throughput(Throughput::Bytes(body_be.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{w}x{h}")),
            &(w, h, body_be),
            |b, (ww, hh, body)| {
                let row_bytes = (*ww as usize) * 8;
                b.iter(|| {
                    let mut writer = FarbfeldStreamWriter::new(Vec::new(), *ww, *hh)
                        .expect("raw writer header emits");
                    for row in body.chunks_exact(row_bytes) {
                        writer
                            .write_row_raw(black_box(row))
                            .expect("write_row_raw well-formed");
                    }
                    let out = writer.finish().expect("finish well-formed");
                    black_box(out);
                });
            },
        );
    }
    g.finish();
}

fn bench_stream_skip_row(c: &mut Criterion) {
    let mut g = c.benchmark_group("stream_skip_row");
    for &(w, h) in SIZES {
        let pixels = pattern_pixels(w, h);
        let stream = pattern_full_stream(w, h, &pixels);
        g.throughput(Throughput::Bytes(stream.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{w}x{h}")),
            &stream,
            |b, s| {
                b.iter(|| {
                    let cursor = Cursor::new(black_box(s.as_slice()));
                    let mut reader =
                        FarbfeldStreamReader::new(cursor).expect("skip reader header parses");
                    // Walk the whole body via the row-window path: consume
                    // each row's bytes without decoding to samples or
                    // copying them out. This is the partial-decode floor.
                    while reader.skip_row().expect("skip reader body well-formed") {}
                    black_box(reader.rows_read());
                });
            },
        );
    }
    g.finish();
}

criterion_group!(
    benches,
    bench_parse_whole,
    bench_encode_raw_be,
    bench_encode_from_rgba16,
    bench_encode_image,
    bench_stream_read_all_rows,
    bench_stream_write_all_rows,
    bench_stream_read_row_raw,
    bench_stream_write_row_raw,
    bench_stream_skip_row,
);
criterion_main!(benches);
