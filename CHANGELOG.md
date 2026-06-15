# Changelog

All notable changes to `oxideav-farbfeld` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Two new Criterion benchmark groups in `benches/codec.rs`, bringing the
  suite to eleven groups: `stream_skip_rows_bulk` exercises
  `FarbfeldStreamReader::skip_rows(height)` (the whole body skipped in one
  bulk call, the public counterpart to the per-row `stream_skip_row`
  floor), and `peek_header` exercises `peek_farbfeld_dimensions` +
  `total_len()` — the body-independent sandbox pre-flight that rejects an
  over-large image before allocating its body. The baseline run confirms
  `peek_header` is flat at ~1.6 ns per call across all three announced
  sizes (its work never touches the pixel array) and that bulk
  `skip_rows` edges out the per-row loop most at the small size
  (~38.0 vs ~35.4 GiB/s at 64×64). `BENCHMARKS.md` was re-measured in a
  single run: every cell of the baseline table is now a fresh point
  estimate (the earlier carry-over figures and dashes are gone), and two
  new explanatory sub-sections document the `peek_header` per-call-latency
  reading and the bulk-skip comparison. Bench/doc-only — no `src/`
  behaviour change.
- `FarbfeldImage`'s three frame iterators — `Rows` (from `rows()`),
  `RowsMut` (from `rows_mut()`), and `Pixels` (from `pixels()`) — now
  implement `DoubleEndedIterator`, so callers can walk scan lines or
  pixels back-to-front via `.rev()` or `.next_back()` (bottom-up row
  traversal, vertical flips, meet-in-the-middle scans) without
  re-indexing the flat `pixels` buffer by hand. The underlying
  `slice::chunks_exact` / `chunks_exact_mut` already supported reverse
  iteration; this surfaces it on the public iterator types. All three
  remain `ExactSizeIterator`, so `.rev()` keeps yielding exactly
  `height` rows / `pixel_count` pixels, including the degenerate
  `width == 0` case where every row is the same empty slice (front and
  back are indistinguishable, so reversed iteration yields the same
  `height` empty rows as the forward walk). Eight new unit tests cover
  reversed order, both-ends-meet-in-the-middle exhaustion, the full
  forward-vs-reversed mirror, in-place reversed row rewrites, and the
  zero-width row contract for each iterator. Pure API-completeness
  addition — no behaviour change to existing forward iteration.

### Changed

- `FarbfeldStreamReader::read_all_rows` now decodes each row straight
  into the output `Vec`'s spare (uninitialised) capacity and then bumps
  the length with `set_len`, instead of `resize(.., 0)`-ing the new tail
  to zero before immediately overwriting every slot via
  `decode_be_samples`. The per-row zero-init was redundant work: the BE
  swap writes all `width * 4` new samples (its `row_buf.len() ==
  width * 8` precondition is enforced by the bounded truncation check in
  `read_row_bytes`), so the tail is fully initialised without a
  preceding memset. The DoS bound is unchanged — capacity still grows by
  one delivered row at a time (`reserve` runs only after a bounded row
  read succeeds), so a header announcing a multi-gigabyte body that ships
  no bytes still fails on the first short read having reserved nothing.
  Bit-identical output (no behaviour change). Measured on the 1024×1024
  release-build bench (4 MiB body): `stream_read_all_rows` ~7.3 → ~8.8
  GiB/s (~+20%); 256×256 ~19.0 → ~25.2 GiB/s (~+32%); 64×64 ~13.8 → ~15.5
  GiB/s (~+12%). New unit test
  `read_all_rows_spare_capacity_decode_is_bit_identical_to_whole_file`
  locks the output against `parse_farbfeld` across five shapes for both
  the fresh-reader (`prev_len == 0`) and partial-`read_row`-drain
  (`prev_len != 0`) growth paths.

### Added

- `benches/codec.rs`: ninth Criterion group `stream_skip_row`,
  exercising the row-window decode floor — `FarbfeldStreamReader::skip_row`
  looped over the whole body across the three suite sizes (64×64,
  256×256, 1024×1024). `skip_row` runs the same bounded `Read::take`
  body-consume discipline as `read_row` / `read_row_raw` but performs
  neither the per-sample big-endian → native decode nor the verbatim
  byte copy into a caller slot, so the group is the floor for how fast
  the reader can walk a body it doesn't keep (partial / row-window
  decode: thumbnail row, scan-line inspection, "rows N..M of a
  multi-gigapixel stream"). Baseline on the bench host: ~38.6 GiB/s at
  64×64, ~61.1 GiB/s at 256×256, ~67.8 GiB/s at 1024×1024 — ahead of
  `stream_read_row_raw` (~36 GiB/s at 1024×1024), which additionally
  copies each row out. Pure bench addition — no behaviour change.
- `BENCHMARKS.md`: collects every bench group's description and a
  regression baseline table (bench host + `rustc` version recorded) in
  one place, replacing the per-round figures previously scattered across
  `README.md`.
- `tests/dimension_overflow.rs`: dimension-overflow hardening sweep for
  the header size arithmetic. Drives a **real 16-byte header** carrying
  pathological `u32` dimensions (boundary points plus 4096 PRNG-driven
  pairs biased to the high `u32` band) through `parse_farbfeld_header` /
  `peek_farbfeld_dimensions` / `FarbfeldHeader::total_len`, and
  cross-checks the announced `width * height * 8` body length against an
  independent `u128` oracle. Asserts five invariants: no panic on any
  `u32` pair; body-length exactness vs the oracle when the product fits
  `usize`; overflow reported (never silently wrapped) when it doesn't;
  `total_len()` either equals `16 + body_len` or rejects exactly when
  that sum overflows `usize`; and a header-only file announcing a
  multi-gigabyte body is rejected via the announced-vs-present
  cross-check without allocating the announced body. Pure test addition
  — no behaviour change.
- `FarbfeldImage::pixel(x, y) -> Option<[u16; 4]>` /
  `set_pixel(x, y, [u16; 4]) -> bool` /
  `channel(x, y, c) -> Option<u16>` /
  `row(y) -> Option<&[u16]>` /
  `row_mut(y) -> Option<&mut [u16]>` /
  `pixel_count() -> usize`: spatial accessors that let callers index
  the decoded frame by `(x, y)` (and by single-channel or by whole
  scan-line) without re-implementing the `(y * width + x) * 4`
  row-major arithmetic at every call site. All accessors return
  `Option` and bounds-check against `width` / `height` (and the
  channel index against `4`); the mutating variants return `false` /
  do nothing on out-of-bounds writes so callers that loop over
  `(0..big_w, 0..big_h)` against a smaller frame can't accidentally
  panic. `row` / `row_mut` borrow a contiguous `width * 4`-sample
  slice, which is the shape colour-space conversion helpers and
  per-row downsamplers want. Companion constant
  [`CHANNELS_PER_PIXEL`] (= 4) is exported at the crate root so
  external offset arithmetic can name the channel count instead of
  sprinkling `4`s. Fourteen unit tests cover the in-bounds reads /
  writes, out-of-bounds rejections, zero-width / zero-height edges,
  per-channel reads, single-row overwrite, and a row-major-layout
  consistency check that round-trips every pixel of a 3×2 image
  through `pixel_offset` → `channel`. Pure-data addition: no codec
  / parser / encoder behaviour changes, no new dependencies, both
  feature modes (`registry` and `default-features = false`) build
  and test clean.

- `benches/codec.rs`: two new Criterion groups —
  `stream_read_row_raw` and `stream_write_row_raw` — that exercise the
  raw-bytes pass-through pair (`FarbfeldStreamReader::read_row_raw` /
  `FarbfeldStreamWriter::write_row_raw`) row-by-row across the same
  three image sizes (64×64, 256×256, 1024×1024) as the six existing
  groups. The new groups guard the round-11 perf claim that the raw
  path is faster than its native-endian sibling because it skips the
  per-sample BE swap. At 1024×1024 the raw read path measures
  ~36 GiB/s and the raw write path ~10 GiB/s. Run as
  `cargo bench -p oxideav-farbfeld -- stream_read_row_raw` /
  `stream_write_row_raw` (or alongside the rest of the suite).

- `FarbfeldStreamReader::read_row_raw` and
  `FarbfeldStreamWriter::write_row_raw`: a symmetric pair of raw-bytes
  pass-through methods on the streaming API. `read_row_raw` yields the
  next row's on-disk `width * 8` big-endian body bytes verbatim into a
  caller-provided `&mut [u8]`, skipping the per-sample BE → native
  conversion `read_row` performs; `write_row_raw` accepts an already-BE-
  encoded `width * 8`-byte row and forwards it to the underlying writer
  unchanged. Both methods share the same row-bytes pump, bounded
  `Read::take` / `Write::write_all` discipline, and row-count accounting
  as their native-endian counterparts, so a stream may mix the raw and
  native paths row-by-row in either direction (verified by mid-stream
  mixing tests). The use case is forwarders / proxies / hash-and-
  discard pipelines that need the body bytes but never the native-endian
  sample shape — e.g. reading a farbfeld stream from one source and
  forwarding the body to another consumer without paying the native-
  endian round trip. Ten unit tests cover the new surfaces: read-side
  byte equality against the synthesised reference, mid-stream mixing
  with `read_row` and `skip_row`, zero-width and truncated-body edges,
  write-side byte equality against the reference, extra-row / wrong-
  length / zero-width / mixed-mode rejection, and an end-to-end
  `read_row_raw` → `write_row_raw` byte-identical passthrough on a
  7×5 image.

- `fuzz/fuzz_targets/stream_io.rs`: a third `cargo-fuzz` target that
  drives `FarbfeldStreamReader` / `FarbfeldStreamWriter` through a
  chunked I/O transport (`ChoppyReader` / `ChoppyWriter`) whose `read`
  / `write` calls return short, mid-sized, or full-buffer chunks drawn
  from a deterministic xorshift32 schedule seeded by the fuzz input.
  The existing `decode` / `encode` targets drive their input through a
  bulk `Cursor` / byte slice and never exercise the streaming reader's
  bounded `Read::take` + `read_to_end` discipline or the streaming
  writer's `Write::write_all` discipline under choppy I/O. The new
  target asserts five invariants on every input: no panics under any
  chunk pattern; chunked-stream decode equals bulk
  `parse_farbfeld` decode; chunked-stream encode equals bulk
  `encode_farbfeld_image` encode; the streaming roundtrip closes
  (encode-then-decode reproduces the input samples); and a
  `skip_row`-only walk covers `height` rows without error. A
  body-truncation rejection path is also probed on every non-empty
  body. Image dimensions are capped at 32×32 (4 KiB body) so the
  fuzzer can iterate hundreds of `read` / `write` calls per execution
  inside libFuzzer's per-input budget. Initial coverage burn:
  2 548 637 executions / 61 s clean, cov 382 ft 1396 corp 117 — no
  panics.

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
