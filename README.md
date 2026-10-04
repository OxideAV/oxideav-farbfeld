# oxideav-farbfeld

[![CI](https://github.com/OxideAV/oxideav-farbfeld/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-farbfeld/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-farbfeld.svg)](https://crates.io/crates/oxideav-farbfeld) [![docs.rs](https://docs.rs/oxideav-farbfeld/badge.svg)](https://docs.rs/oxideav-farbfeld) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust **farbfeld** reader and writer for the
[`oxideav`](https://github.com/OxideAV/oxideav) framework. farbfeld is
a minimalist lossless image format: 16 bytes of header followed by 8
bytes per pixel (four 16-bit big-endian channels in `R, G, B, A` order,
row-major). No compression, no metadata, no animation. Written from
scratch against an independently-authored factual description of the
byte layout (`docs/image/farbfeld/farbfeld-format.md`). Follows the
OxideAV image-crate API contract (`IMAGE_CRATE_API`), so it reads like
`oxideav-png` / `oxideav-qoi` / every other OxideAV image crate.

## Standalone use

```toml
[dependencies]
oxideav-farbfeld = { version = "0.0", default-features = false }   # no oxideav-core
```

```rust
let bytes = std::fs::read("in.ff")?;
if oxideav_farbfeld::probe(&bytes) {
    let info = oxideav_farbfeld::info(&bytes)?;      // header only: width, height, format
    let img  = oxideav_farbfeld::decode(&bytes)?;    // FarbfeldImage, native Rgba64Le plane
    let rgba: Vec<u8> = img.to_rgba8();              // tightly packed RGBA8 (high byte per sample)
    let (w, h) = (img.width(), img.height());

    let opts = oxideav_farbfeld::EncodeOptions::default();
    let out: Vec<u8> = oxideav_farbfeld::encode_rgba8(w, h, &rgba, &opts)?;
    std::fs::write("out.ff", out)?;
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

| Item | Signature |
|---|---|
| `probe` | `fn(&[u8]) -> bool` — `farbfeld` magic sniff; total, allocation-free |
| `info` | `fn(&[u8]) -> Result<ImageInfo>` — 16-byte header only: `width`, `height`, `format` (`Rgba64Le`), `frames` (1), `has_alpha` (true), `color`, `has_icc` / `has_exif` / `has_xmp` (false); `body_len()` / `file_len()` / `header()` |
| `decode` / `decode_with` | `fn(&[u8]) -> Result<FarbfeldImage>`, `fn(&[u8], &DecodeOptions) -> Result<FarbfeldImage>` — native layout, one tightly packed plane |
| `decode_rgb8` / `decode_rgba8` | `-> Result<RgbImage>` / `-> Result<RgbaImage>` (`{ width, height, data }`, 3 / 4 bytes per pixel) |
| `decode_rgba16` / `decode_rgba16_with` | `-> Result<Rgba16Image>` — the lossless 16-bit sample view (depth) |
| `decode_from` / `decode_from_with` | `fn<R: Read>(R) -> Result<FarbfeldImage>` — genuinely streaming, row by row |
| `encode` | `fn(&FarbfeldImage, &EncodeOptions) -> Result<Vec<u8>>` |
| `encode_rgb8` / `encode_rgba8` | `fn(w, h, &[u8], &EncodeOptions) -> Result<Vec<u8>>` — widened to 16-bit |
| `encode_rgba16` | `fn(w, h, &[u16], &EncodeOptions) -> Result<Vec<u8>>` — native-endian samples (depth) |
| `encode_to` | `fn<W: Write>(&FarbfeldImage, &EncodeOptions, W) -> Result<()>` — streaming, byte-identical to `encode` |

`FarbfeldImage { width, height, format: PixelFormat, planes: Vec<Plane>,
color: ColorInfo, metadata: Metadata }` with `new` / `packed` /
`from_rgb8` / `from_rgba8` / `from_rgba16` / `from_rgba64le`
(geometry-validated, `Result`), `width()` / `height()` / `format()` /
`stride()` / `as_bytes()` / `into_raw()` / `to_rgb8()` / `to_rgba8()` /
`to_rgba16()`. farbfeld has no palette, so there is no `palette` field.
`Error` = `FarbfeldError { InvalidData, Unsupported, LimitExceeded,
Io(std::io::Error) }`.

The pre-contract entry points (`parse_farbfeld`, `parse_farbfeld_header`,
`peek_farbfeld_dimensions`, `encode_farbfeld`,
`encode_farbfeld_from_rgba16`, `encode_farbfeld_image`,
`register_runtime`) remain for one release as `#[deprecated]` wrappers
with byte-identical output. The old `FarbfeldImage { width, height,
pixels: Vec<u16> }` is now `Rgba16Image { width, height, data }` (the
name `FarbfeldImage` denotes the contract image); `register` now takes
`&mut RuntimeContext` and the two-registry form is `register_registries`.

## Framework use

The default `registry` feature pulls in `oxideav-core`:

```rust
let mut ctx = oxideav_core::RuntimeContext::new();
oxideav_farbfeld::register(&mut ctx);     // codec "farbfeld" + container (demux/mux/probe, .ff/.farbfeld)
// or: register_codecs(&mut ctx.codecs) / register_containers(&mut ctx.containers)
# let params = oxideav_core::CodecParameters::video(oxideav_core::CodecId::new("farbfeld"));
let dec = oxideav_farbfeld::make_decoder(&params)?;   // oxideav_core::Decoder
let enc = oxideav_farbfeld::make_encoder(&params)?;   // oxideav_core::Encoder
# Ok::<(), oxideav_core::Error>(())
```

`From<FarbfeldImage> for VideoFrame` (one packed `Rgba64Le` plane; a
colour-signal side-channel only for a caller-set `color` other than the
sRGB convention) and
`FarbfeldImage::from_video_frame(&VideoFrame, &CodecParameters) ->
Result<_, FarbfeldError>` / `TryFrom<(&VideoFrame, &CodecParameters)>`
bridge the two layers; `FarbfeldPixelFormat` ↔ `oxideav_core::PixelFormat`
map 1:1 by name. The framework `Decoder` / `Encoder` are thin adapters
over the standalone `decode` / `encode`: the decoder threads the
`Packet`'s `pts` onto the frame, attaches the colour signal and overrides
`reset()` (a reused decoder returns to `NeedMore`, not `Eof`); the encoder
takes `width` / `height` from `CodecParameters`, always declares
`Rgba64Le`, skips stride padding, rejects a plane too short for the
geometry, and marks every packet a keyframe. The demuxer validates the
header and the exact file length through `info` and hands the whole file
to the decoder as one packet; the muxer writes the packet through.

## Supported layouts

| Native layout | Decode | Encode | `to_rgb8` / `to_rgba8` |
|---|---|---|---|
| `Rgba64Le` (16-bit RGBA, 8 bytes/pixel, samples little-endian) | ✅ | ✅ | high byte per sample (alpha dropped / kept) |

That is the whole format: one layout, one plane, no palette, no
grayscale, no animation, zero dimensions legal (a 16-byte file). The
wire is big-endian; the contract image is `Rgba64Le` because that is
the layout `oxideav-core` and the rest of the fleet speak — the decoder
byte-swaps on the way in and the encoder on the way out, both exactly,
so `decode(encode(img)) == img` holds byte for byte and the file is
bit-identical across a round trip. `encode` writes the image as given (a
plane with row padding is written without it); there is no `Rgba64Le`
image farbfeld cannot represent, so `Error::Unsupported` is reserved for
geometry that overflows `usize` (and, under `registry`, for any other
pixel format).

8-bit input: `encode_rgb8` / `encode_rgba8` (and `from_rgb8` /
`from_rgba8`) widen every sample `v → v × 257` (`v << 8 | v`: `0 → 0`,
`255 → 65535`) and set alpha to `65535` for RGB. 8-bit output: `to_rgb8`
/ `to_rgba8` / `decode_rgb8` / `decode_rgba8` take the high byte of each
16-bit sample (`v >> 8`, truncation — the same reduction the other 16-bit
OxideAV image crates apply). The two are exact inverses:
`decode_rgba8(encode_rgba8(px)) == px`.

## Options

`DecodeOptions { max_width, max_height, max_pixels, max_bytes: Option<_>,
strict }` — `None` lifts a limit; the default caps the decoded plane
(`width × height × 8`) at 1 GiB and leaves dimensions unlimited;
`unlimited()` lifts everything. `strict` is accepted for contract
uniformity and has no effect: farbfeld has no advisory rules — the magic
and the exact body length are mandatory in both modes.

`EncodeOptions {}` — farbfeld has no encoder knobs (no compression level,
no colour tag, no metadata slot). The record exists for the uniform call
shape and is `#[non_exhaustive]`; construct it with `default()` / `new()`.

## Metadata and colour

farbfeld carries no ICC profile, Exif, XMP or gamma: `Metadata` is always
empty on a decoded image and the encoder ignores (cannot carry) whatever a
caller sets. The format also carries no colour tag of any kind — the
staged description records integer sample values only — so `color` is
the crate's documented **convention**, sRGB:

| `ColorInfo { range, primaries, transfer, matrix }` |
|---|
| `Full`, 1 (BT.709 / sRGB), 13 (IEC 61966-2-1 sRGB), 0 (identity / RGB) — `ColorInfo::farbfeld_default()` |

The convention is **not stamped on registry frames**: the file carries
no colour tag and the format defines no colour semantics, so a decoded
frame has no colour-signal side-channel (`from_video_frame` restores the
convention). A caller-set `color` is accepted, kept on the image,
forwarded to the frame's colour-signal side-channel when it differs
from the convention, and never written to the file.

## Limits

Every `DecodeOptions` limit is checked against the 16-byte header before
the plane is allocated (`Error::LimitExceeded`; a `width × height × 8`
that overflows `u64` counts as exceeding any finite byte cap).
Independently of the options, the whole-file decoder cross-checks the
announced body against the bytes actually present before allocating, so
a 16-byte file announcing a 28 GB image is rejected as truncated without
asking the allocator; the streaming reader pulls each row through a
bounded `Read::take`, so the same header fails on the first short row.
`probe` and `info` never allocate. A `width × height × 8` that does not
fit this host's `usize` is `Error::Unsupported`.

**Speed** (release build, Apple M4 Max, single thread,
`cargo bench --bench codec -- contract_12mp`): one 4000×3000 `Rgba64Le`
frame (12 MP, 96 000 000 body bytes):

| Path | Time | Throughput (body bytes) |
|---|---|---|
| `decode` (whole file → `FarbfeldImage`) | 2.06 ms | 43 GiB/s |
| `decode_from` (streaming, `Cursor`) | 3.04 ms | 29 GiB/s |
| `decode_rgba8` (→ 48 MB RGBA8) | 6.71 ms | 13 GiB/s |
| `encode` (`FarbfeldImage` → file) | 2.06 ms | 43 GiB/s |
| `encode_to` (streaming, `Vec` sink) | 2.20 ms | 41 GiB/s |

Both directions are a single vectorised pairwise byte swap over the body,
so they run at memory speed; the streaming variants add one bounded
`take` / `write_all` per row. `BENCHMARKS.md` has the per-entry-point
groups at 64², 256² and 1024².

## Streaming (depth API)

`FarbfeldStreamReader` / `FarbfeldStreamWriter` decode / encode one row
at a time in the native-endian `u16` sample space without holding the
whole image in memory. Both carry a raw-bytes pass-through pair
(`read_row_raw` / `write_row_raw`) that skips the per-sample byte swap,
plus `skip_row` / `skip_rows` for partial decode (thumbnail row,
scan-line inspection, "rows N..M of a multi-gigapixel stream"). Bulk
convenience on both ends: `read_all_rows` drains the whole body in one
call, `write_all_rows` / `write_all_rows_raw` emit a whole flat plane.
Streaming and whole-file output are byte-identical; `decode_from` /
`encode_to` are built on this pair. A premature end of input is
`Error::InvalidData` (truncated file); any other read / write failure is
`Error::Io`.

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

## 16-bit sample view (depth API)

`Rgba16Image { width, height, data: Vec<u16> }` is the native-endian
sample view: `decode_rgba16` reads it straight from the wire,
`encode_rgba16` writes it, and `FarbfeldImage::to_rgba16` /
`From<Rgba16Image>` convert losslessly both ways. It keeps the frame
accessors — random-access `pixel` / `set_pixel` / `channel` / `row` /
`row_mut` and the sequential `rows` / `rows_mut` / `pixels` iterators
(all `ExactSizeIterator` + `DoubleEndedIterator`) — so a caller can index
by `(x, y)` or walk the frame without re-implementing the
`(y * width + x) * 4` arithmetic.

```rust
use oxideav_farbfeld::{decode_rgba16, encode_rgba16, EncodeOptions};

let bytes = encode_rgba16(1, 1, &[0xFFFF, 0x0000, 0x0000, 0xFFFF], &EncodeOptions::default()).unwrap();
let img = decode_rgba16(&bytes).unwrap();
assert_eq!(img.pixel(0, 0), Some([0xFFFF, 0x0000, 0x0000, 0xFFFF]));
assert_eq!(img.channel(0, 0, 0), Some(0xFFFF)); // R
assert_eq!(img.pixel(99, 99), None);            // out of bounds
assert_eq!(img.rows().len(), 1);
```

## Robustness

- **DoS hardening** — see *Limits*; `tests/dos_hardening.rs` pins the
  no-allocation rejection of crafted headers, and
  `tests/dimension_overflow.rs` cross-checks `width*height*8` against a
  `u128` oracle for 4096 PRNG `u32` pairs plus boundary points (no panic,
  no silent wrap).
- **Round-trip property** — `tests/contract.rs` sweeps 53 random shapes
  (incl. zero-width / zero-height) of random 16-bit samples with the
  byte-order edge values through every contract path (`decode` /
  `decode_from` / `decode_with` / `decode_rgba16` / `to_rgba16` / `encode`
  / `encode_to` / `encode_rgba16` / padded planes / the 8-bit inverses /
  truncation / trailing bytes / exact limits), and pushes every one of
  the 65 536 sample values through all four channels.
  `tests/property_sweep.rs` asserts the eight spec-mandated invariants
  across six shape distributions on the pre-contract paths.
- **Validator** — `tests/magick_xv.rs` round-trips through ImageMagick's
  farbfeld coder (bit-identical) when the `magick` binary is present.
- **Fuzzing** — five `cargo-fuzz` targets: `contract` (`probe` / `info` /
  `decode` / `decode_with` / `decode_from` / raw paths / encode round
  trips / limits; 2.85 M runs in 120 s clean), `decode`, `encode`,
  `stream_io` (choppy `Read` / `Write` transport), `trait_roundtrip`
  (framework `Decoder` / `Encoder` / demuxer). The `contract` target found
  the one defect of this round on its first run: the streaming writer
  allocated its `width × 8` row buffer eagerly, so a zero-height image
  with a multi-gigapixel width reserved gigabytes — it is lazy now.

## Cargo features

| Feature    | Default | Effect |
|------------|---------|--------|
| `registry` | yes     | Pulls `oxideav-core` and exposes `register` / factories / the frame bridge. Disable for the `oxideav-core`-free standalone API (everything in *Standalone use*, *Streaming* and *16-bit sample view*). |

## License

MIT — see [LICENSE](LICENSE). Copyright Karpelès Lab Inc.
