// Derived from vtamp (MIT, (c) 2026 Jang-Ho Hwang and vtamp contributors).
// See NOTICE at the repository root for the full licence text.
//! A filled, interpolated curve on the shared braille canvas.
//!
//! Braille dots pack two columns and four rows per cell, which is four times the
//! vertical resolution of half-blocks and twice the horizontal. Drawing through
//! the shared canvas rather than assembling glyphs here means the curve colors by
//! height zone for free, and the same dot addressing serves every braille style.

use super::bars::zone;
use super::braille::Braille;
use super::geometry::BRAILLE_ROWS;
use super::geometry::sample_at;
use crate::spectrum::BANDS;
use crate::theme::Theme;
use ratatui::{buffer::Buffer, layout::Rect};

/// Whether there is room for the extra detail, or the caller should draw bars.
pub(super) fn fits(body: Rect) -> bool {
  body.width >= 4 && body.height >= 2
}

pub(super) fn draw(buf: &mut Buffer, body: Rect, theme: &Theme, levels: &[f32; BANDS]) {
  let mut canvas = Braille::new(body);
  let (columns, rows) = canvas.size();
  for x in 0..columns {
    let dots = (sample_at(levels, x as usize, columns as usize) * rows as f32).ceil() as i32;
    for height in 0..dots.min(rows) {
      // Color by cell row, so the gradient runs up the curve in bands.
      canvas.dot(x, rows - 1 - height, zone(theme, (height as usize / BRAILLE_ROWS) as u16, body.height), 0);
    }
  }
  canvas.render(buf, theme);
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::spectrum_view::braille::dots_of;
  use crate::theme::THEMES;

  #[test]
  fn the_curve_fills_upward_uses_height_zones_and_keeps_the_treble() {
    let body = Rect::new(0, 0, 32, 8);
    let mut buf = Buffer::empty(body);
    let ramp: [f32; BANDS] = std::array::from_fn(|i| i as f32 / (BANDS - 1) as f32);
    draw(&mut buf, body, &THEMES[0], &ramp);
    assert_eq!(buf[(0, 0)].symbol(), " ", "a ramp starts at silence on the left");
    assert_eq!(dots_of(buf[(31, 0)].symbol()).len(), 8, "a full column lights all four rows");
    assert_eq!(buf[(31, 0)].fg, THEMES[0].spectrum[2], "the top is the high role");
    assert_eq!(buf[(31, 7)].fg, THEMES[0].spectrum[0], "the bottom is the low role");
    // The whole point of interpolating: no column is left half empty next to a
    // full one, so the curve reads as a slope rather than a staircase.
    let mut buf = Buffer::empty(body);
    draw(&mut buf, body, &THEMES[0], &[0.0; BANDS]);
    assert!(buf.content().iter().all(|cell| cell.symbol() == " "), "silence draws nothing");
  }

  #[test]
  fn a_pane_too_small_for_the_detail_is_declined() {
    for (width, height) in [(0_u16, 0_u16), (1, 5), (3, 8), (4, 1), (40, 1)] {
      assert!(!fits(Rect::new(0, 0, width, height)), "{width}x{height} cannot carry the detail");
    }
    assert!(fits(Rect::new(0, 0, 4, 2)));
    assert!(fits(Rect::new(0, 0, 40, 12)));
  }
}
