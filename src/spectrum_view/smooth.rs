//! Smooth: braille bars with the bands interpolated between them.
//!
//! Braille glyphs pack two dot columns and four dot rows per cell, which is four
//! times the vertical resolution of half-blocks and twice the horizontal. That
//! is what turns 32 bands into a curve instead of a staircase.

use super::SpectrumView;
use super::bars::BarKind;
use super::geometry::{BRAILLE_COLUMNS, BRAILLE_ROWS, braille_glyph, sample_interpolated};
use crate::theme::Theme;
use ratatui::{buffer::Buffer, layout::Rect};

impl SpectrumView {
  /// Falls back to plain bars when the pane is too small for the extra detail
  /// to show, since four rows per cell in two rows of body is just noise.
  pub(super) fn draw_smooth(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    if body.width < 4 || body.height < 2 {
      self.draw_bars(buf, body, theme, BarKind::Zoned);
      return;
    }
    let dot_rows = usize::from(body.height) * BRAILLE_ROWS;
    let dot_columns = usize::from(body.width) * BRAILLE_COLUMNS;
    for row in 0..body.height {
      let y = body.y + row;
      for x in 0..body.width {
        let mut filled = [[false; BRAILLE_ROWS]; BRAILLE_COLUMNS];
        let mut any = false;
        for (column, column_filled) in filled.iter_mut().enumerate() {
          let index = usize::from(x) * BRAILLE_COLUMNS + column;
          if index >= dot_columns {
            continue;
          }
          let position = index as f32 / (dot_columns - 1).max(1) as f32;
          let level = sample_interpolated(&self.levels, position) * dot_rows as f32;
          // Dots fill upward from the bottom of the cell stack.
          let base = usize::from(body.height - 1 - row) * BRAILLE_ROWS;
          for dot_row in 0..BRAILLE_ROWS {
            if base + dot_row < level.ceil() as usize {
              // Glyph rows count from the top of the cell.
              column_filled[BRAILLE_ROWS - 1 - dot_row] = true;
              any = true;
            }
          }
        }
        let cell = &mut buf[(body.x + x, y)];
        if any {
          cell.set_char(braille_glyph(&filled));
        } else {
          cell.set_char(' ');
        }
        cell.set_fg(Self::zone(theme, row, body.height)).set_bg(theme.panel_bg);
      }
    }
  }
}
