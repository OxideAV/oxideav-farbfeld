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

Round 1 covers the entire spec — parser, encoder, and registry-side
trait integration. Self-roundtrip and bit-exact byte compares against
hand-built reference files are hard-asserted in `tests/`.

| Capability        | Status |
|-------------------|--------|
| Parse             | full   |
| Encode            | full   |
| Round-trip        | exact  |
| Container demux   | full   |
| Container mux     | full   |

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
