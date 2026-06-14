//! Standalone image container returned by `oxideav-farbfeld`'s
//! framework-free decode API and accepted by the standalone encode API.
//!
//! Defined here (rather than reusing `oxideav_core::VideoFrame`) so the
//! crate can be built with the default `registry` feature off — i.e.
//! without depending on `oxideav-core` at all. When the `registry`
//! feature is on the [`crate::registry`] module bridges between this
//! type and `oxideav_core::VideoFrame`.
//!
//! Layout: every farbfeld pixel is exactly four 16-bit channels in
//! `R, G, B, A` order. The on-disk representation is big-endian
//! 16-bit-per-sample, so a fully decoded image carries
//! `width * height * 4` `u16` values in [`FarbfeldImage::pixels`] —
//! flat row-major, with channel order matching the file.

/// One decoded farbfeld frame, framework-free shape.
///
/// The pixel buffer is flat row-major: pixel `(x, y)` starts at index
/// `(y * width + x) * 4`, with channels `[R, G, B, A]` as native-endian
/// `u16`. Big-endian conversion happens at the parser/encoder boundary
/// so callers always see the architecture's native word order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FarbfeldImage {
    /// Picture width in pixels — copied verbatim from the file header.
    pub width: u32,
    /// Picture height in pixels — copied verbatim from the file header.
    pub height: u32,
    /// Flat row-major pixel buffer, `width * height * 4` entries, in
    /// `R, G, B, A` order. Each value is a 16-bit channel sample in
    /// native endian (the parser converts from big-endian on read; the
    /// encoder converts back to big-endian on write).
    pub pixels: Vec<u16>,
}

/// Channel count per farbfeld pixel — four 16-bit components in
/// `R, G, B, A` order. Exposed as a named constant so callers
/// computing offsets into [`FarbfeldImage::pixels`] don't sprinkle
/// `4`s through the call site.
pub const CHANNELS_PER_PIXEL: usize = 4;

impl FarbfeldImage {
    /// Construct a [`FarbfeldImage`] from raw 16-bit RGBA samples.
    /// Returns `None` if `pixels.len() != width * height * 4` — the
    /// only invariant the type carries.
    pub fn new(width: u32, height: u32, pixels: Vec<u16>) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(CHANNELS_PER_PIXEL))?;
        if pixels.len() != expected {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels,
        })
    }

    /// Read the pixel at `(x, y)` as a four-channel `[R, G, B, A]`
    /// native-endian `u16` quad.
    ///
    /// Returns `None` if `x >= width` or `y >= height`. The returned
    /// array is a value-copy of the four `pixels` slots starting at
    /// `(y * width + x) * 4`; the type carries no border / wrap
    /// semantics — callers that want clamping or tiling must layer
    /// it themselves.
    ///
    /// ```
    /// use oxideav_farbfeld::FarbfeldImage;
    ///
    /// // 2×1 image: red, then green.
    /// let img = FarbfeldImage::new(
    ///     2,
    ///     1,
    ///     vec![0xFFFF, 0x0000, 0x0000, 0xFFFF, 0x0000, 0xFFFF, 0x0000, 0xFFFF],
    /// )
    /// .unwrap();
    /// assert_eq!(img.pixel(0, 0), Some([0xFFFF, 0x0000, 0x0000, 0xFFFF]));
    /// assert_eq!(img.pixel(1, 0), Some([0x0000, 0xFFFF, 0x0000, 0xFFFF]));
    /// assert_eq!(img.pixel(2, 0), None); // out of bounds
    /// ```
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u16; CHANNELS_PER_PIXEL]> {
        let base = self.pixel_offset(x, y)?;
        // `pixel_offset` already proved `base + 3` is in bounds (it
        // checked the full 4-sample slot); the indexed reads below are
        // therefore panic-free without per-slot bounds checks.
        Some([
            self.pixels[base],
            self.pixels[base + 1],
            self.pixels[base + 2],
            self.pixels[base + 3],
        ])
    }

    /// Overwrite the pixel at `(x, y)` with the supplied four-channel
    /// `[R, G, B, A]` native-endian quad.
    ///
    /// Returns `true` on success, `false` if `(x, y)` is out of bounds
    /// (in which case `pixels` is untouched). Symmetric to [`pixel`]
    /// for callers building a frame in place after `FarbfeldImage::new`.
    ///
    /// [`pixel`]: FarbfeldImage::pixel
    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: [u16; CHANNELS_PER_PIXEL]) -> bool {
        let Some(base) = self.pixel_offset(x, y) else {
            return false;
        };
        self.pixels[base] = rgba[0];
        self.pixels[base + 1] = rgba[1];
        self.pixels[base + 2] = rgba[2];
        self.pixels[base + 3] = rgba[3];
        true
    }

    /// Read a single channel `c` (0..=3 = R, G, B, A) at `(x, y)`.
    ///
    /// Returns `None` if `(x, y)` is out of bounds or `c >= 4`. Useful
    /// for downsampler / colour-conversion pipelines that walk one
    /// channel at a time and don't want to materialise the whole quad.
    pub fn channel(&self, x: u32, y: u32, c: usize) -> Option<u16> {
        if c >= CHANNELS_PER_PIXEL {
            return None;
        }
        let base = self.pixel_offset(x, y)?;
        Some(self.pixels[base + c])
    }

    /// Borrow row `y` as a contiguous slice of `width * 4` samples in
    /// `[R, G, B, A, R, G, B, A, …]` order.
    ///
    /// Returns `None` if `y >= height`. For a `width == 0` frame the
    /// slice is empty but still returned `Some(&[])` for any `y < height`
    /// (since each "row" is well-defined as a zero-length block).
    pub fn row(&self, y: u32) -> Option<&[u16]> {
        let (lo, hi) = self.row_range(y)?;
        Some(&self.pixels[lo..hi])
    }

    /// Mutable counterpart to [`row`].
    ///
    /// Returns `None` if `y >= height`. Callers can overwrite a whole
    /// scan line in one borrow without going through [`set_pixel`].
    ///
    /// [`row`]: FarbfeldImage::row
    pub fn row_mut(&mut self, y: u32) -> Option<&mut [u16]> {
        let (lo, hi) = self.row_range(y)?;
        Some(&mut self.pixels[lo..hi])
    }

    /// Total number of pixels in the frame (`width * height` as `usize`).
    ///
    /// Cannot overflow because [`FarbfeldImage::new`] already proved
    /// `width * height * 4` fits in `usize` when the value was
    /// constructed (and the parser does the same on decode).
    pub fn pixel_count(&self) -> usize {
        // The new()/parser invariant guarantees this multiplication
        // already fit through u32 → usize widening at construction time.
        (self.width as usize) * (self.height as usize)
    }

    /// Iterate the frame's scan lines top-to-bottom, yielding each row
    /// as a contiguous `&[u16]` slice of `width * 4` samples in
    /// `[R, G, B, A, R, G, B, A, …]` order.
    ///
    /// This is the sequential counterpart to [`row`]: it walks every
    /// row exactly once without the caller having to thread a `y`
    /// counter or re-check `Some`/`None` on each step. The iterator
    /// yields exactly `height` items; for a `width == 0` frame each
    /// yielded slice is empty (but the row count still equals `height`),
    /// matching [`row`]'s zero-width contract.
    ///
    /// Built on [`slice::chunks_exact`] over [`pixels`], so the
    /// iterator is allocation-free and the per-step cost is a pointer
    /// bump. A `height == 0` frame yields nothing.
    ///
    /// ```
    /// use oxideav_farbfeld::FarbfeldImage;
    ///
    /// // 1×3 image, one distinct red value per row.
    /// let img = FarbfeldImage::new(
    ///     1,
    ///     3,
    ///     vec![
    ///         0x0001, 0, 0, 0xFFFF, // row 0
    ///         0x0002, 0, 0, 0xFFFF, // row 1
    ///         0x0003, 0, 0, 0xFFFF, // row 2
    ///     ],
    /// )
    /// .unwrap();
    /// let reds: Vec<u16> = img.rows().map(|r| r[0]).collect();
    /// assert_eq!(reds, [1, 2, 3]);
    /// ```
    ///
    /// [`row`]: FarbfeldImage::row
    /// [`pixels`]: FarbfeldImage::pixels
    pub fn rows(&self) -> Rows<'_> {
        Rows {
            inner: self.chunked(&self.pixels),
        }
    }

    /// Mutable counterpart to [`rows`]: yields each scan line as a
    /// `&mut [u16]` of `width * 4` samples so a caller can rewrite the
    /// frame one row at a time in a single pass.
    ///
    /// Same `height`-item, zero-width-yields-empty-slice contract as
    /// [`rows`]; built on [`slice::chunks_exact_mut`].
    ///
    /// [`rows`]: FarbfeldImage::rows
    pub fn rows_mut(&mut self) -> RowsMut<'_> {
        // A `width == 0` frame has a zero-length row stride; chunks of
        // size 0 are forbidden, so route that case through an empty
        // iterator that still reports `height` rows below.
        let row_samples = (self.width as usize) * CHANNELS_PER_PIXEL;
        let height = self.height as usize;
        if row_samples == 0 {
            RowsMut {
                inner: [].chunks_exact_mut(1),
                zero_width_remaining: height,
            }
        } else {
            RowsMut {
                inner: self.pixels.chunks_exact_mut(row_samples),
                zero_width_remaining: 0,
            }
        }
    }

    /// Iterate the frame's pixels in row-major scan order, yielding each
    /// as an owned `[R, G, B, A]` native-endian `u16` quad.
    ///
    /// The sequential counterpart to [`pixel`]: it visits every pixel
    /// once (`pixel_count` items total) without the caller threading
    /// `(x, y)` or unwrapping `Option` per step. Built on
    /// [`slice::chunks_exact`] of width 4 over [`pixels`]; each yielded
    /// quad is a value-copy, so the iterator borrows the frame immutably
    /// and is allocation-free.
    ///
    /// ```
    /// use oxideav_farbfeld::FarbfeldImage;
    ///
    /// let img = FarbfeldImage::new(
    ///     2,
    ///     1,
    ///     vec![0xFFFF, 0, 0, 0xFFFF, 0, 0xFFFF, 0, 0xFFFF],
    /// )
    /// .unwrap();
    /// let quads: Vec<[u16; 4]> = img.pixels().collect();
    /// assert_eq!(quads, [[0xFFFF, 0, 0, 0xFFFF], [0, 0xFFFF, 0, 0xFFFF]]);
    /// ```
    ///
    /// [`pixel`]: FarbfeldImage::pixel
    /// [`pixels`]: FarbfeldImage::pixels
    pub fn pixels(&self) -> Pixels<'_> {
        Pixels {
            inner: self.pixels.chunks_exact(CHANNELS_PER_PIXEL),
        }
    }

    /// Build a `chunks_exact` iterator over `slice` whose chunk size is
    /// this frame's per-row sample count (`width * 4`), translating the
    /// degenerate `width == 0` case (chunk size 0, which `chunks_exact`
    /// forbids) into an empty-slice walker that still reports `height`
    /// rows. Shared by [`rows`](FarbfeldImage::rows).
    fn chunked<'a>(&self, slice: &'a [u16]) -> RowChunks<'a> {
        let row_samples = (self.width as usize) * CHANNELS_PER_PIXEL;
        if row_samples == 0 {
            RowChunks::ZeroWidth {
                remaining: self.height as usize,
            }
        } else {
            RowChunks::Sized(slice.chunks_exact(row_samples))
        }
    }

    // ---- internals ----

    /// Resolve `(x, y)` to the starting offset in [`pixels`] for the
    /// four-sample RGBA slot, or `None` if the coordinate is out of
    /// bounds. Single source of truth for the bounds + index arithmetic
    /// used by the public accessors above.
    fn pixel_offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        // (y * width + x) * 4. The new()/parser invariant proved the
        // final pixel index fits in usize, so the intermediate ops here
        // are guaranteed not to overflow on any reachable input.
        let pix_index = (y as usize) * (self.width as usize) + (x as usize);
        Some(pix_index * CHANNELS_PER_PIXEL)
    }

    /// Resolve row `y` to the `[lo, hi)` byte range covering its
    /// `width * 4` samples in [`pixels`], or `None` if `y >= height`.
    fn row_range(&self, y: u32) -> Option<(usize, usize)> {
        if y >= self.height {
            return None;
        }
        let row_samples = (self.width as usize) * CHANNELS_PER_PIXEL;
        let lo = (y as usize) * row_samples;
        let hi = lo + row_samples;
        Some((lo, hi))
    }
}

/// Internal scan-line chunker that bridges the normal
/// [`slice::chunks_exact`] path and the degenerate `width == 0` case
/// (chunk size 0 is rejected by `chunks_exact`, yet a zero-width frame
/// still has `height` well-defined empty rows).
enum RowChunks<'a> {
    /// Normal path: `width > 0`, so the row stride is non-zero.
    Sized(std::slice::ChunksExact<'a, u16>),
    /// `width == 0`: emit `remaining` empty rows then stop.
    ZeroWidth { remaining: usize },
}

impl<'a> Iterator for RowChunks<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<&'a [u16]> {
        match self {
            RowChunks::Sized(c) => c.next(),
            RowChunks::ZeroWidth { remaining } => {
                if *remaining == 0 {
                    None
                } else {
                    *remaining -= 1;
                    Some(&[])
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.len();
        (n, Some(n))
    }
}

impl<'a> DoubleEndedIterator for RowChunks<'a> {
    fn next_back(&mut self) -> Option<&'a [u16]> {
        match self {
            RowChunks::Sized(c) => c.next_back(),
            // A zero-width frame's rows are all the same empty slice, so
            // front and back are indistinguishable; just decrement the
            // shared counter so `rows().rev()` yields the same `height`
            // empty rows as the forward walk.
            RowChunks::ZeroWidth { remaining } => {
                if *remaining == 0 {
                    None
                } else {
                    *remaining -= 1;
                    Some(&[])
                }
            }
        }
    }
}

impl ExactSizeIterator for RowChunks<'_> {
    fn len(&self) -> usize {
        match self {
            RowChunks::Sized(c) => c.len(),
            RowChunks::ZeroWidth { remaining } => *remaining,
        }
    }
}

/// Iterator over a [`FarbfeldImage`]'s scan lines, yielding each row as
/// a `&[u16]` slice of `width * 4` samples. Created by
/// [`FarbfeldImage::rows`].
pub struct Rows<'a> {
    inner: RowChunks<'a>,
}

impl<'a> Iterator for Rows<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<&'a [u16]> {
        self.inner.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl DoubleEndedIterator for Rows<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back()
    }
}

impl ExactSizeIterator for Rows<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

/// Mutable iterator over a [`FarbfeldImage`]'s scan lines, yielding each
/// row as a `&mut [u16]` slice of `width * 4` samples. Created by
/// [`FarbfeldImage::rows_mut`].
pub struct RowsMut<'a> {
    inner: std::slice::ChunksExactMut<'a, u16>,
    /// Number of empty rows still to emit in the `width == 0` case
    /// (zero in the normal path, where `inner` does all the work).
    zero_width_remaining: usize,
}

impl<'a> Iterator for RowsMut<'a> {
    type Item = &'a mut [u16];

    fn next(&mut self) -> Option<&'a mut [u16]> {
        if self.zero_width_remaining > 0 {
            self.zero_width_remaining -= 1;
            return Some(&mut []);
        }
        self.inner.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.len();
        (n, Some(n))
    }
}

impl DoubleEndedIterator for RowsMut<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        // In the normal (`width > 0`) path `zero_width_remaining` is 0
        // and `inner` does all the work. In the `width == 0` path
        // `inner` is empty and every row is the same empty slice, so
        // taking from the back is the same as taking from the front.
        if let Some(back) = self.inner.next_back() {
            return Some(back);
        }
        if self.zero_width_remaining > 0 {
            self.zero_width_remaining -= 1;
            return Some(&mut []);
        }
        None
    }
}

impl ExactSizeIterator for RowsMut<'_> {
    fn len(&self) -> usize {
        self.inner.len() + self.zero_width_remaining
    }
}

/// Iterator over a [`FarbfeldImage`]'s pixels in row-major scan order,
/// yielding each as an owned `[R, G, B, A]` `u16` quad. Created by
/// [`FarbfeldImage::pixels`].
pub struct Pixels<'a> {
    inner: std::slice::ChunksExact<'a, u16>,
}

impl Iterator for Pixels<'_> {
    type Item = [u16; CHANNELS_PER_PIXEL];

    fn next(&mut self) -> Option<[u16; CHANNELS_PER_PIXEL]> {
        // `chunks_exact(4)` guarantees every yielded slice is exactly
        // four samples long, so the array conversion can't fail; map it
        // to a value-copy quad.
        self.inner.next().map(|c| [c[0], c[1], c[2], c[3]])
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.inner.len();
        (n, Some(n))
    }
}

impl DoubleEndedIterator for Pixels<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        // Same `chunks_exact(4)` guarantee as `next`: every yielded
        // chunk is exactly four samples, so the array conversion is
        // infallible.
        self.inner.next_back().map(|c| [c[0], c[1], c[2], c[3]])
    }
}

impl ExactSizeIterator for Pixels<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red_green_image() -> FarbfeldImage {
        // 2×1: pixel (0,0) = pure red, pixel (1,0) = pure green.
        FarbfeldImage::new(
            2,
            1,
            vec![
                0xFFFF, 0x0000, 0x0000, 0xFFFF, // (0,0) R
                0x0000, 0xFFFF, 0x0000, 0xFFFF, // (1,0) G
            ],
        )
        .unwrap()
    }

    #[test]
    fn new_rejects_mismatched_pixel_buffer() {
        // 2×2 expects 16 samples (4 pixels × 4 channels); only 4 given.
        assert!(FarbfeldImage::new(2, 2, vec![0u16; 4]).is_none());
        // Empty buffer for non-zero dims is also wrong.
        assert!(FarbfeldImage::new(1, 1, vec![]).is_none());
    }

    #[test]
    fn new_accepts_zero_dim_with_empty_buffer() {
        // 0×0 — empty pixels is the only valid buffer.
        let img = FarbfeldImage::new(0, 0, vec![]).unwrap();
        assert_eq!(img.pixel_count(), 0);
        // 0×5 — width is zero, so the pixel count is zero regardless
        // of height, and an empty buffer is correct.
        let img = FarbfeldImage::new(0, 5, vec![]).unwrap();
        assert_eq!(img.pixel_count(), 0);
    }

    #[test]
    fn pixel_reads_each_channel_in_rgba_order() {
        let img = red_green_image();
        assert_eq!(img.pixel(0, 0), Some([0xFFFF, 0x0000, 0x0000, 0xFFFF]));
        assert_eq!(img.pixel(1, 0), Some([0x0000, 0xFFFF, 0x0000, 0xFFFF]));
    }

    #[test]
    fn pixel_returns_none_out_of_bounds() {
        let img = red_green_image();
        assert_eq!(img.pixel(2, 0), None); // x == width
        assert_eq!(img.pixel(0, 1), None); // y == height
        assert_eq!(img.pixel(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn set_pixel_overwrites_one_slot_only() {
        let mut img = red_green_image();
        assert!(img.set_pixel(0, 0, [0x1234, 0x5678, 0x9ABC, 0xDEF0]));
        // The first slot changed.
        assert_eq!(img.pixel(0, 0), Some([0x1234, 0x5678, 0x9ABC, 0xDEF0]));
        // The second slot did NOT change.
        assert_eq!(img.pixel(1, 0), Some([0x0000, 0xFFFF, 0x0000, 0xFFFF]));
    }

    #[test]
    fn set_pixel_returns_false_and_no_op_out_of_bounds() {
        let mut img = red_green_image();
        let before = img.pixels.clone();
        assert!(!img.set_pixel(5, 5, [0, 0, 0, 0]));
        assert_eq!(img.pixels, before, "out-of-bounds set must not mutate");
    }

    #[test]
    fn channel_reads_individual_components() {
        let img = red_green_image();
        // pixel (0,0): R=0xFFFF, G=0, B=0, A=0xFFFF
        assert_eq!(img.channel(0, 0, 0), Some(0xFFFF));
        assert_eq!(img.channel(0, 0, 1), Some(0x0000));
        assert_eq!(img.channel(0, 0, 2), Some(0x0000));
        assert_eq!(img.channel(0, 0, 3), Some(0xFFFF));
        // pixel (1,0): R=0, G=0xFFFF, B=0, A=0xFFFF
        assert_eq!(img.channel(1, 0, 1), Some(0xFFFF));
    }

    #[test]
    fn channel_returns_none_for_oob_coord_or_channel() {
        let img = red_green_image();
        assert_eq!(img.channel(2, 0, 0), None); // x OOB
        assert_eq!(img.channel(0, 0, 4), None); // c OOB
        assert_eq!(img.channel(0, 0, usize::MAX), None);
    }

    #[test]
    fn row_returns_width_times_four_samples() {
        let img = red_green_image();
        let r = img.row(0).unwrap();
        assert_eq!(r.len(), 2 * CHANNELS_PER_PIXEL);
        assert_eq!(
            r,
            &[0xFFFF, 0x0000, 0x0000, 0xFFFF, 0x0000, 0xFFFF, 0x0000, 0xFFFF]
        );
    }

    #[test]
    fn row_oob_returns_none() {
        let img = red_green_image();
        assert!(img.row(1).is_none()); // y == height
        assert!(img.row(u32::MAX).is_none());
    }

    #[test]
    fn row_zero_width_returns_empty_slice() {
        // 0×3 — three "rows" of zero pixels each. Each row(y) for
        // y<3 must return Some(&[]); row(3) must return None.
        let img = FarbfeldImage::new(0, 3, vec![]).unwrap();
        for y in 0..3 {
            let r = img.row(y).expect("row in-bounds");
            assert!(r.is_empty(), "row {y} should be empty for width=0");
        }
        assert!(img.row(3).is_none());
    }

    #[test]
    fn row_mut_lets_caller_overwrite_one_scan_line() {
        // 2×2 image; overwrite row 1 wholesale.
        let mut img = FarbfeldImage::new(
            2,
            2,
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, // row 0
                0, 0, 0, 0, 0, 0, 0, 0, // row 1
            ],
        )
        .unwrap();
        let r1 = img.row_mut(1).unwrap();
        for s in r1.iter_mut() {
            *s = 0xABCD;
        }
        // Row 0 untouched; row 1 entirely 0xABCD.
        assert_eq!(img.row(0).unwrap(), &[0u16; 8]);
        assert_eq!(img.row(1).unwrap(), &[0xABCDu16; 8]);
    }

    #[test]
    fn pixel_count_matches_width_times_height() {
        assert_eq!(FarbfeldImage::new(0, 0, vec![]).unwrap().pixel_count(), 0);
        assert_eq!(red_green_image().pixel_count(), 2);
        let big = FarbfeldImage::new(64, 32, vec![0u16; 64 * 32 * 4]).unwrap();
        assert_eq!(big.pixel_count(), 64 * 32);
    }

    #[test]
    fn rows_iter_yields_every_scan_line_in_order() {
        // 1×3 image, one distinct red value per row.
        let img = FarbfeldImage::new(
            1,
            3,
            vec![
                0x0001, 0, 0, 0xFFFF, // row 0
                0x0002, 0, 0, 0xFFFF, // row 1
                0x0003, 0, 0, 0xFFFF, // row 2
            ],
        )
        .unwrap();
        let collected: Vec<&[u16]> = img.rows().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0], &[0x0001, 0, 0, 0xFFFF]);
        assert_eq!(collected[1], &[0x0002, 0, 0, 0xFFFF]);
        assert_eq!(collected[2], &[0x0003, 0, 0, 0xFFFF]);
    }

    #[test]
    fn rows_iter_agrees_with_row_accessor() {
        let img = FarbfeldImage::new(
            2,
            2,
            vec![
                0, 1, 2, 3, 4, 5, 6, 7, // row 0
                8, 9, 10, 11, 12, 13, 14, 15, // row 1
            ],
        )
        .unwrap();
        for (y, r) in img.rows().enumerate() {
            assert_eq!(r, img.row(y as u32).unwrap());
        }
    }

    #[test]
    fn rows_iter_is_exact_sized() {
        let img = FarbfeldImage::new(2, 3, vec![0u16; 2 * 3 * 4]).unwrap();
        let mut it = img.rows();
        assert_eq!(it.len(), 3);
        assert_eq!(it.size_hint(), (3, Some(3)));
        it.next();
        assert_eq!(it.len(), 2);
    }

    #[test]
    fn rows_iter_zero_width_yields_height_empty_rows() {
        // 0×3 — three rows, each an empty slice.
        let img = FarbfeldImage::new(0, 3, vec![]).unwrap();
        let collected: Vec<&[u16]> = img.rows().collect();
        assert_eq!(collected.len(), 3);
        assert!(collected.iter().all(|r| r.is_empty()));
        // ExactSize reports the right count up front.
        assert_eq!(img.rows().len(), 3);
    }

    #[test]
    fn rows_iter_zero_height_yields_nothing() {
        let img = FarbfeldImage::new(5, 0, vec![]).unwrap();
        assert_eq!(img.rows().count(), 0);
        assert_eq!(img.rows().len(), 0);
    }

    #[test]
    fn rows_mut_lets_caller_rewrite_each_row_in_one_pass() {
        let mut img = FarbfeldImage::new(2, 2, vec![0u16; 2 * 2 * 4]).unwrap();
        for (y, r) in img.rows_mut().enumerate() {
            for s in r.iter_mut() {
                *s = (y as u16) + 1;
            }
        }
        assert_eq!(img.row(0).unwrap(), &[1u16; 8]);
        assert_eq!(img.row(1).unwrap(), &[2u16; 8]);
    }

    #[test]
    fn rows_mut_zero_width_yields_height_empty_rows() {
        let mut img = FarbfeldImage::new(0, 4, vec![]).unwrap();
        let mut count = 0;
        for r in img.rows_mut() {
            assert!(r.is_empty());
            count += 1;
        }
        assert_eq!(count, 4);
    }

    #[test]
    fn pixels_iter_yields_every_pixel_in_scan_order() {
        let img = red_green_image();
        let quads: Vec<[u16; 4]> = img.pixels().collect();
        assert_eq!(
            quads,
            vec![
                [0xFFFF, 0x0000, 0x0000, 0xFFFF],
                [0x0000, 0xFFFF, 0x0000, 0xFFFF],
            ]
        );
    }

    #[test]
    fn pixels_iter_agrees_with_pixel_accessor() {
        // 3×2 image with a distinct value per channel per pixel.
        let mut pixels = vec![0u16; 3 * 2 * CHANNELS_PER_PIXEL];
        for (i, p) in pixels.iter_mut().enumerate() {
            *p = i as u16;
        }
        let img = FarbfeldImage::new(3, 2, pixels).unwrap();
        let mut iter = img.pixels();
        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(iter.next(), img.pixel(x, y));
            }
        }
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn pixels_iter_count_matches_pixel_count_and_is_exact_sized() {
        let img = FarbfeldImage::new(4, 5, vec![0u16; 4 * 5 * 4]).unwrap();
        assert_eq!(img.pixels().count(), img.pixel_count());
        assert_eq!(img.pixels().len(), 20);
        assert_eq!(img.pixels().size_hint(), (20, Some(20)));
    }

    #[test]
    fn pixels_iter_empty_for_zero_dim() {
        let img = FarbfeldImage::new(0, 0, vec![]).unwrap();
        assert_eq!(img.pixels().count(), 0);
        let img = FarbfeldImage::new(0, 7, vec![]).unwrap();
        assert_eq!(img.pixels().count(), 0);
    }

    #[test]
    fn pixel_offset_is_consistent_with_row_major_layout() {
        // 3×2 image; channel-0 (R) of pixel (x, y) is set to (y*3 + x).
        let mut pixels = vec![0u16; 3 * 2 * CHANNELS_PER_PIXEL];
        for y in 0..2 {
            for x in 0..3 {
                let base = (y * 3 + x) * CHANNELS_PER_PIXEL;
                pixels[base] = (y * 3 + x) as u16;
            }
        }
        let img = FarbfeldImage::new(3, 2, pixels).unwrap();
        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(img.channel(x, y, 0), Some((y * 3 + x) as u16));
            }
        }
    }

    // ---- DoubleEndedIterator ----

    #[test]
    fn rows_iter_reversed_yields_scan_lines_bottom_up() {
        let img = FarbfeldImage::new(
            1,
            3,
            vec![
                0x0001, 0, 0, 0xFFFF, // row 0
                0x0002, 0, 0, 0xFFFF, // row 1
                0x0003, 0, 0, 0xFFFF, // row 2
            ],
        )
        .unwrap();
        let bottom_up: Vec<&[u16]> = img.rows().rev().collect();
        assert_eq!(bottom_up.len(), 3);
        assert_eq!(bottom_up[0], &[0x0003, 0, 0, 0xFFFF]);
        assert_eq!(bottom_up[1], &[0x0002, 0, 0, 0xFFFF]);
        assert_eq!(bottom_up[2], &[0x0001, 0, 0, 0xFFFF]);
        // Reversed sequence is the exact mirror of the forward one.
        let mut forward: Vec<&[u16]> = img.rows().collect();
        forward.reverse();
        assert_eq!(forward, bottom_up);
    }

    #[test]
    fn rows_iter_meets_in_the_middle_from_both_ends() {
        // Distinct red value per row so front/back picks are unambiguous.
        let img = FarbfeldImage::new(
            1,
            4,
            vec![10, 0, 0, 1, 20, 0, 0, 1, 30, 0, 0, 1, 40, 0, 0, 1],
        )
        .unwrap();
        let mut it = img.rows();
        assert_eq!(it.next().unwrap()[0], 10); // front
        assert_eq!(it.next_back().unwrap()[0], 40); // back
        assert_eq!(it.next().unwrap()[0], 20); // front
        assert_eq!(it.next_back().unwrap()[0], 30); // back
        assert_eq!(it.next(), None);
        assert_eq!(it.next_back(), None);
    }

    #[test]
    fn rows_iter_reversed_zero_width_yields_height_empty_rows() {
        let img = FarbfeldImage::new(0, 3, vec![]).unwrap();
        let collected: Vec<&[u16]> = img.rows().rev().collect();
        assert_eq!(collected.len(), 3);
        assert!(collected.iter().all(|r| r.is_empty()));
    }

    #[test]
    fn rows_mut_reversed_rewrites_each_row_bottom_up() {
        let mut img = FarbfeldImage::new(1, 3, vec![0u16; 3 * 4]).unwrap();
        // Stamp each row with an ascending counter taken back-to-front,
        // so the *last* row gets 1, the first row gets 3.
        let mut tag = 0u16;
        for r in img.rows_mut().rev() {
            tag += 1;
            for s in r.iter_mut() {
                *s = tag;
            }
        }
        assert_eq!(img.row(0).unwrap(), &[3u16; 4]);
        assert_eq!(img.row(1).unwrap(), &[2u16; 4]);
        assert_eq!(img.row(2).unwrap(), &[1u16; 4]);
    }

    #[test]
    fn rows_mut_reversed_zero_width_yields_height_empty_rows() {
        let mut img = FarbfeldImage::new(0, 4, vec![]).unwrap();
        let mut count = 0;
        for r in img.rows_mut().rev() {
            assert!(r.is_empty());
            count += 1;
        }
        assert_eq!(count, 4);
    }

    #[test]
    fn pixels_iter_reversed_yields_pixels_last_to_first() {
        let img = red_green_image(); // 2×1: red then green
        let back_to_front: Vec<[u16; 4]> = img.pixels().rev().collect();
        assert_eq!(
            back_to_front,
            vec![
                [0x0000, 0xFFFF, 0x0000, 0xFFFF], // green (last)
                [0xFFFF, 0x0000, 0x0000, 0xFFFF], // red (first)
            ]
        );
    }

    #[test]
    fn pixels_iter_meets_in_the_middle_from_both_ends() {
        // 4×1, channel-0 of each pixel = its scan index.
        let img =
            FarbfeldImage::new(4, 1, vec![0, 0, 0, 1, 1, 0, 0, 1, 2, 0, 0, 1, 3, 0, 0, 1]).unwrap();
        let mut it = img.pixels();
        assert_eq!(it.next().unwrap()[0], 0); // front
        assert_eq!(it.next_back().unwrap()[0], 3); // back
        assert_eq!(it.next().unwrap()[0], 1); // front
        assert_eq!(it.next_back().unwrap()[0], 2); // back
        assert_eq!(it.next(), None);
        assert_eq!(it.next_back(), None);
    }

    #[test]
    fn pixels_iter_reversed_is_full_mirror_of_forward() {
        // Multi-row frame to confirm the reverse honours row-major order.
        let mut pixels = vec![0u16; 3 * 2 * CHANNELS_PER_PIXEL];
        for (i, p) in pixels.chunks_exact_mut(4).enumerate() {
            p[0] = i as u16;
        }
        let img = FarbfeldImage::new(3, 2, pixels).unwrap();
        let mut forward: Vec<[u16; 4]> = img.pixels().collect();
        forward.reverse();
        let reversed: Vec<[u16; 4]> = img.pixels().rev().collect();
        assert_eq!(forward, reversed);
        assert_eq!(reversed.first().unwrap()[0], 5); // last pixel first
        assert_eq!(reversed.last().unwrap()[0], 0); // first pixel last
    }
}
