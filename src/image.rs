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
}
