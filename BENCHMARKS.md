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

## Baseline

Bench host: Apple M4 Max, `rustc 1.95.0`, release profile, single
sample run (`cargo bench --bench codec`). Throughput is the Criterion
point estimate (mid of the `[lo est hi]` interval); absolute numbers are
host-specific — treat the table as a regression baseline, not a portable
spec. Larger sizes amortise the per-call header/allocation constant cost,
so GiB/s rises with image size.

| Group                  | 64×64        | 256×256      | 1024×1024     |
|------------------------|-------------:|-------------:|--------------:|
| `parse_whole`          | —            | —            | ~39 GiB/s     |
| `encode_raw_be`        | —            | —            | ~78 GiB/s     |
| `encode_from_rgba16`   | —            | —            | ~47 GiB/s     |
| `encode_image`         | —            | —            | ~46 GiB/s     |
| `stream_read_all_rows` | ~15.5 GiB/s  | ~25.2 GiB/s  | ~8.8 GiB/s    |
| `stream_write_all_rows`| —            | —            | ~10 GiB/s     |
| `stream_read_row_raw`  | —            | —            | ~36 GiB/s     |
| `stream_write_row_raw` | —            | —            | ~10 GiB/s     |
| `stream_skip_row`      | ~38.6 GiB/s  | ~61.1 GiB/s  | ~67.8 GiB/s   |

A dash means the figure was not separately recorded for that size on the
baseline run; rerun the named group to fill it in. The whole-file and
`encode_*` headline figures are carried over from the round-9 hot-path
rewrite recorded in `README.md`.

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
