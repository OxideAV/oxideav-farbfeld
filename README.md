# oxideav-farbfeld

Pure-Rust farbfeld reader/writer.

[farbfeld](http://tools.suckless.org/farbfeld/) is suckless's minimalist
lossless image format: 16 bytes of header followed by 8 bytes per pixel
(four 16-bit big-endian channels in `R, G, B, A` order, row-major). No
compression, no metadata, no animation. The complete spec lives in the
public `farbfeld(5)` man page.

This crate is part of the [OxideAV](https://github.com/OxideAV)
workspace and was written from scratch against that man page — no
suckless source, no third-party Rust farbfeld crate, and no `image`
crate's farbfeld submodule were consulted.

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

| Capability                      | Status                            |
|---------------------------------|-----------------------------------|
| Parse (whole-file)              | full                              |
| Parse (streaming, row-at-a-time)| full — `FarbfeldStreamReader`     |
| Encode (whole-file)             | full                              |
| Encode (streaming, row-at-a-time)| full — `FarbfeldStreamWriter`    |
| Round-trip (self)               | exact                             |
| Round-trip (vs `magick`)        | exact (bit-identical, when present)|
| Container demux                 | full                              |
| Container mux                   | full                              |
| DoS hardening (crafted header)  | refuses without allocating body   |

## API

Standalone (no `oxideav-core` dependency, build with
`default-features = false`):

```rust
use oxideav_farbfeld::{
    encode_farbfeld, encode_farbfeld_from_rgba16, parse_farbfeld,
    FarbfeldImage, FarbfeldError,
};

// Encode from native-endian [R, G, B, A] u16 pixels.
let pixels = [[0xFFFFu16, 0x0000, 0x0000, 0xFFFF]];
let bytes = encode_farbfeld_from_rgba16(1, 1, &pixels)?;

// Or pass a pre-serialised big-endian RGBA u16 body verbatim.
let body_be = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF];
let bytes = encode_farbfeld(1, 1, &body_be)?;

// Parse a complete farbfeld byte stream.
let img: FarbfeldImage = parse_farbfeld(&bytes)?;
assert_eq!(img.width, 1);
assert_eq!(img.height, 1);
assert_eq!(img.pixels, [0xFFFF, 0x0000, 0x0000, 0xFFFF]);
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
