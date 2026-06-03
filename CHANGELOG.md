# Changelog

All notable changes to `oxideav-farbfeld` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Hot-path codec loops auto-vectorised. The per-sample big-endian byte
  swap on the five non-memcpy entry points — `parse_farbfeld`,
  `encode_farbfeld_from_rgba16`, `encode_farbfeld_image`,
  `FarbfeldStreamReader::{read_row, read_all_rows}`,
  `FarbfeldStreamWriter::write_row` — now routes through two shared
  internal helpers (`decode_be_samples` / `encode_be_samples`) that
  walk the input as `chunks_exact(2)` / `chunks_exact_mut(2)` in
  lockstep with the output. The lockstep shape lets the auto-vectoriser
  emit a SIMD bswap (PSHUFB on x86_64, REV16 on aarch64) instead of
  the scalar per-sample `from_be_bytes` / `to_be_bytes` loop the
  previous code generated. Measured speedup on the 1024×1024
  release-build bench: `parse_whole` ~3.6 → ~39 GiB/s,
  `encode_from_rgba16` ~5.4 → ~47 GiB/s, `encode_image` ~4.7 → ~46
  GiB/s, `stream_read_all_rows` ~2.3 → ~8 GiB/s,
  `stream_write_all_rows` ~7.9 → ~10 GiB/s. `encode_raw_be` stays at
  the ~78 GiB/s memcpy ceiling. The whole-image encoders also build
  their output into a pre-sized `Vec<u8>` and write the 16-byte header
  with three `copy_from_slice` calls instead of three growing
  `extend_from_slice` calls, removing the small per-call overhead the
  64×64 numbers were dominated by. New unit tests cover the helpers'
  unit-length, long-run, empty, and asymmetric-input edges, plus a
  parse/encode inversion check on every `u16` 0..1024 and a
  `[[u16; 4]] -> [u16]` cast aliasing check at every offset.

### Added

- `FarbfeldStreamReader::skip_row` + `skip_rows(n)`: row-window decode
  primitives that consume exactly `width * 8` body bytes per row from
  the underlying reader without performing the per-sample big-endian
  to native-endian decode. Useful for thumbnail / scan-line-window
  callers that want rows N..M of a multi-gigapixel stream without
  paying the conversion cost for rows they'll discard. `skip_rows`
  caps at [`rows_remaining`](FarbfeldStreamReader::rows_remaining) so
  asking past the end returns the count actually skipped instead of
  an error. Both methods inherit the same length-bounded `Read::take`
  discipline as `read_row`, so a header announcing a multi-gigabyte
  row width but shipping no body still surfaces as a truncation
  error without forcing the announced-width allocation.
- `peek_farbfeld_dimensions(bytes)`: top-level convenience that decodes
  the 16-byte farbfeld header off the front of `bytes` without
  touching the body. Useful for sandboxes that want to refuse
  over-large images before allocating the pixel buffer. Operates on
  whatever prefix the caller supplies (a 16-byte slice, a memory-
  mapped file, an in-flight buffer), so the pre-flight check costs
  exactly the header parse.
- `FarbfeldHeader::total_len()`: announced on-disk file size
  (`HEADER_LEN + body_len` = `16 + width * height * 8`) with a
  checked-add so the degenerate 32-bit overflow case surfaces as
  `FarbfeldError::InvalidData` instead of a panic. The whole-file
  `parse_farbfeld` now dogfoods this method for its own body-length
  cross-check.
- Encode-side `cargo-fuzz` target (`fuzz/fuzz_targets/encode.rs`).
  Interprets the fuzz bytes as a `(width, height, body)` triple
  bounded to 64×64 (16 KiB body cap per execution) and drives all
  three whole-file encoder entry points (`encode_farbfeld`,
  `encode_farbfeld_from_rgba16`, `encode_farbfeld_image`) plus
  `FarbfeldStreamWriter` row-by-row. Asserts six invariants per
  input — no panics, three-encoder byte-agreement, streaming-writer-
  equals-whole-file byte-agreement, lossless parse roundtrip, exact
  `HEADER_LEN + w*h*8` size identity, and ASCII-magic / dimension
  header echo — plus two rejection probes (a body 1 byte short /
  1 byte long must reject; calling `FarbfeldStreamWriter::finish`
  before any row is written must reject). Complement to the
  pre-existing decode-side fuzz target. 601 312 executions / 61 s
  clean on the current build.
- Criterion micro-benchmark suite (`benches/codec.rs`). Six groups —
  `parse_whole`, `encode_raw_be`, `encode_from_rgba16`, `encode_image`,
  `stream_read_all_rows`, `stream_write_all_rows` — each parameterised
  over three image sizes (64×64, 256×256, 1024×1024) so future changes
  to the parse / encode / streaming paths can be regression-checked on
  both per-call constant cost and per-byte throughput in one run. Wired
  via a `[dev-dependencies]` Criterion with default features disabled
  and a `[[bench]] harness = false` entry, so `cargo build` /
  `cargo test` of the library itself stays lean.
- Deterministic property-style sweep (`tests/property_sweep.rs`): 96
  pseudo-random `(width, height, pixels)` triples per shape
  distribution × six distributions (tiny, square, tall-narrow,
  wide-short, zero-axis, medium) assert the eight spec-mandated
  invariants — lossless roundtrip, exact size identity, header echo,
  encoder determinism, three-encoder-path byte-agreement, streaming /
  whole-file byte-agreement, idempotent re-encode, and header-peek /
  whole-file decode agreement. Four malformed-input scenarios
  (arbitrary bytes never panic, corrupted magic always rejected,
  trailing garbage always rejected, truncated body always rejected)
  run additional PRNG-driven sweeps. Uses an inline xorshift32 PRNG
  so the sweep stays offline / no-extra-dep, and every assertion
  prints its seed so any future failure is reproducible.

### Changed

- README, `lib.rs` module doc, `roundtrip.rs` comment, and
  `Cargo.toml` description rephrased to reference the workspace's own
  factual byte-layout description at
  `docs/image/farbfeld/farbfeld-format.md` instead of external
  reference points.
- Same realignment now extended to `src/parser.rs` and
  `src/encoder.rs` module docs — both previously cited the upstream
  `farbfeld(5)` man page; that project-shipped documentation is
  treated as link-only by this workspace's clean-room policy and
  isn't the source the implementation was built against. The
  `src/lib.rs` summary now also names `docs/image/farbfeld/farbfeld-format.md`
  explicitly (rather than "a single man page"), keeping the source
  citation consistent across the whole crate.

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
