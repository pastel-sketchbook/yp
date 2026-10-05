// Derived from vtamp (MIT, (c) 2026 Jang-Ho Hwang and vtamp contributors).
// See NOTICE at the repository root for the full licence text.
//! Whole-cell segments, sharing the dots ladder and the spectrum gradient.
//!
//! The same ladder as `dots` with each step filled in and colored from the
//! gradient instead of the flat zones, which reads as a contribution graph
//! rather than a bar chart. It walks [`bar_layout`] so the segments line up with
//! the other bar styles and the frequency axis can snap to them.

use super::geometry::{bar_layout, merged};
use crate::spectrum::BANDS;
use crate::theme::{Theme, spectrum_gradient};
use ratatui::{buffer::Buffer, layout::Rect};

pub(super) fn draw(buf: &mut Buffer, body: Rect, theme: &Theme, levels: &[f32; BANDS], peaks: &[f32; BANDS]) {
  if body.height == 0 {
    return;
  }
  for bar in bar_layout(body.width) {
    let lit = (merged(levels, bar.bands.clone()) * f32::from(body.height)).round() as u16;
    let peak = (merged(peaks, bar.bands) * f32::from(body.height)).round() as u16;
    for x in bar.columns {
      for row in 0..body.height {
        let on = row < lit || peak.checked_sub(1) == Some(row);
        let color = if on {
          spectrum_gradient(theme, f32::from(row) / f32::from(body.height.saturating_sub(1).max(1)))
        } else {
          theme.border
        };
        buf[(body.x + x, body.bottom() - 1 - row)]
          .set_char(if on { '█' } else { '·' })
          .set_fg(color)
          .set_bg(theme.panel_bg);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::theme::THEMES;
  use ratatui::buffer::Buffer;

  fn first_column(body: Rect) -> u16 {
    body.x + bar_layout(body.width).next().expect("a bar").columns.start
  }

  #[test]
  fn segments_use_the_full_gradient_and_keep_the_held_peak() {
    let body = Rect::new(2, 3, 8, 5);
    let mut buf = Buffer::empty(body);
    draw(&mut buf, body, &THEMES[0], &[0.4; BANDS], &[1.0; BANDS]);
    let x = first_column(body);
    assert_eq!(buf[(x, 7)].symbol(), "█", "the level lights the bottom steps");
    assert_eq!(buf[(x, 7)].fg, THEMES[0].spectrum[0], "the bottom is the low role");
    assert_eq!(buf[(x, 5)].symbol(), "·", "above the level but below the held peak");
    assert_eq!(buf[(x, 3)].symbol(), "█", "the held peak still shows");
    assert_eq!(buf[(x, 3)].fg, THEMES[0].spectrum[2], "the top is the high role");
  }

  #[test]
  fn it_fills_whole_cells_and_never_draws_the_round_dot() {
    let body = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(body);
    draw(&mut buf, body, &THEMES[0], &[0.9; BANDS], &[0.9; BANDS]);
    let mut blocks = 0;
    let mut dots = 0;
    for cell in buf.content() {
      match cell.symbol() {
        "█" => blocks += 1,
        "●" => dots += 1,
        _ => {}
      }
    }
    assert!(blocks > 0, "squares must draw solid blocks");
    assert_eq!(dots, 0, "squares must not draw the round dot glyph");
    let colors: std::collections::HashSet<_> =
      buf.content().iter().filter(|cell| cell.symbol() == "█").map(|cell| cell.fg).collect();
    assert!(colors.len() >= 2, "the gradient must vary with height, got {} colors", colors.len());
  }

  #[test]
  fn it_lines_up_with_the_other_bar_styles() {
    // Sharing the layout is what lets the axis snap a label to a segment.
    let body = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(body);
    draw(&mut buf, body, &THEMES[0], &[1.0; BANDS], &[1.0; BANDS]);
    let bars: Vec<_> = bar_layout(body.width).collect();
    for bar in &bars {
      for x in bar.columns.clone() {
        assert_eq!(buf[(body.x + x, 11)].symbol(), "█", "every bar column is filled");
      }
    }
    // The gap between two bars is what keeps them from reading as one block.
    for pair in bars.windows(2) {
      assert!(pair[0].columns.end < pair[1].columns.start, "bars must be separated: {pair:?}");
    }
  }
}
