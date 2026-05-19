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

use crate::error::{FarbfeldError, Result};
use crate::parser::{parse_farbfeld_header, FarbfeldHeader, BYTES_PER_PIXEL, HEADER_LEN, MAGIC};

/// Row-at-a-time farbfeld reader.
///
/// Reads the 16-byte header from the underlying `R` at construction
/// time; the rest of the body is consumed lazily by
/// [`read_row`](Self::read_row), one row per call.
pub struct FarbfeldStreamReader<R: Read> {
    inner: R,
    header: FarbfeldHeader,
    rows_read: u32,
    /// Reusable per-row scratch buffer (`width * 8` bytes of on-disk
    /// big-endian samples).
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
        let row_bytes = (header.width as usize)
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or_else(|| FarbfeldError::invalid("farbfeld: row size overflows usize"))?;
        Ok(Self {
            inner,
            header,
            rows_read: 0,
            row_buf: vec![0u8; row_bytes],
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
        if !self.row_buf.is_empty() {
            read_exact_to_invalid(
                &mut self.inner,
                &mut self.row_buf,
                "farbfeld stream: row body",
            )?;
            for (i, slot) in out.iter_mut().enumerate() {
                let off = i * 2;
                *slot = u16::from_be_bytes([self.row_buf[off], self.row_buf[off + 1]]);
            }
        }
        self.rows_read += 1;
        Ok(true)
    }

    /// Convenience: drain the remaining body, accumulating every row
    /// into a flat row-major `Vec<u16>` of length `width * height * 4`.
    pub fn read_all_rows(&mut self) -> Result<Vec<u16>> {
        let total_samples = (self.header.width as usize)
            .checked_mul(self.header.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| FarbfeldError::invalid("farbfeld stream: total samples overflow"))?;
        let mut out = vec![0u16; total_samples];
        let row_samples = (self.header.width as usize) * 4;
        if row_samples == 0 {
            // Width=0 — drain every row (no body bytes) so rows_read
            // reflects the height.
            while self.read_row(&mut [])? {}
            return Ok(out);
        }
        for row_idx in 0..self.header.height as usize {
            let off = row_idx * row_samples;
            let row_slice = &mut out[off..off + row_samples];
            let got = self.read_row(row_slice)?;
            if !got {
                return Err(FarbfeldError::invalid(format!(
                    "farbfeld stream: read_all_rows short — got {row_idx} of {} rows",
                    self.header.height,
                )));
            }
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
            for (i, &sample) in row.iter().enumerate() {
                let off = i * 2;
                let be = sample.to_be_bytes();
                self.row_buf[off] = be[0];
                self.row_buf[off + 1] = be[1];
            }
            write_all_to_invalid(&mut self.inner, &self.row_buf, "farbfeld stream: row body")?;
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
}
