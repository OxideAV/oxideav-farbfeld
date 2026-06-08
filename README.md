# oxideav-farbfeld

Pure-Rust farbfeld reader/writer.

farbfeld is a minimalist lossless image format: 16 bytes of header
followed by 8 bytes per pixel (four 16-bit big-endian channels in
`R, G, B, A` order, row-major). No compression, no metadata, no
animation. The complete spec fits on a single man page.

This crate is part of the [OxideAV](https://github.com/OxideAV)
workspace and was written from scratch against an independently-authored
factual description of the byte layout
(`docs/image/farbfeld/farbfeld-format.md`).

## Status

Round 1 covered the entire spec — parser, encoder, and registry-side
trait integration. Self-roundtrip and bit-exact byte compares against
hand-built reference files are hard-asserted in `tests/`.

Round 2 hardened the parser against a malicious 16-byte header
announcing a multi-gigabyte body (the body length is now cross-checked
*before* the pixel buffer allocation), added a row-at-a-time
[`FarbfeldStreamReader`] / [`FarbfeldStreamWriter`] API that decodes /
encodes without holding the whole image in memory, and added a
`magick`-cross-validator integration test that round-trips through
ImageMagick's farbfeld coder.

Round 3 added a `cargo-fuzz` decode target (`fuzz/fuzz_targets/decode.rs`)
that drives `parse_farbfeld`, `parse_farbfeld_header`, and the streaming
reader against arbitrary bytes and cross-checks the decode → encode
roundtrip. It surfaced three allocation/CPU DoS amplifications in
`FarbfeldStreamReader`, all now fixed: the convenience `read_all_rows`
drain pre-allocated the *announced* `width·height·4` samples, the
constructor pre-allocated the *announced* `width·8` row buffer, and the
zero-width case looped `height` (up to 2³²) empty iterations. Each is now
bounded by the bytes actually delivered; regression tests live in
`tests/dos_hardening.rs`.

Round 7 added a dimension-only peek convenience
([`peek_farbfeld_dimensions`]) and a [`FarbfeldHeader::total_len`]
method (= `16 + width * height * 8`) so sandboxes and pre-flight size
checks can read the 16-byte header off a file (or its prefix) and
refuse over-large images before allocating the body. The two surfaces
are dogfooded by the whole-file decoder's existing announced-body
cross-check. The round also realigned three internal module-doc
citations from the upstream `farbfeld(5)` man page (project-shipped
documentation, treated as link-only per this workspace's clean-room
policy) to the workspace's own independent factual byte-layout
description at `docs/image/farbfeld/farbfeld-format.md`.

Round 8 added row-window decode primitives to the streaming reader —
[`FarbfeldStreamReader::skip_row`] consumes exactly `width * 8` body
bytes without performing the per-sample big-endian decode (advancing
[`FarbfeldStreamReader::rows_read`] by one), and `skip_rows(n)` caps
at the rows remaining instead of erroring past the end. The two are
the symmetric counterpart to [`FarbfeldStreamReader::read_row`] for
callers that only want rows N..M of a multi-gigapixel stream (thumbnail
row, scan-line inspection, partial decode) and don't want to pay the
conversion cost for rows they'll discard. Both inherit the same
length-bounded `Read::take` discipline as `read_row`, so a malicious
header announcing a multi-gigabyte row width but shipping no body still
surfaces as a truncation error without forcing the announced-width
allocation.

Round 6 added the symmetric encode-side fuzz target
(`fuzz/fuzz_targets/encode.rs`). The fuzz bytes are interpreted as
`(width, height, body)` triples bounded to 64×64 (16 KiB body cap per
execution, well inside libFuzzer's iteration budget) and every input
drives all three whole-file encoder entry points (`encode_farbfeld`,
`encode_farbfeld_from_rgba16`, `encode_farbfeld_image`) plus
`FarbfeldStreamWriter` row-by-row. Six invariants are asserted on every
input: no-panics, three-encoder agreement, streaming-writer-equals-
whole-file agreement, lossless parse roundtrip, exact-size identity, and
header echo. Each input also probes two rejection paths (one-byte-short
and one-byte-long body lengths must reject; premature `finish` must
reject). 601 312 executions / 61 s clean against the new corpus.

Round 4 added a Criterion micro-benchmark suite (`benches/codec.rs`)
covering both the in-memory and streaming codecs across three image
sizes (64×64, 256×256, 1024×1024). Six bench groups exercise the six
public encode/parse entry points so future changes can spot regressions
on either path. Round 12 extended the suite with two more groups —
`stream_read_row_raw` and `stream_write_row_raw` — that exercise the
raw-bytes pass-through pair (`FarbfeldStreamReader::read_row_raw` /
`FarbfeldStreamWriter::write_row_raw`) row-by-row across the same three
sizes, guarding the round-11 perf claim that the raw path is faster
than its native-endian sibling because it skips the per-sample BE swap.
At 1024×1024 the raw read path measures ~36 GiB/s and the raw write
path ~10 GiB/s on the bench host. Round 5 added a deterministic property-style sweep
(`tests/property_sweep.rs`): 96 pseudo-random `(width, height, pixels)`
triples per shape distribution × six distributions (tiny, square,
tall-narrow, wide-short, zero-axis, medium) each assert the eight
spec-mandated invariants — lossless roundtrip, exact size, header echo,
encoder determinism, three-encoder-path agreement, streaming /
whole-file agreement, idempotent re-encode, and peek / decode
agreement. Four malformed-input scenarios (arbitrary bytes never panic,
corrupted magic always rejected, trailing garbage always rejected,
truncated body always rejected) run additional PRNG-driven sweeps.
The sweep is offline / no-extra-dep (xorshift32 inlined) so any
failure is reproducible from the seed printed in the assertion.

Round 13 added a small family of spatial accessors on
[`FarbfeldImage`] — `pixel(x, y) -> Option<[u16; 4]>`,
`set_pixel(x, y, [R, G, B, A])`, `channel(x, y, c) -> Option<u16>`,
`row(y) -> Option<&[u16]>`, `row_mut(y) -> Option<&mut [u16]>`, and
`pixel_count() -> usize` — that let callers index the decoded frame
by `(x, y)` (or by single channel, or by whole scan-line) without
re-implementing the `(y * width + x) * 4` row-major arithmetic at
every call site. All accessors bounds-check against `width` /
`height` (and the channel index against `4`) and return `Option`,
so a caller looping over a fixed grid against a smaller frame can't
accidentally panic. A new crate-level constant `CHANNELS_PER_PIXEL`
(= `4`) is also exported so external offset arithmetic can name the
channel count instead of hard-coding `4`. Fourteen unit tests cover
the in-bounds reads/writes, out-of-bounds rejections, zero-width
and zero-height edges, per-channel reads, single-row overwrite,
and a row-major-layout consistency round-trip on a 3×2 image. Pure
addition — no behaviour change to the parser, encoder, or
streaming I/O.

Round 11 added a raw-bytes pass-through pair on the streaming API —
[`FarbfeldStreamReader::read_row_raw`] yields the next row's on-disk
`width * 8` big-endian bytes verbatim into a caller `&mut [u8]` slot,
and [`FarbfeldStreamWriter::write_row_raw`] accepts an already-BE-encoded
`width * 8`-byte row and forwards it to the underlying writer
unchanged. Both methods share the same row-bytes pump, bounded
`Read::take` / `Write::write_all` discipline, and row-count accounting
as their native-endian counterparts ([`FarbfeldStreamReader::read_row`]
/ [`FarbfeldStreamWriter::write_row`]), so a stream may mix the raw and
native paths row-by-row in either direction. The reader's raw path is
the symmetric counterpart to [`FarbfeldStreamReader::skip_row`] for
callers that want the bytes *and* want them as bytes; the writer's raw
path closes the loop for pipelines reading from one farbfeld source and
forwarding the body to another consumer (proxies, hash-and-discard,
re-muxing to another 16-bit-BE container) without paying the
native-endian round-trip. Ten unit tests cover the new surfaces:
read-side byte equality against the synthesised reference, mid-stream
mixing with `read_row` and `skip_row`, zero-width and truncated-body
edges, write-side byte equality against the reference, extra-row /
wrong-length / zero-width / mixed-mode rejection, and an end-to-end
`read_row_raw` → `write_row_raw` byte-identical passthrough.

Round 10 added a third `cargo-fuzz` target
(`fuzz/fuzz_targets/stream_io.rs`) that exercises `FarbfeldStreamReader`
and `FarbfeldStreamWriter` through a chunked I/O transport
(`ChoppyReader` / `ChoppyWriter`) whose `read` / `write` calls return
short, mid-sized, or full-buffer chunks drawn from a deterministic
xorshift32 schedule seeded by the fuzz input. The existing decode /
encode targets drive their input through a bulk `Cursor` / byte slice,
so neither exercises the streaming reader's bounded `Read::take` +
`read_to_end` discipline or the streaming writer's `Write::write_all`
discipline under choppy I/O — a genuinely different surface that fails
silently if `write_all` stops looping on a short success or `Read::take`
stops respecting its length cap. The target asserts five invariants on
every input: no panics; chunked decode equals bulk `parse_farbfeld`
decode; chunked encode equals bulk `encode_farbfeld_image` encode; the
streaming roundtrip closes; and a `skip_row`-only walk covers `height`
rows. A body-truncation rejection path is also probed on every non-
empty body. 2 548 637 executions / 61 s clean against the new corpus
(cov 382 ft 1396 corp 117), no panics, no truncation regressions.

Round 9 hoisted the per-sample big-endian byte swap on the five
non-memcpy hot paths into two shared internal helpers
(`decode_be_samples` / `encode_be_samples`) that walk the input as
`chunks_exact(2)` / `chunks_exact_mut(2)` in lockstep with the output,
which the auto-vectoriser turns into a SIMD bswap. The whole-image
encoder (`encode_farbfeld_image`) and the `[[u16; 4]]`-shaped
convenience encoder (`encode_farbfeld_from_rgba16`) now also build
their output into a pre-sized `Vec<u8>` and `copy_from_slice` the
16-byte header in one store instead of three `extend_from_slice`
calls, removing the four-times-per-pixel push that previously kept
the loop scalar. The `[[u16; 4]] -> [u16]` cast that lets the
convenience encoder share the same flat hot loop is a pure-Rust
borrow-reinterpret (no extra crate dep, layout argument written out
in the source), and the new shared helpers carry unit tests for the
unit-length, long-run, empty, and asymmetric-input edges plus a
parse/encode inversion check. Measured speedups on the
1024×1024 release-build bench (4 MiB body, single sample,
`cargo bench --bench codec -- --quick`):

| Group at 1024×1024            | Throughput (4 MiB body)    |
|-------------------------------|----------------------------|
| `parse_whole`                 | ~39 GiB/s (was ~3.6 GiB/s) |
| `encode_raw_be`               | ~78 GiB/s (memcpy-bound)   |
| `encode_from_rgba16`          | ~47 GiB/s (was ~5.4 GiB/s) |
| `encode_image`                | ~46 GiB/s (was ~4.7 GiB/s) |
| `stream_read_all_rows`        | ~8 GiB/s (was ~2.3 GiB/s)  |
| `stream_write_all_rows`       | ~10 GiB/s (was ~7.9 GiB/s) |

Run with `cargo bench -p oxideav-farbfeld` (or
`cargo bench -p oxideav-farbfeld -- parse_whole` to scope to one
group).

| Capability                      | Status                            |
|---------------------------------|-----------------------------------|
| Parse (whole-file)              | full                              |
| Parse (streaming, row-at-a-time)| full — `FarbfeldStreamReader` (incl. `read_row_raw` / `skip_row` / `skip_rows`) |
| Encode (whole-file)             | full                              |
| Encode (streaming, row-at-a-time)| full — `FarbfeldStreamWriter` (incl. `write_row_raw`) |
| Round-trip (self)               | exact                             |
| Round-trip (vs `magick`)        | exact (bit-identical, when present)|
| Container demux                 | full                              |
| Container mux                   | full                              |
| DoS hardening (crafted header)  | whole-file + streaming bounded by delivered bytes |
| Fuzzing (decode)                | `cargo-fuzz` target, no known crashes |
| Fuzzing (encode)                | `cargo-fuzz` target (3 whole-file paths + streaming writer agree, 6 invariants + 2 rejection probes), 601 312 runs / 61 s clean |
| Fuzzing (streaming I/O)         | `cargo-fuzz` target — `FarbfeldStreamReader` / `FarbfeldStreamWriter` over a choppy `Read` / `Write` transport (1..=8-byte chunks), 5 invariants + truncation probe, 2 548 637 runs / 61 s clean |
| Property sweep (PRNG)           | 8 invariants × 6 shape distributions × 96 iters + 4 malformed-input scenarios |

## API

Standalone (no `oxideav-core` dependency, build with
`default-features = false`):

```rust
use oxideav_farbfeld::{
    encode_farbfeld, encode_farbfeld_from_rgba16, parse_farbfeld,
    peek_farbfeld_dimensions, FarbfeldImage, FarbfeldError,
};

// Encode from native-endian [R, G, B, A] u16 pixels.
let pixels = [[0xFFFFu16, 0x0000, 0x0000, 0xFFFF]];
let bytes = encode_farbfeld_from_rgba16(1, 1, &pixels)?;

// Or pass a pre-serialised big-endian RGBA u16 body verbatim.
let body_be = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF];
let bytes = encode_farbfeld(1, 1, &body_be)?;

// Cheap pre-flight: read the 16-byte header off the file and
// reject over-large images before allocating the body.
let header = peek_farbfeld_dimensions(&bytes[..16])?;
assert_eq!(header.width, 1);
assert_eq!(header.height, 1);
assert_eq!(header.total_len()?, bytes.len()); // 16 + 1*1*8

// Parse a complete farbfeld byte stream.
let img: FarbfeldImage = parse_farbfeld(&bytes)?;
assert_eq!(img.width, 1);
assert_eq!(img.height, 1);
assert_eq!(img.pixels, [0xFFFF, 0x0000, 0x0000, 0xFFFF]);

// Spatial accessors (round 13) — index by (x, y) instead of
// reaching into `pixels` with manual `(y * w + x) * 4` arithmetic.
assert_eq!(img.pixel(0, 0), Some([0xFFFF, 0x0000, 0x0000, 0xFFFF]));
assert_eq!(img.channel(0, 0, 0), Some(0xFFFF)); // R
assert_eq!(img.pixel(99, 99), None);            // out of bounds
# Ok::<(), FarbfeldError>(())
```

Streaming (row-at-a-time, no whole-image buffer needed):

```rust
use std::io::Cursor;
use oxideav_farbfeld::{FarbfeldStreamReader, FarbfeldStreamWriter};

// Encode a 2-row image one row at a time.
let mut writer = FarbfeldStreamWriter::new(Vec::new(), 1, 2).unwrap();
writer.write_row(&[0xFFFF, 0, 0, 0xFFFF]).unwrap();
writer.write_row(&[0, 0xFFFF, 0, 0xFFFF]).unwrap();
let bytes = writer.finish().unwrap();

// Decode it back, one row at a time.
let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
let mut row = [0u16; 4];
while reader.read_row(&mut row).unwrap() {
    // do something with this row...
}
```

Registry-integrated (default; pulls in `oxideav-core`):

```ignore
use oxideav_farbfeld::register;
let mut codecs = oxideav_core::CodecRegistry::new();
let mut containers = oxideav_core::ContainerRegistry::new();
register(&mut codecs, &mut containers);
```

## Cargo features

| Feature    | Default | Effect |
|------------|---------|--------|
| `registry` | yes     | Pulls `oxideav-core` and registers the codec + container with the framework. Disable for `oxideav-core`-free builds. |

## License

MIT — see [LICENSE](LICENSE). Copyright Karpelès Lab Inc.
