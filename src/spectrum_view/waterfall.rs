//! Waterfall: the history of recent frames scrolling upward.

use super::SpectrumView;
use super::geometry::{bands, merged};
use crate::theme::{Theme, spectrum_gradient};
use ratatui::{buffer::Buffer, layout::Rect};

/// Draws the scrolling history.
pub(super) fn draw(view: &SpectrumView, buf: &mut Buffer, body: Rect, theme: &Theme) {
  view.draw_waterfall(buf, body, theme)
}

impl SpectrumView {
  pub(super) fn draw_waterfall(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    let width = usize::from(body.width);
    if width == 0 || body.height == 0 {
      return;
    }
    let bottom = body.y + body.height;
    for row in 0..body.height {
      let y = bottom - 1 - row;
      let levels = self.history.len().checked_sub(1 + usize::from(row)).map(|index| self.history[index]);
      for column in 0..width {
        // Every column shows a band, merged when narrow and repeated when wide,
        // so the history fills the width without gaps.
        let x = body.x + column as u16;
        let level = levels.map_or(0.0, |levels| merged(&levels, bands(column, width))).clamp(0.0, 1.0);
        let cell = &mut buf[(x, y)];
        if level <= 0.0 {
          cell.set_char(' ').set_fg(theme.panel_bg).set_bg(theme.panel_bg);
        } else {
          let glyph = if level < 0.25 {
            '░'
          } else if level < 0.5 {
            '▒'
          } else if level < 0.75 {
            '▓'
          } else {
            '█'
          };
          cell.set_char(glyph).set_fg(spectrum_gradient(theme, level)).set_bg(theme.panel_bg);
        }
      }
    }
  }
}
