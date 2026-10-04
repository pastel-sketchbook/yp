//! Trail: bars with a fading tail drawn from recent frames.
//!
//! Reuses the waterfall's frame history, so this costs no extra state: a
//! transient leaves a visible streak that decays over the trail length.

use super::SpectrumView;
use super::geometry::sample_interpolated;
use crate::spectrum::BANDS;
use crate::theme::Theme;
use ratatui::{buffer::Buffer, layout::Rect};

/// Recent frames drawn behind the bars.
const TRAIL_LENGTH: usize = 6;

impl SpectrumView {
  pub(super) fn draw_trail(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    let height = body.height;
    if height == 0 || body.width == 0 {
      return;
    }
    let count = BANDS.min(usize::from(body.width)).max(1);
    let bottom = body.y + height;
    let history: Vec<&[f32; BANDS]> = self.history.iter().rev().take(TRAIL_LENGTH).collect();

    // Draw oldest first so newer frames land on top.
    for (age, levels) in history.iter().enumerate() {
      let fade = 1.0 - (age as f32 + 1.0) / (TRAIL_LENGTH + 1) as f32;
      if fade <= 0.0 {
        break;
      }
      for bar in 0..count {
        let position = bar as f32 / (count - 1).max(1) as f32;
        let level = sample_interpolated(levels, position) * f32::from(height);
        let x = body.x + (bar * 2) as u16;
        if x >= body.x + body.width {
          break;
        }
        let top = (bottom - 1).saturating_sub(level.round() as u16).max(body.y);
        let cell = &mut buf[(x, top)];
        // Newer frames keep the bar color; older ones fade toward the border.
        let color = if age == 0 { Self::zone(theme, top.saturating_sub(body.y), height) } else { theme.border };
        cell.set_char('▄').set_fg(color).set_bg(theme.panel_bg);
      }
    }
  }
}
