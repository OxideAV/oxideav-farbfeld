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

Complete: the parser, encoder, container demux/mux, and registry-side
trait integration cover the entire farbfeld spec. Self-roundtrip and
bit-exact byte compares against hand-built reference files are
hard-asserted in `tests/`, and a `magick` cross-validator round-trips
through ImageMagick's farbfeld coder when the binary is present.

Beyond the core codec the crate ships:

- **Streaming I/O** — `FarbfeldStreamReader` / `FarbfeldStreamWriter`
  decode/encode one row at a time without holding the whole image in
  memory. Both carry a raw-bytes pass-through pair (`read_row_raw` /
  `write_row_raw`) that skips the per-sample big-endian swap, plus
  `skip_row` / `skip_rows` for partial decode (thumbnail row, scan-line
  inspection, "rows N..M of a multi-gigapixel stream"). Bulk convenience
  on both ends: `read_all_rows` drains the whole body in one call, and
  `write_all_rows` / `write_all_rows_raw` emit a whole flat plane (native
  or pre-serialised big-endian) in one call instead of a hand-written
  per-row loop. Streaming and whole-file output are byte-identical.
- **DoS hardening** — every announced body length is cross-checked
  against the bytes actually delivered *before* allocating, so a header
  announcing a multi-gigabyte body that ships nothing fails on the first
  short read having reserved nothing. A dimension-overflow sweep
  (`tests/dimension_overflow.rs`) cross-checks `width*height*8` against a
  `u128` oracle for 4096 PRNG `u32` pairs plus boundary points — no
  panic, no silent wrap.
- **Frame accessors** — random-access (`pixel` / `set_pixel` / `channel`
  / `row` / `row_mut`) and sequential iterators (`rows` / `rows_mut` /
  `pixels`), all `ExactSizeIterator` and `DoubleEndedIterator`, so a
  caller can index by `(x, y)` or walk the frame (in either direction)
  without re-implementing the `(y * width + x) * 4` row-major arithmetic.
- **Fuzzing** — three `cargo-fuzz` targets (`decode`, `encode`,
  `stream_io`) drive the whole-file paths and the streaming reader/writer
  over a choppy `Read`/`Write` transport; no known crashes.
- **Property sweep** — `tests/property_sweep.rs` asserts the eight
  spec-mandated invariants across six shape distributions plus four
  malformed-input scenarios, offline and reproducible from a printed
  seed.
- **Benchmarks** — a Criterion suite of eleven groups (`benches/codec.rs`,
  see `BENCHMARKS.md`) covers every public encode/parse entry point, the
  streaming raw/skip paths (per-row and bulk `skip_rows`), and the
  body-independent `peek_header` pre-flight, across three image sizes.
  The `BENCHMARKS.md` baseline table is fully populated (every cell a
  fresh point estimate). Run with `cargo bench -p oxideav-farbfeld`.

## Capability summary

| Capability                      | Status                            |
|---------------------------------|-----------------------------------|
| Parse (whole-file)              | full                              |
| Parse (streaming, row-at-a-time)| full — `FarbfeldStreamReader` (incl. `read_row_raw` / `skip_row` / `skip_rows` / bulk `read_all_rows`) |
| Encode (whole-file)             | full                              |
| Encode (streaming, row-at-a-time)| full — `FarbfeldStreamWriter` (incl. `write_row_raw` / bulk `write_all_rows` / `write_all_rows_raw`) |
| Round-trip (self)               | exact                             |
| Round-trip (vs `magick`)        | exact (bit-identical, when present)|
| Container demux                 | full                              |
| Container mux                   | full                              |
| DoS hardening (crafted header)  | whole-file + streaming bounded by delivered bytes; dimension-overflow sweep (4096 PRNG `u32` pairs + boundary points) cross-checks `width*height*8` vs a `u128` oracle, no panic / no silent wrap |
| Fuzzing (decode)                | `cargo-fuzz` target, no known crashes |
| Fuzzing (encode)                | `cargo-fuzz` target (3 whole-file paths + streaming writer agree, 6 invariants + 2 rejection probes), 601 312 runs / 61 s clean |
| Fuzzing (streaming I/O)         | `cargo-fuzz` target — `FarbfeldStreamReader` / `FarbfeldStreamWriter` over a choppy `Read` / `Write` transport (1..=8-byte chunks), 5 invariants + truncation probe, 2 548 637 runs / 61 s clean |
| Property sweep (PRNG)           | 8 invariants × 6 shape distributions × 96 iters + 4 malformed-input scenarios |
| Frame accessors                 | random-access (`pixel` / `set_pixel` / `channel` / `row` / `row_mut`) + sequential iterators (`rows` / `rows_mut` / `pixels`, all `ExactSizeIterator`) |

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

// Spatial accessors — index by (x, y) instead of
// reaching into `pixels` with manual `(y * w + x) * 4` arithmetic.
assert_eq!(img.pixel(0, 0), Some([0xFFFF, 0x0000, 0x0000, 0xFFFF]));
assert_eq!(img.channel(0, 0, 0), Some(0xFFFF)); // R
assert_eq!(img.pixel(99, 99), None);            // out of bounds

// Sequential iterators — walk the whole frame once
// without threading a (x, y) counter. All three are ExactSizeIterator.
assert_eq!(img.rows().len(), 1);
let quads: Vec<[u16; 4]> = img.pixels().collect();
assert_eq!(quads, [[0xFFFF, 0x0000, 0x0000, 0xFFFF]]);
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
