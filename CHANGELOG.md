# Changelog

All notable changes to `oxideav-farbfeld` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
