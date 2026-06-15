# oxideav-farbfeld benchmarks

Criterion micro-benchmarks for the farbfeld codec live in
`benches/codec.rs`. Run the whole suite with:

```text
cargo bench -p oxideav-farbfeld
```

or scope to one group, e.g.

```text
cargo bench -p oxideav-farbfeld -- stream_skip_row
```

Every group reports `Throughput::Bytes(body_len)` so the GiB/s figure is
directly comparable across the three image sizes — `64×64` (16 KiB body),
`256×256` (256 KiB body), and `1024×1024` (4 MiB body).

## Bench groups

| Group                  | What it measures |
|------------------------|------------------|
| `parse_whole`          | `parse_farbfeld` on a pre-encoded buffer (whole-file decode) |
| `encode_raw_be`        | `encode_farbfeld` (pre-serialised BE body — memcpy-bound) |
| `encode_from_rgba16`   | `encode_farbfeld_from_rgba16` (native-endian RGBA, per-channel BE swap on emit) |
| `encode_image`         | `encode_farbfeld_image` (flat `[u16]` plane via `FarbfeldImage`) |
| `stream_read_all_rows` | `FarbfeldStreamReader::read_all_rows` against a `Cursor`-backed body |
| `stream_write_all_rows`| `FarbfeldStreamWriter::write_row` looped to completion |
| `stream_read_row_raw`  | `FarbfeldStreamReader::read_row_raw` row-by-row (no per-sample BE swap; verbatim byte copy) |
| `stream_write_row_raw` | `FarbfeldStreamWriter::write_row_raw` row-by-row (already-BE body forwarded verbatim) |
| `stream_skip_row`      | `FarbfeldStreamReader::skip_row` row-by-row — the row-window decode floor (consumes body bytes without decoding to samples or copying them out) |
| `stream_skip_rows_bulk`| `FarbfeldStreamReader::skip_rows(height)` — skip the whole body in one bulk call (the height-bounded loop folded inside `skip_rows`, vs the per-row `skip_row` group's one call per row) |
| `peek_header`          | `peek_farbfeld_dimensions` on the 16-byte header prefix — the body-independent sandbox pre-flight (magic + two BE `u32` dims + `total_len()` overflow gate); per-call constant, never touches the pixel array |

## Baseline

Bench host: Apple M4 Max, `rustc 1.95.0`, release profile (`cargo bench
--bench codec`). Throughput is the Criterion point estimate (mid of the
`[lo est hi]` interval); absolute numbers are host-specific — treat the
table as a regression baseline, not a portable spec. Larger sizes
amortise the per-call header/allocation constant cost, so GiB/s generally
rises with image size (the streaming whole-buffer groups are the
exception — see the note below the table).

| Group                  | 64×64        | 256×256      | 1024×1024     |
|------------------------|-------------:|-------------:|--------------:|
| `parse_whole`          | ~30.6 GiB/s  | ~35.6 GiB/s  | ~38.5 GiB/s   |
| `encode_raw_be`        | ~24.4 GiB/s  | ~50.3 GiB/s  | ~55.8 GiB/s   |
| `encode_from_rgba16`   | ~20.5 GiB/s  | ~47.4 GiB/s  | ~42.8 GiB/s   |
| `encode_image`         | ~18.2 GiB/s  | ~48.9 GiB/s  | ~50.1 GiB/s   |
| `stream_read_all_rows` | ~15.2 GiB/s  | ~25.6 GiB/s  | ~8.2 GiB/s    |
| `stream_write_all_rows`| ~12.2 GiB/s  | ~27.3 GiB/s  | ~16.2 GiB/s   |
| `stream_read_row_raw`  | ~25.5 GiB/s  | ~35.9 GiB/s  | ~34.6 GiB/s   |
| `stream_write_row_raw` | ~15.7 GiB/s  | ~35.2 GiB/s  | ~18.3 GiB/s   |
| `stream_skip_row`      | ~35.4 GiB/s  | ~55.5 GiB/s  | ~62.7 GiB/s   |
| `stream_skip_rows_bulk`| ~38.0 GiB/s  | ~57.1 GiB/s  | ~63.1 GiB/s   |
| `peek_header`          | ~1.59 ns     | ~1.58 ns     | ~1.60 ns      |

The whole table was re-measured in a single run; every cell is a fresh
point estimate (no carry-over figures, no dashes). `parse_whole` /
`encode_*` headline GiB/s replaces the earlier round-9 carry-over numbers.
`stream_read_all_rows` dips at 1024×1024 because that group grows the
output `Vec` across the full 4 MiB body and the realloc/copy traffic
dominates once the body no longer fits a warm cache; the row-at-a-time
`stream_read_row_raw` and `stream_skip_row` groups, which reuse one row
buffer, keep climbing instead.

### Reading the `peek_header` row

`peek_header` is the one group whose figure is a **per-call latency**, not
a body-scaled rate — it reads only the 16-byte header (magic + two
big-endian `u32` dimensions) and runs the `total_len()` overflow check
that a sandbox uses to refuse an over-large image *before* allocating its
body. Because no pixel byte is ever touched, the cost is constant: the
baseline holds flat at ~1.6 ns across all three announced sizes even
though the 64×64 and 1024×1024 headers describe bodies three orders of
magnitude apart. The `GiB/s` figure Criterion prints for this group (it
divides the fixed 16-byte header length by the time) is therefore an
artefact and is ignored here in favour of the ~1.6 ns wall-clock latency.

### Reading the `stream_skip_rows_bulk` row

`skip_rows(height)` folds the height-bounded skip loop inside one public
call, where `stream_skip_row` pays a separate `skip_row` call (plus its
bounds + accounting) per row. The two run against the same body and the
same `Read::take` body-consume discipline, so the bulk group is the
direct "drop the rest of this frame in one call" counterpart to the
per-row floor. The single-call amortisation shows up most at the small
size (~38.0 vs ~35.4 GiB/s at 64×64) and narrows as the per-row call
overhead is dwarfed by the body walk on the larger sizes (~63.1 vs ~62.7
GiB/s at 1024×1024).

### Reading the `stream_skip_row` row

`skip_row` runs the same bounded `Read::take` body-consume discipline as
`read_row` / `read_row_raw` but performs neither the per-sample
big-endian → native decode nor the verbatim byte copy into a caller
slot. It is therefore the fastest way to walk a body the caller does not
keep — the floor for partial / row-window decode (thumbnail row,
scan-line inspection, "rows N..M of a multi-gigapixel stream"). The
baseline shows it climbing from ~38.6 GiB/s at 64×64 to ~67.8 GiB/s at
1024×1024 as the fixed per-call header parse is amortised over a larger
body, comfortably ahead of `stream_read_row_raw` (~36 GiB/s at
1024×1024), which additionally copies each row's bytes into the caller
slot.
