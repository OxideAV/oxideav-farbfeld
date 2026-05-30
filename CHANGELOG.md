# Changelog

All notable changes to `oxideav-farbfeld` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.3](https://github.com/OxideAV/oxideav-farbfeld/compare/v0.0.2...v0.0.3) - 2026-05-24

### Other

- add cargo-fuzz decode target + fix 3 streaming DoS amplifications
- Round 77: streaming reader/writer, DoS hardening, magick cross-validator

### Added

- `FarbfeldStreamReader` / `FarbfeldStreamWriter`: row-at-a-time
  decode / encode against `std::io::Read` / `std::io::Write`. Avoids
  holding the whole `width * height * 8`-byte body in memory; useful
  for >100 MP farbfeld files where the in-memory `parse_farbfeld` /
  `encode_farbfeld_image` shape would allocate gigabytes.
- `parse_farbfeld_header` + `FarbfeldHeader`: decode the 16-byte
  header without reading the body. Lets callers inspect the
  dimensions and reject over-large images before committing to a
  pixel-buffer allocation.
- `cargo-fuzz` decode target (`fuzz/fuzz_targets/decode.rs`): drives
  `parse_farbfeld`, `parse_farbfeld_header`, and `FarbfeldStreamReader`
  against arbitrary attacker bytes (must never panic), cross-checks the
  whole-file vs streaming verdict, and asserts the decode → encode
  roundtrip is byte-identical on inputs that parse. 37M executions /
  120 s clean.

### Changed

- `parse_farbfeld` now cross-checks the announced `width * height * 8`
  body length against the actual input length **before** allocating
  the decoded pixel buffer. A maliciously-crafted 16-byte file
  announcing huge dimensions is now refused in microseconds instead
  of triggering a multi-gigabyte allocation.

### Fixed

- DoS hardening of `FarbfeldStreamReader` (found by the new fuzz
  target). Three allocation/CPU amplifications where a tiny crafted
  header forced an outsized response, all now bounded by the body bytes
  actually delivered:
  - `read_all_rows` pre-allocated the *announced* `width·height·4`
    samples (a 16-byte file announcing 65 536×65 536 ⇒ ~34 GB) before
    reading any body — now grown one row at a time.
  - `FarbfeldStreamReader::new` pre-allocated the *announced* `width·8`
    row buffer (a 21-byte file announcing `width = 0x29000000` ⇒
    ~5.9 GB/row) — the per-row read is now length-bounded via
    `Read::take`, so the buffer grows only to the bytes delivered.
  - `read_all_rows` looped `height` (up to 2³²) empty iterations on a
    zero-width image — now short-circuits since a zero-width body is
    empty whatever the height.

### Tested

- New `tests/streaming.rs`: row-major streaming reader and writer
  match the whole-file `parse_farbfeld` / `encode_farbfeld_*` API
  byte-for-byte across 1×1..128×64 image sizes.
- New `tests/dos_hardening.rs`: regression for the body-length
  cross-check; 65 535×65 535 crafted header is refused inside 500 ms
  without allocating the announced 32 GB. Extended with three streaming
  regressions covering the `read_all_rows` / constructor allocation
  bounds and the zero-width short-circuit.
- New `tests/magick_xv.rs`: opaque-process cross-validator that
  round-trips our encoder output through ImageMagick's farbfeld
  coder and re-parses it with us, and decodes magick-produced
  farbfeld files with our parser. Test is a runtime no-op when
  `magick` is absent (no `#[ignore]`).

## [0.0.2](https://github.com/OxideAV/oxideav-farbfeld/compare/v0.0.1...v0.0.2) - 2026-05-05

### Other

- silence clippy --all-targets warnings

### Added

- Initial round 1: full coverage of the farbfeld spec from
  `farbfeld(5)`. Standalone parser (`parse_farbfeld`) and encoder
  (`encode_farbfeld`, `encode_farbfeld_from_rgba16`,
  `encode_farbfeld_image`) plus crate-local `FarbfeldImage` /
  `FarbfeldError` types.
- Default-on `registry` Cargo feature wires `oxideav-core` `Decoder` /
  `Encoder` trait impls, codec/container registration, and
  `PixelFormat::Rgba64Le` mapping. `default-features = false` builds
  the crate framework-free.
- Container demuxer + muxer + `ff` / `farbfeld` extensions + magic
  probe.
- Hard-asserted self-roundtrip + bit-exact byte compare against
  hand-built reference files in `tests/roundtrip.rs`. Framework
  integration roundtrip in `tests/registry.rs`.
