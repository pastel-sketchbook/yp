//! Geometry and sampling shared by more than one style.
//!
//! The bar styles, the waterfall, and the particle styles all have to answer the
//! same two questions — which bands belong in this column, and what is the
//! loudest of them — so the answers live here rather than in each renderer.

use crate::spectrum::BANDS;
use std::ops::Range;

/// Dot rows per braille cell.
pub(super) const BRAILLE_ROWS: usize = 4;
/// Dot columns per braille cell.
pub(super) const BRAILLE_COLUMNS: usize = 2;

/// The bands covered by column `index` of `count`.
pub(super) fn bands(index: usize, count: usize) -> Range<usize> {
  let start = index * BANDS / count;
  start..((index + 1) * BANDS / count).max(start + 1)
}

/// The loudest value in a span of bands.
pub(super) fn merged(values: &[f32; BANDS], bands: Range<usize>) -> f32 {
  values[bands].iter().copied().fold(0.0, f32::max)
}

/// One bar: the bands it merges and its body columns.
pub(super) struct Bar {
  pub(super) bands: Range<usize>,
  pub(super) columns: Range<u16>,
}

/// Bars run from low to high bands, centered in the body with one blank column
/// between neighbors; narrow bodies merge neighboring bands.
pub(super) fn bar_layout(width: u16) -> impl Iterator<Item = Bar> {
  let width = usize::from(width);
  let count = BANDS.min(width.div_ceil(2)).max(1);
  let step = (width + 1) / count;
  let bar = step.saturating_sub(1).max(1);
  let offset = (width + step - bar).saturating_sub(count * step) / 2;
  (0..count).map(move |index| {
    let x = (offset + index * step) as u16;
    Bar { bands: bands(index, count), columns: x..x + bar as u16 }
  })
}

/// Reads a band value at a fractional position, interpolating between bands.
///
/// [`BANDS`] values spread across a pane far wider than the band count leave
/// visible steps. Interpolating fills them in, which is what makes bars read as
/// a curve rather than a staircase.
pub(super) fn sample_interpolated(levels: &[f32; BANDS], position: f32) -> f32 {
  if BANDS == 1 {
    return levels[0];
  }
  let position = position.clamp(0.0, 1.0) * (BANDS - 1) as f32;
  let low = position.floor() as usize;
  let high = (low + 1).min(BANDS - 1);
  let t = position - low as f32;
  levels[low] * (1.0 - t) + levels[high] * t
}

/// Braille bit for a dot, addressed by column and row counted from the top.
const fn braille_bit(column: usize, row: usize) -> u16 {
  match (column, row) {
    (0, 0) => 0x0001,
    (0, 1) => 0x0002,
    (0, 2) => 0x0004,
    (0, 3) => 0x0040,
    (1, 0) => 0x0008,
    (1, 1) => 0x0010,
    (1, 2) => 0x0020,
    (1, 3) => 0x0080,
    _ => 0,
  }
}

/// Builds a braille glyph from a per-dot occupancy grid.
///
/// `filled[column][row]` with row 0 at the top, matching the glyph's own
/// numbering rather than the bar's bottom-up sense.
pub(super) fn braille_glyph(filled: &[[bool; BRAILLE_ROWS]; BRAILLE_COLUMNS]) -> char {
  let mut bits = 0_u16;
  for (column, rows) in filled.iter().enumerate() {
    for (row, on) in rows.iter().enumerate() {
      if *on {
        bits |= braille_bit(column, row);
      }
    }
  }
  char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ')
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn braille_glyphs_use_the_documented_dot_numbering() {
    assert_eq!(braille_glyph(&[[false; BRAILLE_ROWS]; BRAILLE_COLUMNS]), '\u{2800}', "blank cell");
    let mut bottom_left = [[false; BRAILLE_ROWS]; BRAILLE_COLUMNS];
    bottom_left[0][BRAILLE_ROWS - 1] = true;
    assert_eq!(braille_glyph(&bottom_left), char::from_u32(0x2800 + 0x40).expect("valid"));
    let mut top_right = [[false; BRAILLE_ROWS]; BRAILLE_COLUMNS];
    top_right[1][0] = true;
    assert_eq!(braille_glyph(&top_right), char::from_u32(0x2800 + 0x08).expect("valid"));
    assert_eq!(braille_glyph(&[[true; BRAILLE_ROWS]; BRAILLE_COLUMNS]), char::from_u32(0x28FF).expect("valid"));
  }

  #[test]
  fn interpolation_fills_the_gaps_between_bands() {
    // A linear ramp: any position between bands must read between its neighbours
    // rather than snapping to a staircase.
    let ramp: [f32; BANDS] = std::array::from_fn(|i| i as f32 / (BANDS - 1) as f32);
    assert_eq!(sample_interpolated(&ramp, 0.0), 0.0, "the low end is read");
    assert_eq!(sample_interpolated(&ramp, 1.0), 1.0, "the high end is read");
    for position in [0.1, 0.25, 0.5, 0.75, 0.9] {
      let read = sample_interpolated(&ramp, position);
      assert!((read - position).abs() < 0.05, "at {position} expected about {position}, got {read}");
    }
    // A lone hot band must still fall off smoothly around itself.
    let mut spike = [0.0; BANDS];
    spike[0] = 1.0;
    assert_eq!(sample_interpolated(&spike, 0.0), 1.0);
    assert_eq!(sample_interpolated(&spike, 1.0), 0.0, "far from the spike is silence");
    assert!(sample_interpolated(&spike, 0.02) > sample_interpolated(&spike, 0.08));
    assert_eq!(sample_interpolated(&spike, -1.0), 1.0, "out of range is clamped");
    assert_eq!(sample_interpolated(&spike, 2.0), 0.0);
  }

  #[test]
  fn every_band_belongs_to_exactly_one_bar() {
    for width in [1_u16, 2, 7, 20, 38, 61, 200] {
      let bars: Vec<Bar> = bar_layout(width).collect();
      assert!(!bars.is_empty(), "width {width} must still draw one bar");
      assert_eq!(bars[0].bands.start, 0, "width {width} starts at the low band");
      assert_eq!(bars.last().expect("non-empty").bands.end, BANDS, "width {width} reaches the top band");
      for pair in bars.windows(2) {
        assert_eq!(pair[0].bands.end, pair[1].bands.start, "bands must not skip or overlap at width {width}");
      }
      for bar in &bars {
        assert!(bar.columns.start < bar.columns.end, "a bar needs at least one column");
      }
      // One blank column between neighbors is what keeps the bars readable.
      for pair in bars.windows(2) {
        assert!(pair[0].columns.end < pair[1].columns.start, "bars must be separated at width {width}");
      }
      // Nothing may fall outside the body.
      let last = bars.last().expect("non-empty");
      assert!(last.columns.end <= width, "width {width} overflowed to {}", last.columns.end);
    }
  }

  #[test]
  fn merged_takes_the_loudest_band_not_the_average() {
    let mut levels = [0.0; BANDS];
    levels[3] = 0.9;
    levels[4] = 0.2;
    assert_eq!(merged(&levels, 0..8), 0.9);
    assert_eq!(merged(&levels, 5..8), 0.0);
  }

  #[test]
  fn bands_covers_the_whole_range_even_for_more_columns_than_bands() {
    let mut covered = vec![0_usize; BANDS];
    for column in 0..(BANDS * 3) {
      for band in bands(column, BANDS * 3) {
        covered[band] += 1;
      }
    }
    assert!(covered.iter().all(|count| *count == 3), "every band appears once per pass");
  }
}
