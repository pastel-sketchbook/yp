//! Geometry and sampling shared by more than one style.
//!
//! The bar styles, the waterfall, and the particle styles all have to answer the
//! same two questions — which bands belong in this column, and what is the
//! loudest of them — so the answers live here rather than in each renderer.

use crate::spectrum::BANDS;
use std::ops::Range;

/// Dot rows per braille cell.
pub(super) const BRAILLE_ROWS: usize = 4;

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
#[derive(Debug)]
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

/// The band value under dot column `x` of `width`, joined by straight segments
/// between band centers and flat beyond the outer ones.
///
/// Fewer columns than bands merges them instead, so a narrow pane shows every
/// band rather than clipping the top. Reading column by column rather than by a
/// normalized position is what lets the curve styles, the bars, and the
/// frequency axis all agree on which band a given column means.
pub(super) fn sample_at(levels: &[f32; BANDS], x: usize, width: usize) -> f32 {
  if width < BANDS {
    return merged(levels, bands(x, width)).clamp(0.0, 1.0);
  }
  let position = ((x as f32 + 0.5) * BANDS as f32 / width as f32 - 0.5).max(0.0);
  let band = (position as usize).min(BANDS - 1);
  let next = (band + 1).min(BANDS - 1);
  let t = (position - band as f32).min(1.0);
  (levels[band] + (levels[next] - levels[band]) * t).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sampling_a_column_reads_between_its_neighbours() {
    // A linear ramp: every column must read near its own position.
    let ramp: [f32; BANDS] = std::array::from_fn(|i| i as f32 / (BANDS - 1) as f32);
    for width in [64_usize, 128, 512] {
      for x in 0..width {
        let read = sample_at(&ramp, x, width);
        let expected = x as f32 / (width - 1) as f32;
        assert!((read - expected).abs() < 0.06, "width {width} column {x}: read {read}, expected {expected}");
      }
    }
  }

  #[test]
  fn a_lone_hot_band_falls_off_around_itself() {
    let mut spike = [0.0; BANDS];
    spike[0] = 1.0;
    assert_eq!(sample_at(&spike, 0, 64), 1.0);
    assert_eq!(sample_at(&spike, 63, 64), 0.0, "far from the spike is silence");
    assert!(sample_at(&spike, 1, 64) < sample_at(&spike, 0, 64), "it must decay away from the spike");
  }

  #[test]
  fn a_narrow_row_merges_bands_rather_than_skipping_them() {
    let mut spike = [0.0; BANDS];
    spike[20] = 1.0;
    // With fewer columns than bands every band still reaches the display.
    for width in [1_usize, 4, 16, 31] {
      let peak = (0..width).map(|x| sample_at(&spike, x, width)).fold(0.0, f32::max);
      assert_eq!(peak, 1.0, "width {width} lost the hot band");
    }
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
