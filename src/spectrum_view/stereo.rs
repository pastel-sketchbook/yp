//! Stereo: left and right channels as opposing meters.
//!
//! Reads as a single bar when the channels match and opens a V when the stereo
//! image is wide, which the summed display could never show.

use super::SpectrumView;
use super::bars::BarKind;
use super::geometry::sample_interpolated;
use crate::spectrum::BANDS;
use crate::theme::Theme;
use ratatui::{buffer::Buffer, layout::Rect};

impl SpectrumView {
  pub(super) fn draw_stereo(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    let height = body.height & !1;
    if height < 4 || body.width < 8 {
      self.draw_bars(buf, body, theme, BarKind::Zoned);
      return;
    }
    let half = height / 2;
    let count = BANDS.min(usize::from(body.width) / 2).max(1);
    let bottom = body.y + height;
    for bar in 0..count {
      let position = bar as f32 / (count - 1).max(1) as f32;
      let left = sample_interpolated(&self.levels, position) * f32::from(half);
      let right = sample_interpolated(&self.levels_right, position) * f32::from(half);
      let x = body.x + (bar * 2) as u16;
      if x >= body.x + body.width {
        break;
      }
      for row in 0..half {
        // The upper half grows up from the midline, the lower grows down. The two
        // meet between rows `bottom - half - 1` and `bottom - half`.
        let y_up = bottom - half + row;
        let y_down = bottom - half - 1 - row;
        let (glyph_l, _) = Self::bar_cell(BarKind::Zoned, theme, row, half, left, 0.0);
        let (glyph_r, _) = Self::bar_cell(BarKind::Zoned, theme, row, half, right, 0.0);
        buf[(x, y_up)].set_char(glyph_l).set_fg(Self::zone(theme, row, half)).set_bg(theme.panel_bg);
        buf[(x, y_down)].set_char(glyph_r).set_fg(Self::zone(theme, row, half)).set_bg(theme.panel_bg);
      }
      // A dim spine keeps the two meters visually paired.
      buf[(x, bottom - half)].set_char('│').set_fg(theme.border).set_bg(theme.panel_bg);
    }
  }
}
