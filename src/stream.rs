//! Streaming farbfeld reader and writer — row-at-a-time, no
//! whole-image buffer required.
//!
//! [`parse_farbfeld`](crate::parse_farbfeld) loads an entire farbfeld
//! file into a [`FarbfeldImage`](crate::FarbfeldImage). That's the
//! shape framework callers (and small fixtures) want. For larger
//! images the round-major layout makes a row-at-a-time API trivial,
//! and avoids holding `width * height * 8` bytes of pixel buffer at
//! once.
//!
//! Spec recap: after the 16-byte header, the file body is exactly
//! `width * height` pixels of 4×u16 BE = R, G, B, A, in row-major
//! scan order. Each row is `width * 8` bytes on disk. [`FarbfeldStreamReader`]
//! parses the header once and then yields one row of native-endian
//! u16 samples per call to [`FarbfeldStreamReader::read_row`].
//! [`FarbfeldStreamWriter`] mirrors the same shape on the encode side.
//!
//! Both types operate on `std::io::Read` / `std::io::Write`, not byte
//! slices, so the streaming reader can drive a 16k×16k farbfeld off a
//! mmap or socket without ever holding the body in memory.

use std::io::{self, Read, Write};

use crate::encoder::encode_be_samples;
use crate::error::{FarbfeldError, Result};
use crate::parser::{
    decode_be_samples, parse_farbfeld_header, FarbfeldHeader, BYTES_PER_PIXEL, HEADER_LEN, MAGIC,
};

/// Row-at-a-time farbfeld reader.
///
/// Reads the 16-byte header from the underlying `R` at construction
/// time; the rest of the body is consumed lazily by
/// [`read_row`](Self::read_row), one row per call.
pub struct FarbfeldStreamReader<R: Read> {
    inner: R,
    header: FarbfeldHeader,
    rows_read: u32,
    /// On-disk bytes per row (`width * 8`), validated at construction to
    /// fit in `usize`. The scratch buffer that holds one row is grown
    /// lazily inside [`read_row`](Self::read_row) so a header that
    /// announces a multi-gigabyte row width can't force the allocation
    /// before any body bytes are present (see the DoS-hardening note on
    /// [`read_row`](Self::read_row)).
    row_bytes: usize,
    /// Reusable per-row scratch buffer, grown only as far as the bytes
    /// the reader actually delivers.
    row_buf: Vec<u8>,
}

impl<R: Read> FarbfeldStreamReader<R> {
    /// Construct a streaming reader by consuming the 16-byte header
    /// from `inner`.
    ///
    /// Returns [`FarbfeldError::InvalidData`] if the header is short,
    /// the magic is wrong, or `width * height * 8` overflows `usize`.
    pub fn new(mut inner: R) -> Result<Self> {
        let mut header_buf = [0u8; HEADER_LEN];
        read_exact_to_invalid(&mut inner, &mut header_buf, "farbfeld: header")?;
        let header = parse_farbfeld_header(&header_buf)?;
        // Validate `width * 8` fits in `usize` but do NOT allocate it yet:
        // the per-row scratch buffer is grown lazily on the first
        // `read_row`, capped by the bytes the reader actually supplies.
        let row_bytes = (header.width as usize)
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or_else(|| FarbfeldError::invalid("farbfeld: row size overflows usize"))?;
        Ok(Self {
            inner,
            header,
            rows_read: 0,
            row_bytes,
            row_buf: Vec::new(),
        })
    }

    /// The decoded header: `width`, `height`, and the announced body
    /// byte count.
    pub fn header(&self) -> &FarbfeldHeader {
        &self.header
    }

    /// Picture width in pixels.
    pub fn width(&self) -> u32 {
        self.header.width
    }

    /// Picture height in pixels.
    pub fn height(&self) -> u32 {
        self.header.height
    }

    /// Number of rows already returned by [`read_row`](Self::read_row).
    pub fn rows_read(&self) -> u32 {
        self.rows_read
    }

    /// Number of rows still pending in the body.
    pub fn rows_remaining(&self) -> u32 {
        self.header.height.saturating_sub(self.rows_read)
    }

    /// Skip the next body row without decoding its samples.
    ///
    /// Consumes exactly `width * 8` bytes from the underlying reader
    /// (the bytes a single row occupies on disk) and advances
    /// [`rows_read`](Self::rows_read) by one. The row data is
    /// discarded — no `u16` decoding happens. Symmetric to
    /// [`read_row`](Self::read_row) for callers that only want a
    /// row-window from a much larger image (thumbnail row, scan-line
    /// inspection, partial decode) and don't want to pay the
    /// per-sample big-endian conversion cost for rows they'll discard.
    ///
    /// Returns:
    /// * `Ok(true)` if a row's worth of bytes was skipped.
    /// * `Ok(false)` if all `height` rows have already been consumed.
    /// * [`FarbfeldError::InvalidData`] if the underlying reader yields
    ///   fewer than `width * 8` bytes for the row (truncated file).
    ///
    /// The skip uses the same length-bounded [`Read::take`] discipline
    /// as [`read_row`](Self::read_row), so a malicious header announcing
    /// a multi-gigabyte row width but shipping no body still surfaces
    /// as a truncation error without forcing the announced-width
    /// allocation.
    pub fn skip_row(&mut self) -> Result<bool> {
        if self.rows_read >= self.header.height {
            return Ok(false);
        }
        // `read_row_bytes` runs the same bounded `Read::take` /
        // `read_to_end` discipline used by `read_row`; the row bytes
        // are then simply not converted to `u16` samples.
        let _ = self.read_row_bytes()?;
        self.rows_read += 1;
        Ok(true)
    }

    /// Skip the next `n` rows of the body, or as many rows as remain if
    /// `n` exceeds [`rows_remaining`](Self::rows_remaining).
    ///
    /// Returns the number of rows actually skipped (capped at
    /// `rows_remaining` before this call). A return value smaller than
    /// `n` is therefore not an error — it's the normal "skipped past
    /// the end" outcome and mirrors the `Ok(false)` shape of
    /// [`skip_row`](Self::skip_row).
    ///
    /// Each skipped row still consumes `width * 8` bytes from the
    /// underlying reader, so the same truncation contract as
    /// [`skip_row`](Self::skip_row) applies: a short read on any of the
    /// skipped rows surfaces as [`FarbfeldError::InvalidData`].
    pub fn skip_rows(&mut self, n: u32) -> Result<u32> {
        let want = n.min(self.rows_remaining());
        let mut done = 0u32;
        while done < want {
            // `skip_row` cannot return `Ok(false)` here because we've
            // capped `want` at `rows_remaining` up front, but propagate
            // any truncation error as-is.
            if !self.skip_row()? {
                break;
            }
            done += 1;
        }
        Ok(done)
    }

    /// Read the next row's on-disk bytes directly into `out`, without
    /// the per-sample big-endian → native-endian conversion that
    /// [`read_row`](Self::read_row) performs.
    ///
    /// `out` must be exactly `width * 8` bytes long: each pixel
    /// contributes eight bytes (four 16-bit big-endian channels). The
    /// row is delivered verbatim — the on-disk byte order is preserved.
    ///
    /// This is the symmetric counterpart to [`write_row_raw`] on the
    /// writer side, and the pass-through-friendly counterpart to
    /// [`read_row`](Self::read_row). It exists for callers that don't
    /// need the native-endian sample shape and would otherwise have to
    /// re-encode it back to big-endian — proxies forwarding a farbfeld
    /// stream onto another consumer, hash-and-discard pipelines that
    /// only need the bytes, and consumers writing the body verbatim
    /// into another 16-bit-big-endian sample container.
    ///
    /// Returns:
    /// * `Ok(true)` if a row was read.
    /// * `Ok(false)` if all `height` rows have already been returned.
    /// * [`FarbfeldError::InvalidData`] if `out` is the wrong length,
    ///   or if the underlying reader yields fewer than `width * 8`
    ///   bytes (truncated file).
    ///
    /// The same DoS-hardening discipline as [`read_row`](Self::read_row)
    /// applies: the row body is read with a length-bounded [`Read::take`],
    /// so a header announcing a multi-gigabyte row width but shipping no
    /// body surfaces as a truncation error without forcing the
    /// announced-width allocation.
    pub fn read_row_raw(&mut self, out: &mut [u8]) -> Result<bool> {
        if self.rows_read >= self.header.height {
            return Ok(false);
        }
        if out.len() != self.row_bytes {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld stream: row out-slice has {} bytes, need {} ({} × {BYTES_PER_PIXEL})",
                out.len(),
                self.row_bytes,
                self.header.width,
            )));
        }
        if self.read_row_bytes()? {
            // `read_row_bytes` returns `true` only when `row_bytes > 0`
            // and exactly that many bytes were read into `self.row_buf`.
            out.copy_from_slice(&self.row_buf);
        }
        self.rows_read += 1;
        Ok(true)
    }

    /// Read the next row of the body into `out`.
    ///
    /// `out` must be exactly `width * 4` `u16` slots long: each pixel
    /// contributes four samples in `R, G, B, A` order. The slice is
    /// overwritten in place with native-endian values (the big-endian
    /// on-disk encoding is decoded as the bytes are read).
    ///
    /// Returns:
    /// * `Ok(true)` if a row was read.
    /// * `Ok(false)` if all `height` rows have already been returned
    ///   (i.e. EOF on the well-formed body).
    /// * [`FarbfeldError::InvalidData`] if `out` is the wrong length,
    ///   or if the underlying reader yields fewer than `width * 8`
    ///   bytes (truncated file).
    ///
    /// # DoS hardening
    ///
    /// The row body is read with a length-bounded [`Read::take`] into a
    /// buffer grown only as far as the bytes the reader actually
    /// delivers. A header announcing a multi-gigabyte row width
    /// (`width = 0x2c000000`, ~5.9 GB per row) but shipping no body must
    /// not be able to force the announced-width allocation: the bounded
    /// read tops out at the bytes present, and a short read surfaces as a
    /// truncation error having allocated only what arrived.
    pub fn read_row(&mut self, out: &mut [u16]) -> Result<bool> {
        if self.rows_read >= self.header.height {
            return Ok(false);
        }
        let want_samples = (self.header.width as usize) * 4;
        if out.len() != want_samples {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld stream: row out-slice has {} samples, need {want_samples} ({} × 4)",
                out.len(),
                self.header.width,
            )));
        }
        // Zero-width images carry no body — still count the row.
        if self.read_row_bytes()? {
            // Vectorisable shared helper — `chunks_exact(2)` zipped with
            // a `&mut [u16]` of matching length lets the auto-vectoriser
            // turn the per-sample byte swap into a SIMD bswap.
            decode_be_samples(&self.row_buf, out);
        }
        self.rows_read += 1;
        Ok(true)
    }

    /// Read the next row's `width * 8` on-disk bytes into `self.row_buf`
    /// with a length-bounded read, advancing `rows_read` accounting in
    /// the caller. Returns `Ok(true)` when body bytes were read (i.e.
    /// `width > 0`), `Ok(false)` for the zero-width no-body case.
    ///
    /// The read uses [`Read::take`] capped at `row_bytes` and
    /// `read_to_end`, so `self.row_buf` only grows as far as the bytes
    /// the reader actually delivers — a header announcing a multi-
    /// gigabyte row width but shipping no body fails as a truncation
    /// error having allocated only what arrived, never the full
    /// announced `width * 8`.
    fn read_row_bytes(&mut self) -> Result<bool> {
        if self.row_bytes == 0 {
            return Ok(false);
        }
        self.row_buf.clear();
        let read = (&mut self.inner)
            .take(self.row_bytes as u64)
            .read_to_end(&mut self.row_buf)
            .map_err(|e| {
                FarbfeldError::invalid(format!("farbfeld stream: row body: io error: {e}"))
            })?;
        if read != self.row_bytes {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld stream: row body: truncated input ({} bytes wanted, {read} delivered)",
                self.row_bytes,
            )));
        }
        Ok(true)
    }

    /// Convenience: drain the remaining body, accumulating every row
    /// into a flat row-major `Vec<u16>` of length `width * height * 4`.
    ///
    /// # DoS hardening
    ///
    /// The output buffer is grown **one row at a time** and each row's
    /// on-disk bytes are read with a length-bounded read, so peak
    /// allocation tracks the body bytes the reader actually delivers —
    /// never the header's *announced* `width * height * 4` sample count.
    /// The reader works on an arbitrary [`Read`] and can't know the true
    /// remaining input length up front, so neither a 16-byte file
    /// announcing `width = height = 0x10000` (~34 GB of samples) nor a
    /// 21-byte file announcing a `width = 0x29000000` row (~5.5 GB per
    /// row) may force a giant allocation: both fail as a truncation
    /// error on the first short row read, having allocated only the few
    /// body bytes that were genuinely present.
    pub fn read_all_rows(&mut self) -> Result<Vec<u16>> {
        // Validate the announced sample count fits in `usize` so callers
        // get the explicit overflow error rather than a panic, but do
        // NOT allocate it eagerly. It also caps the confirmed-data-bounded
        // doubling below so a reserve never overshoots the announced body.
        let total_samples = (self.header.width as usize)
            .checked_mul(self.header.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| FarbfeldError::invalid("farbfeld stream: total samples overflow"))?;
        let row_samples = (self.header.width as usize) * 4;
        // Zero-width images carry no body whatever the height, so there
        // is nothing to read. Short-circuit instead of looping `height`
        // (up to 2^32) times doing no work — a 16-byte file announcing
        // `width = 0, height = u32::MAX` would otherwise spin billions
        // of empty iterations (a CPU-time DoS).
        if row_samples == 0 {
            self.rows_read = self.header.height;
            return Ok(Vec::new());
        }
        // Grow `out` row-by-row, decoding directly from the bounded
        // `row_buf` read. Honour any rows already consumed via `read_row`
        // by draining from `rows_read` to `height`.
        //
        // Each row decodes the freshly-read body straight into the
        // `Vec`'s spare (uninitialised) capacity through the shared
        // SIMD-friendly `decode_be_samples` helper, then bumps the length
        // with `set_len`. This skips the per-row zero-init that a
        // `resize(.., 0)` would do: `decode_be_samples` writes every one
        // of the `row_samples` new slots (its `body.len() == row_bytes ==
        // row_samples * 2` contract is enforced by the truncation check
        // in `read_row_bytes`), so the tail is fully initialised by the
        // BE swap with no preceding memset.
        //
        // ## Allocation strategy: bounded one-shot, else incremental
        //
        // The previous implementation grew `out` one row at a time. That
        // leaves `Vec`'s internal amortised doubling to fire at sizes it
        // picks, and **each doubling memcpy's the entire accumulated
        // buffer**. On the 4 MiB 1024×1024 body that repeated whole-buffer
        // copy traffic dominated once the buffer outgrew the warm cache,
        // showing up as the documented `stream_read_all_rows` throughput
        // dip at the large size.
        //
        // When the announced body is within a fixed cap
        // (`ONE_SHOT_SAMPLE_CAP`), reserve it **once** up front: a single
        // allocation, zero reallocs, zero whole-buffer copies. Each row
        // then decodes straight into the pre-sized spare capacity.
        //
        // The cap preserves the DoS bound. A 16-byte file announcing a
        // multi-gigabyte body has `total_samples` far above the cap, so it
        // takes the incremental row-by-row path and still fails on the
        // first short read having reserved only a single row — never the
        // announced giant. The cap is sized so the worst-case eager
        // reservation a malicious-but-under-cap header can trigger stays a
        // bounded, fast-failing allocation, not an OOM.
        const ONE_SHOT_SAMPLE_CAP: usize = 64 * 1024 * 1024; // 64 Mi samples = 128 MiB
        let one_shot = total_samples <= ONE_SHOT_SAMPLE_CAP;
        let mut out: Vec<u16> = if one_shot {
            Vec::with_capacity(total_samples)
        } else {
            Vec::new()
        };
        while self.rows_read < self.header.height {
            // `read_row_bytes` returns false only for the zero-width
            // (no body) case; the row is still counted toward `height`.
            if self.read_row_bytes()? {
                let prev_len = out.len();
                // One-shot path already reserved the whole body; the
                // incremental path reserves a single row's worth here.
                if out.capacity() - prev_len < row_samples {
                    out.reserve(row_samples);
                }
                // SAFETY: the grow block above guarantees `capacity -
                // prev_len >= row_samples` (it only skips the `reserve`
                // when that already holds), so the `&mut [u16]` view over
                // `spare_capacity_mut()[..row_samples]` reborrowed as
                // initialised `u16` is in bounds. `decode_be_samples`
                // fills exactly those `row_samples` slots (`row_buf.len()
                // == row_bytes == row_samples * 2` after a successful
                // bounded read), so every slot is initialised before
                // `set_len` exposes it.
                // `u16` has no invalid bit patterns, so even a
                // contract-violating partial fill could not produce an
                // invalid value — but the truncation check above rules
                // that out regardless.
                let spare = out.spare_capacity_mut();
                let tail = unsafe {
                    core::slice::from_raw_parts_mut(spare.as_mut_ptr() as *mut u16, row_samples)
                };
                decode_be_samples(&self.row_buf, tail);
                // SAFETY: the `row_samples` newly-written slots starting
                // at `prev_len` are now initialised by `decode_be_samples`
                // above; growing the length by exactly that count exposes
                // only initialised memory.
                unsafe {
                    out.set_len(prev_len + row_samples);
                }
            }
            self.rows_read += 1;
        }
        Ok(out)
    }

    /// Surrender the wrapped reader. The caller is responsible for any
    /// trailing bytes after the body (the spec disallows them; callers
    /// who care should check `inner` for EOF).
    pub fn into_inner(self) -> R {
        self.inner
    }
}

/// Row-at-a-time farbfeld writer.
///
/// Writes the 16-byte header to the underlying `W` at construction
/// time, then accepts `height` rows of `[u16; 4]`-encoded RGBA pixels
/// via [`write_row`](Self::write_row). [`finish`](Self::finish) checks
/// the expected row count was honoured and returns the wrapped writer.
pub struct FarbfeldStreamWriter<W: Write> {
    inner: W,
    width: u32,
    height: u32,
    rows_written: u32,
    /// Reusable per-row scratch buffer (`width * 8` bytes of on-disk
    /// big-endian samples).
    row_buf: Vec<u8>,
}

impl<W: Write> FarbfeldStreamWriter<W> {
    /// Construct a streaming writer, emitting the 16-byte header.
    ///
    /// Returns [`FarbfeldError::InvalidData`] if `width * height * 8`
    /// overflows `usize`, or the underlying writer rejects the header
    /// bytes.
    pub fn new(mut inner: W, width: u32, height: u32) -> Result<Self> {
        let row_bytes = (width as usize)
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or_else(|| FarbfeldError::invalid("farbfeld: row size overflows usize"))?;
        // Cross-check the full body fits in `usize` too — keeps the
        // stream-writer's row-count loop honest on 32-bit hosts with
        // pathological dimensions.
        let _ = row_bytes
            .checked_mul(height as usize)
            .ok_or_else(|| FarbfeldError::invalid("farbfeld: body size overflows usize"))?;
        write_all_to_invalid(&mut inner, MAGIC, "farbfeld stream: magic")?;
        write_all_to_invalid(&mut inner, &width.to_be_bytes(), "farbfeld stream: width")?;
        write_all_to_invalid(&mut inner, &height.to_be_bytes(), "farbfeld stream: height")?;
        Ok(Self {
            inner,
            width,
            height,
            rows_written: 0,
            row_buf: vec![0u8; row_bytes],
        })
    }

    /// Picture width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Picture height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Number of rows already emitted via [`write_row`](Self::write_row).
    pub fn rows_written(&self) -> u32 {
        self.rows_written
    }

    /// Append one row of native-endian RGBA u16 pixels to the body.
    ///
    /// `row` must be exactly `width * 4` samples. Each sample is
    /// converted to big-endian on the way to the underlying writer.
    ///
    /// Returns [`FarbfeldError::InvalidData`] if the row count would
    /// exceed `height`, or if `row.len()` is wrong, or if the
    /// underlying writer fails.
    pub fn write_row(&mut self, row: &[u16]) -> Result<()> {
        if self.rows_written >= self.height {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld stream: write_row called after all {} rows already written",
                self.height,
            )));
        }
        let want = (self.width as usize) * 4;
        if row.len() != want {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld stream: row in-slice has {} samples, need {want} ({} × 4)",
                row.len(),
                self.width,
            )));
        }
        if !self.row_buf.is_empty() {
            // Vectorisable shared helper — `chunks_exact_mut(2)` zipped
            // with the input `row` lets the auto-vectoriser turn the
            // per-sample BE store into a SIMD bswap.
            encode_be_samples(row, &mut self.row_buf);
            write_all_to_invalid(&mut self.inner, &self.row_buf, "farbfeld stream: row body")?;
        }
        self.rows_written += 1;
        Ok(())
    }

    /// Append one row of pre-serialised big-endian RGBA u16 bytes to
    /// the body, without the per-sample native-endian → big-endian
    /// conversion that [`write_row`](Self::write_row) performs.
    ///
    /// `row` must be exactly `width * 8` bytes — four big-endian 16-bit
    /// channels per pixel, already laid out in on-disk order. The bytes
    /// are forwarded verbatim to the underlying writer.
    ///
    /// This is the symmetric counterpart to
    /// [`FarbfeldStreamReader::read_row_raw`]: a proxy or pipeline that
    /// reads bytes from one farbfeld stream and forwards them onto
    /// another (or to a hash/store/container that consumes the same
    /// 16-bit BE layout) can chain the two together and skip the
    /// round-trip through native-endian `u16` samples entirely.
    ///
    /// Returns [`FarbfeldError::InvalidData`] if the row count would
    /// exceed `height`, or if `row.len()` is wrong, or if the
    /// underlying writer fails.
    pub fn write_row_raw(&mut self, row: &[u8]) -> Result<()> {
        if self.rows_written >= self.height {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld stream: write_row_raw called after all {} rows already written",
                self.height,
            )));
        }
        let want = self.row_buf.len();
        if row.len() != want {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld stream: row in-slice has {} bytes, need {want} ({} × {BYTES_PER_PIXEL})",
                row.len(),
                self.width,
            )));
        }
        if !row.is_empty() {
            // Bytes are already in on-disk big-endian layout; forward
            // verbatim. `write_all_to_invalid` runs the same
            // `Write::write_all` loop as `write_row` so short writes
            // compose correctly under a non-bulk transport.
            write_all_to_invalid(&mut self.inner, row, "farbfeld stream: row body")?;
        }
        self.rows_written += 1;
        Ok(())
    }

    /// Confirm exactly `height` rows were written and surrender the
    /// underlying writer.
    ///
    /// Returns [`FarbfeldError::InvalidData`] if fewer than `height`
    /// rows were emitted — a partial file is not a valid farbfeld.
    pub fn finish(self) -> Result<W> {
        if self.rows_written != self.height {
            return Err(FarbfeldError::invalid(format!(
                "farbfeld stream: finish called with {} of {} rows written",
                self.rows_written, self.height,
            )));
        }
        Ok(self.inner)
    }
}

fn read_exact_to_invalid(r: &mut impl Read, buf: &mut [u8], label: &str) -> Result<()> {
    match r.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(FarbfeldError::invalid(format!(
            "{label}: truncated input ({} bytes wanted)",
            buf.len()
        ))),
        Err(e) => Err(FarbfeldError::invalid(format!("{label}: io error: {e}"))),
    }
}

fn write_all_to_invalid(w: &mut impl Write, buf: &[u8], label: &str) -> Result<()> {
    w.write_all(buf)
        .map_err(|e| FarbfeldError::invalid(format!("{label}: io error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn synth(width: u32, height: u32, samples: &[u16]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&width.to_be_bytes());
        buf.extend_from_slice(&height.to_be_bytes());
        for &s in samples {
            buf.extend_from_slice(&s.to_be_bytes());
        }
        buf
    }

    #[test]
    fn streaming_reader_yields_each_row_native_endian() {
        // 3×2 image — pixel (x, y) has R = y*100 + x*10, G/B/A from 1..4.
        let mut samples = Vec::new();
        for y in 0..2u32 {
            for x in 0..3u32 {
                samples.push((y * 100 + x * 10) as u16);
                samples.push(1);
                samples.push(2);
                samples.push(3);
            }
        }
        let bytes = synth(3, 2, &samples);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.width(), 3);
        assert_eq!(reader.height(), 2);
        assert_eq!(reader.rows_remaining(), 2);

        let mut row = vec![0u16; 3 * 4];
        assert!(reader.read_row(&mut row).unwrap());
        assert_eq!(row, &samples[..12]);
        assert!(reader.read_row(&mut row).unwrap());
        assert_eq!(row, &samples[12..]);
        // Third call past height = no row.
        assert!(!reader.read_row(&mut row).unwrap());
        assert_eq!(reader.rows_read(), 2);
        assert_eq!(reader.rows_remaining(), 0);
    }

    #[test]
    fn streaming_reader_rejects_truncated_body() {
        // Announce 2×2 (= 64 body bytes) but ship only 16.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 16]); // half of one row
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        let mut row = vec![0u16; 8];
        // First row needs 16 BE bytes — we have 16, that succeeds.
        assert!(reader.read_row(&mut row).is_ok());
        // Second row needs 16 more — none left. Truncated.
        let err = reader.read_row(&mut row).unwrap_err();
        let FarbfeldError::InvalidData(s) = err;
        assert!(s.contains("truncated"), "msg = {s:?}");
    }

    #[test]
    fn streaming_reader_rejects_bad_magic_at_construction() {
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes[..8].copy_from_slice(b"OXIDEAVF");
        assert!(FarbfeldStreamReader::new(Cursor::new(bytes)).is_err());
    }

    #[test]
    fn streaming_reader_rejects_wrong_row_length() {
        let bytes = synth(2, 1, &[0, 0, 0, 0, 0, 0, 0, 0]);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        // Caller passes 4 samples — width = 2 needs 8 samples.
        let mut row = vec![0u16; 4];
        assert!(reader.read_row(&mut row).is_err());
    }

    #[test]
    fn streaming_reader_handles_zero_dimension() {
        // 0×0 — no body, two read_row calls both return false.
        let bytes = synth(0, 0, &[]);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        let mut row: Vec<u16> = Vec::new();
        assert!(!reader.read_row(&mut row).unwrap());
        // 5×0 — height zero so still no rows expected.
        let bytes = synth(5, 0, &[]);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        let mut row = vec![0u16; 20];
        assert!(!reader.read_row(&mut row).unwrap());
        // 0×4 — width zero, height four. read_row called four times.
        let bytes = synth(0, 4, &[]);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        let mut row: Vec<u16> = Vec::new();
        for _ in 0..4 {
            assert!(reader.read_row(&mut row).unwrap());
        }
        assert!(!reader.read_row(&mut row).unwrap());
    }

    #[test]
    fn streaming_writer_emits_byte_exact_against_synthesised_reference() {
        // 2×3 image, deterministic samples.
        let mut samples = Vec::new();
        for y in 0..3u32 {
            for x in 0..2u32 {
                let base = (y * 2 + x) as u16;
                samples.push(base.wrapping_mul(0x0123));
                samples.push(base.wrapping_mul(0x4567));
                samples.push(base.wrapping_mul(0x89AB));
                samples.push(base.wrapping_mul(0xCDEF));
            }
        }
        let reference = synth(2, 3, &samples);

        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 2, 3).unwrap();
        for row_idx in 0..3 {
            let off = row_idx * 2 * 4;
            writer.write_row(&samples[off..off + 2 * 4]).unwrap();
        }
        let bytes = writer.finish().unwrap();
        assert_eq!(bytes, reference);
    }

    #[test]
    fn streaming_writer_finish_rejects_missing_rows() {
        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 1, 3).unwrap();
        let row = [0u16; 4];
        writer.write_row(&row).unwrap();
        // Only 1 of 3 rows — finish must refuse.
        let err = writer.finish().unwrap_err();
        let FarbfeldError::InvalidData(s) = err;
        assert!(s.contains("1 of 3"), "msg = {s:?}");
    }

    #[test]
    fn streaming_writer_rejects_extra_row() {
        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 1, 2).unwrap();
        let row = [0u16; 4];
        writer.write_row(&row).unwrap();
        writer.write_row(&row).unwrap();
        // Third call past height = error.
        assert!(writer.write_row(&row).is_err());
    }

    #[test]
    fn streaming_writer_rejects_wrong_row_length() {
        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 3, 1).unwrap();
        let too_short = [0u16; 8]; // need 12.
        assert!(writer.write_row(&too_short).is_err());
    }

    #[test]
    fn skip_row_advances_rows_read_without_decoding() {
        // 3×4 image — each row is `width*8 = 24` body bytes. We skip
        // the first two rows then read the third and confirm the read
        // returns exactly the third row's samples.
        let mut samples = Vec::new();
        for y in 0..4u32 {
            for x in 0..3u32 {
                let v = (y * 100 + x * 10) as u16;
                samples.push(v);
                samples.push(v.wrapping_add(1));
                samples.push(v.wrapping_add(2));
                samples.push(v.wrapping_add(3));
            }
        }
        let bytes = synth(3, 4, &samples);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();

        // Skip row 0 and row 1.
        assert!(reader.skip_row().unwrap());
        assert_eq!(reader.rows_read(), 1);
        assert_eq!(reader.rows_remaining(), 3);
        assert!(reader.skip_row().unwrap());
        assert_eq!(reader.rows_read(), 2);

        // Now decode row 2 — must match samples[24..36] (12 samples).
        let mut row = vec![0u16; 12];
        assert!(reader.read_row(&mut row).unwrap());
        assert_eq!(row, &samples[24..36]);

        // Decode row 3 — last row.
        assert!(reader.read_row(&mut row).unwrap());
        assert_eq!(row, &samples[36..48]);

        // Past end — both skip_row and read_row return Ok(false).
        assert!(!reader.skip_row().unwrap());
        assert!(!reader.read_row(&mut row).unwrap());
    }

    #[test]
    fn skip_row_handles_zero_width() {
        // 0×3 — three zero-byte rows; skip_row must still count them.
        let bytes = synth(0, 3, &[]);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        for i in 0..3u32 {
            assert!(reader.skip_row().unwrap());
            assert_eq!(reader.rows_read(), i + 1);
        }
        assert!(!reader.skip_row().unwrap());
    }

    #[test]
    fn skip_row_propagates_truncated_body() {
        // 2×2 (= 32 body bytes) but only 8 bytes of body present.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 8]); // half of one row
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        // First row needs 16 bytes; only 8 present — truncation.
        let err = reader.skip_row().unwrap_err();
        let FarbfeldError::InvalidData(s) = err;
        assert!(s.contains("truncated"), "msg = {s:?}");
    }

    #[test]
    fn skip_rows_caps_at_remaining() {
        // 1×3 image — ask to skip 100 rows; expect to skip 3.
        let samples = vec![0u16; 3 * 4];
        let bytes = synth(1, 3, &samples);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        let skipped = reader.skip_rows(100).unwrap();
        assert_eq!(skipped, 3);
        assert_eq!(reader.rows_read(), 3);
        assert_eq!(reader.rows_remaining(), 0);
        // Subsequent skip_rows on an exhausted reader returns 0.
        assert_eq!(reader.skip_rows(5).unwrap(), 0);
    }

    #[test]
    fn skip_rows_zero_is_a_noop() {
        let samples = vec![0u16; 2 * 2 * 4];
        let bytes = synth(2, 2, &samples);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.skip_rows(0).unwrap(), 0);
        assert_eq!(reader.rows_read(), 0);
        // Reader is still fully usable — full read_all_rows must work.
        let drained = reader.read_all_rows().unwrap();
        assert_eq!(drained.len(), 2 * 2 * 4);
    }

    #[test]
    fn skip_rows_then_read_remaining_matches_full_decode() {
        // Decode "skip first 2 rows of a 4-row image, then read the
        // remaining 2" and check the result matches the corresponding
        // tail of a full decode.
        let w = 5u32;
        let h = 4u32;
        let mut samples = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = (y * w + x) as u16;
                samples.push(v.wrapping_mul(0x0123));
                samples.push(v.wrapping_mul(0x4567));
                samples.push(v.wrapping_mul(0x89AB));
                samples.push(v.wrapping_mul(0xCDEF));
            }
        }
        let bytes = synth(w, h, &samples);

        // Full decode for reference.
        let mut full_reader = FarbfeldStreamReader::new(Cursor::new(bytes.clone())).unwrap();
        let full = full_reader.read_all_rows().unwrap();

        // Skip-then-read-tail decode.
        let mut win_reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(win_reader.skip_rows(2).unwrap(), 2);
        assert_eq!(win_reader.rows_read(), 2);
        let tail = win_reader.read_all_rows().unwrap();

        let row_samples = (w * 4) as usize;
        assert_eq!(tail, full[row_samples * 2..]);
    }

    #[test]
    fn read_row_raw_yields_on_disk_bytes_verbatim() {
        // 3×2 image — assemble a known sample plane, encode it, then
        // walk the body row-by-row via `read_row_raw` and check the
        // emitted bytes equal the raw body bytes of the synthesised
        // reference. No native-endian conversion happens on this path,
        // so the bytes must come out exactly as they went in.
        let mut samples = Vec::new();
        for y in 0..2u32 {
            for x in 0..3u32 {
                let v = (y * 100 + x * 10) as u16;
                samples.push(v);
                samples.push(v.wrapping_add(1));
                samples.push(v.wrapping_add(2));
                samples.push(v.wrapping_add(3));
            }
        }
        let bytes = synth(3, 2, &samples);
        let row_bytes_len = 3 * BYTES_PER_PIXEL;
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes.clone())).unwrap();

        let mut row_raw = vec![0u8; row_bytes_len];
        assert!(reader.read_row_raw(&mut row_raw).unwrap());
        assert_eq!(
            row_raw,
            bytes[HEADER_LEN..HEADER_LEN + row_bytes_len],
            "row 0 raw bytes must equal the on-disk body prefix"
        );
        assert_eq!(reader.rows_read(), 1);

        assert!(reader.read_row_raw(&mut row_raw).unwrap());
        assert_eq!(
            row_raw,
            bytes[HEADER_LEN + row_bytes_len..HEADER_LEN + 2 * row_bytes_len],
            "row 1 raw bytes must equal the on-disk body's second row"
        );
        assert_eq!(reader.rows_read(), 2);

        // Past end — Ok(false).
        assert!(!reader.read_row_raw(&mut row_raw).unwrap());
    }

    #[test]
    fn read_row_raw_rejects_wrong_out_length() {
        let bytes = synth(2, 1, &[0u16; 8]);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        // width=2 needs 16-byte slot; pass 8.
        let mut row = vec![0u8; 8];
        assert!(reader.read_row_raw(&mut row).is_err());
    }

    #[test]
    fn read_row_raw_handles_zero_width_no_body() {
        // 0×3 — three zero-byte rows; read_row_raw still counts them.
        let bytes = synth(0, 3, &[]);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        let mut row: Vec<u8> = Vec::new();
        for i in 0..3u32 {
            assert!(reader.read_row_raw(&mut row).unwrap());
            assert_eq!(reader.rows_read(), i + 1);
        }
        assert!(!reader.read_row_raw(&mut row).unwrap());
    }

    #[test]
    fn read_row_raw_propagates_truncated_body() {
        // 2×2 (= 32 body bytes) but only 16 bytes of body present
        // — one full row + nothing for the second.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 16]); // one full row only
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes)).unwrap();
        let mut row = vec![0u8; 16];
        // First row needs 16 bytes; ok.
        assert!(reader.read_row_raw(&mut row).is_ok());
        // Second row needs 16; none left — truncation.
        let err = reader.read_row_raw(&mut row).unwrap_err();
        let FarbfeldError::InvalidData(s) = err;
        assert!(s.contains("truncated"), "msg = {s:?}");
    }

    #[test]
    fn read_row_raw_and_read_row_are_interchangeable_mid_stream() {
        // Mixing `read_row_raw` and `read_row` across the same reader
        // must produce contiguous rows — the raw path and the converted
        // path share the same row-bytes pump.
        let mut samples = Vec::new();
        for y in 0..4u32 {
            for x in 0..3u32 {
                let v = (y * 3 + x) as u16;
                samples.push(v.wrapping_mul(0x0123));
                samples.push(v.wrapping_mul(0x4567));
                samples.push(v.wrapping_mul(0x89AB));
                samples.push(v.wrapping_mul(0xCDEF));
            }
        }
        let bytes = synth(3, 4, &samples);
        let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes.clone())).unwrap();

        // Row 0 via raw — verify bytes.
        let row_bytes_len = 3 * BYTES_PER_PIXEL;
        let mut raw = vec![0u8; row_bytes_len];
        assert!(reader.read_row_raw(&mut raw).unwrap());
        assert_eq!(raw, bytes[HEADER_LEN..HEADER_LEN + row_bytes_len]);

        // Row 1 via native — verify the matching sample slice.
        let mut row_native = vec![0u16; 12];
        assert!(reader.read_row(&mut row_native).unwrap());
        assert_eq!(row_native, &samples[12..24]);

        // Row 2 via skip.
        assert!(reader.skip_row().unwrap());

        // Row 3 via raw again — confirm we're aligned on row 3's bytes.
        assert!(reader.read_row_raw(&mut raw).unwrap());
        let row3_off = HEADER_LEN + 3 * row_bytes_len;
        assert_eq!(raw, bytes[row3_off..row3_off + row_bytes_len]);

        assert_eq!(reader.rows_read(), 4);
        assert!(!reader.read_row_raw(&mut raw).unwrap());
    }

    #[test]
    fn write_row_raw_accepts_be_bytes_verbatim_byte_exact_against_reference() {
        // Construct a sample plane, build the synthesised reference
        // (header + per-sample BE), then drive `write_row_raw` with
        // each row's BE-encoded body bytes and check the writer's
        // emitted byte stream equals the reference exactly.
        let mut samples = Vec::new();
        for y in 0..3u32 {
            for x in 0..2u32 {
                let base = (y * 2 + x) as u16;
                samples.push(base.wrapping_mul(0x0123));
                samples.push(base.wrapping_mul(0x4567));
                samples.push(base.wrapping_mul(0x89AB));
                samples.push(base.wrapping_mul(0xCDEF));
            }
        }
        let reference = synth(2, 3, &samples);
        let row_bytes_len = 2 * BYTES_PER_PIXEL;
        // The body bytes of the reference are the per-sample BE bytes;
        // slice them per row to feed `write_row_raw`.
        let body = &reference[HEADER_LEN..];

        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 2, 3).unwrap();
        for row_idx in 0..3usize {
            let off = row_idx * row_bytes_len;
            writer
                .write_row_raw(&body[off..off + row_bytes_len])
                .unwrap();
        }
        let bytes = writer.finish().unwrap();
        assert_eq!(bytes, reference);
    }

    #[test]
    fn write_row_raw_rejects_wrong_row_length() {
        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 3, 1).unwrap();
        // width=3 needs 24 bytes; pass 16.
        let too_short = [0u8; 16];
        assert!(writer.write_row_raw(&too_short).is_err());
    }

    #[test]
    fn write_row_raw_rejects_extra_row() {
        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 1, 2).unwrap();
        let row = [0u8; BYTES_PER_PIXEL];
        writer.write_row_raw(&row).unwrap();
        writer.write_row_raw(&row).unwrap();
        // Third call past height = error.
        assert!(writer.write_row_raw(&row).is_err());
    }

    #[test]
    fn write_row_raw_handles_zero_width() {
        // 0×3 — three rows, each empty body slice.
        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 0, 3).unwrap();
        for _ in 0..3 {
            writer.write_row_raw(&[]).unwrap();
        }
        let bytes = writer.finish().unwrap();
        // 16-byte header only.
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(&bytes[8..12], &[0, 0, 0, 0]);
        assert_eq!(&bytes[12..16], &[0, 0, 0, 3]);
    }

    #[test]
    fn write_row_raw_and_write_row_can_be_mixed_mid_stream() {
        // Mixing the two write paths must produce a stream that round-
        // trips through `parse_farbfeld` and matches the per-row native-
        // endian samples — the raw path emits the bytes verbatim, the
        // native path encodes them, and the combined output is
        // continuous.
        let mut samples = Vec::new();
        for y in 0..4u32 {
            for x in 0..2u32 {
                let v = (y * 2 + x) as u16;
                samples.push(v.wrapping_mul(0x1111));
                samples.push(v.wrapping_mul(0x2222));
                samples.push(v.wrapping_mul(0x3333));
                samples.push(v.wrapping_mul(0x4444));
            }
        }
        let reference = synth(2, 4, &samples);
        let row_bytes_len = 2 * BYTES_PER_PIXEL;
        let body = &reference[HEADER_LEN..];

        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 2, 4).unwrap();
        // Row 0 — raw bytes.
        writer.write_row_raw(&body[..row_bytes_len]).unwrap();
        // Row 1 — native samples.
        writer.write_row(&samples[8..16]).unwrap();
        // Row 2 — raw bytes.
        writer
            .write_row_raw(&body[2 * row_bytes_len..3 * row_bytes_len])
            .unwrap();
        // Row 3 — native samples.
        writer.write_row(&samples[24..32]).unwrap();

        let bytes = writer.finish().unwrap();
        assert_eq!(bytes, reference);
    }

    #[test]
    fn raw_passthrough_round_trips_reader_into_writer_without_conversion() {
        // The combined value of `read_row_raw` + `write_row_raw` is
        // shovelling bytes from one farbfeld stream to another without
        // ever touching the native-endian sample shape. Build a
        // reference stream, drain it row-by-row via `read_row_raw`,
        // forward each row via `write_row_raw`, and confirm the output
        // equals the input byte-for-byte.
        let mut samples = Vec::new();
        for y in 0..5u32 {
            for x in 0..7u32 {
                let v = (y * 7 + x) as u16;
                samples.push(v.wrapping_mul(0x1357));
                samples.push(v.wrapping_mul(0x2468));
                samples.push(v.wrapping_mul(0xACE0));
                samples.push(v.wrapping_mul(0xBDF1));
            }
        }
        let reference = synth(7, 5, &samples);

        let mut reader = FarbfeldStreamReader::new(Cursor::new(reference.clone())).unwrap();
        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 7, 5).unwrap();
        let mut row_raw = vec![0u8; 7 * BYTES_PER_PIXEL];
        while reader.read_row_raw(&mut row_raw).unwrap() {
            writer.write_row_raw(&row_raw).unwrap();
        }
        let forwarded = writer.finish().unwrap();
        assert_eq!(forwarded, reference);
    }

    #[test]
    fn stream_roundtrip_matches_in_memory_parse_and_encode() {
        let mut samples = Vec::new();
        for y in 0..7u32 {
            for x in 0..5u32 {
                let v = (y * 5 + x) as u16;
                samples.push(v.wrapping_mul(0x1111));
                samples.push(v.wrapping_mul(0x2222));
                samples.push(v.wrapping_mul(0x3333));
                samples.push(v.wrapping_mul(0x4444));
            }
        }
        let reference = synth(5, 7, &samples);

        // Streaming decode -> flat samples -> compare against samples.
        let mut reader = FarbfeldStreamReader::new(Cursor::new(reference.clone())).unwrap();
        let decoded = reader.read_all_rows().unwrap();
        assert_eq!(decoded, samples);

        // Streaming encode -> bytes -> compare against reference.
        let mut writer = FarbfeldStreamWriter::new(Vec::new(), 5, 7).unwrap();
        for y in 0..7 {
            let off = y * 5 * 4;
            writer.write_row(&samples[off..off + 5 * 4]).unwrap();
        }
        let encoded = writer.finish().unwrap();
        assert_eq!(encoded, reference);
    }

    #[test]
    fn read_all_rows_spare_capacity_decode_is_bit_identical_to_whole_file() {
        // `read_all_rows` decodes each row straight into the output
        // `Vec`'s spare (uninitialised) capacity and then `set_len`s past
        // it, skipping the per-row zero-init. Lock that this produces the
        // exact same flat sample buffer the whole-file `parse_farbfeld`
        // decoder yields — both for a fresh reader (the `prev_len == 0`
        // first row) and after a partial `read_row` drain (the
        // `prev_len != 0` mid-buffer growth path), across a width whose
        // row sample count is not a multiple of the SIMD lane width.
        use crate::parse_farbfeld;

        for &(w, h) in &[(1u32, 1u32), (3, 5), (7, 1), (1, 9), (13, 11)] {
            let mut samples = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let v = (y.wrapping_mul(w).wrapping_add(x)) as u16;
                    samples.push(v.wrapping_mul(0x0123).wrapping_add(0xBEEF));
                    samples.push(v.wrapping_mul(0x4567));
                    samples.push(v.wrapping_mul(0x89AB).wrapping_add(0x0F0F));
                    samples.push(v.wrapping_mul(0xCDEF));
                }
            }
            let bytes = synth(w, h, &samples);
            let whole = parse_farbfeld(&bytes).unwrap();

            // Fresh reader: every row taken by `read_all_rows`.
            let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes.clone())).unwrap();
            let all = reader.read_all_rows().unwrap();
            assert_eq!(all, whole.pixels, "fresh read_all_rows mismatch at {w}x{h}");

            // Partial drain: consume the first row via `read_row` (when
            // there is one), then let `read_all_rows` grow the buffer from
            // a non-zero starting length for the remaining rows. The
            // concatenation must still equal the whole-file decode.
            if h >= 1 {
                let mut reader = FarbfeldStreamReader::new(Cursor::new(bytes.clone())).unwrap();
                let row_samples = (w as usize) * 4;
                let mut first = vec![0u16; row_samples];
                assert!(reader.read_row(&mut first).unwrap());
                let rest = reader.read_all_rows().unwrap();
                let mut joined = first;
                joined.extend_from_slice(&rest);
                assert_eq!(
                    joined, whole.pixels,
                    "partial-drain read_all_rows mismatch at {w}x{h}"
                );
            }
        }
    }

    #[test]
    fn read_all_rows_above_cap_announcement_with_no_body_fails_fast() {
        // The one-shot reservation in `read_all_rows` is gated by
        // `ONE_SHOT_SAMPLE_CAP`: an announced body at or below the cap is
        // reserved in a single allocation, but an announcement *above* the
        // cap must fall back to the incremental row-by-row path so a
        // header promising a huge body it never ships cannot trigger an
        // eager giant allocation. This locks the gate: a header announcing
        // a body well past the cap, followed by zero body bytes, must fail
        // as a truncation error (the incremental path's first short row
        // read), near-instantly, never OOM. 0x4000 × 0x4000 = 256 Mi
        // pixels = 1 Gi samples — 16× the 64 Mi cap.
        let big = 0x4000u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&big.to_be_bytes());
        buf.extend_from_slice(&big.to_be_bytes());
        let mut reader = FarbfeldStreamReader::new(Cursor::new(buf)).unwrap();
        let t = std::time::Instant::now();
        let err = reader.read_all_rows().expect_err("no body — must refuse");
        let dt = t.elapsed();
        let FarbfeldError::InvalidData(msg) = err;
        assert!(msg.contains("truncated"), "msg = {msg:?}");
        assert!(
            dt < std::time::Duration::from_millis(500),
            "above-cap announcement took {dt:?} — incremental path must not pre-allocate",
        );
    }
}
