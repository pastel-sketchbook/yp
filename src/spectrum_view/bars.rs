//! The bar styles: bars, gradient, mono, mirror, dots, and squares.
//!
//! All six walk the same column layout and differ only in how a single cell of a
//! bar is chosen, so they share one loop and a [`BarKind`] tag rather than
//! carrying six near-identical renderers.

use super::SpectrumView;
use super::geometry::{bar_layout, merged};
use crate::theme::{Theme, spectrum_gradient};
use ratatui::{buffer::Buffer, layout::Rect, style::Color};

/// Eighths of a cell, from empty to solid.
pub(super) const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Which look the shared bar loop draws.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BarKind {
  Zoned,
  Gradient,
  Mono,
  Mirror,
  Dots,
  Squares,
}

/// Eighths of the cell at `row` covered by a bar of `level` rows.
pub(super) fn units(level: f32, row: u16) -> usize {
  ((level - row as f32) * 8.0).ceil().clamp(0.0, 8.0) as usize
}

/// Flat color for a row, matching the gradient's three zones.
pub(super) fn zone(theme: &Theme, row: u16, rows: u16) -> Color {
  let position = row as f32 / rows.max(1) as f32;
  if position < 0.55 {
    theme.spectrum[0]
  } else if position < 0.8 {
    theme.spectrum[1]
  } else {
    theme.spectrum[2]
  }
}

/// Draws one of the bar looks.
pub(super) fn draw(view: &SpectrumView, buf: &mut Buffer, body: Rect, theme: &Theme, kind: BarKind) {
  view.draw_bars(buf, body, theme, kind)
}

impl SpectrumView {
  pub(super) fn draw_bars(&self, buf: &mut Buffer, body: Rect, theme: &Theme, kind: BarKind) {
    let height = body.height;
    if height == 0 || body.width == 0 {
      return;
    }
    let rows = if kind == BarKind::Mirror { height & !1 } else { height };
    if rows == 0 {
      return;
    }
    let half = rows / 2;
    let scale = f32::from(if kind == BarKind::Mirror { half } else { rows });
    let bottom = body.y + height;
    for bar in bar_layout(body.width) {
      let level = merged(&self.levels, bar.bands.clone()) * scale;
      let peak = merged(&self.peaks, bar.bands) * scale;
      for x in bar.columns {
        let x = body.x + x;
        match kind {
          BarKind::Zoned | BarKind::Gradient | BarKind::Mono => {
            for row in 0..rows {
              let (glyph, color) = Self::bar_cell(kind, theme, row, rows, level, peak);
              buf[(x, bottom - 1 - row)].set_char(glyph).set_fg(color).set_bg(theme.panel_bg);
            }
          }
          BarKind::Mirror => {
            for row in 0..half {
              let (glyph, color) = Self::bar_cell(kind, theme, row, half, level, peak);
              buf[(x, bottom - half - 1 - row)].set_char(glyph).set_fg(color).set_bg(theme.panel_bg);
              // The lower half inverts fg and bg so a partial cell reads as a
              // solid bar without a second glyph set.
              let cell = &mut buf[(x, bottom - half + row)];
              match (glyph, units(level, row)) {
                (' ', _) => cell.set_char(' ').set_fg(theme.panel_bg).set_bg(theme.panel_bg),
                ('▔', _) => cell.set_char('▁').set_fg(color).set_bg(theme.panel_bg),
                (_, 8) => cell.set_char('█').set_fg(color).set_bg(theme.panel_bg),
                (_, units) => cell.set_char(BLOCKS[8 - units]).set_fg(theme.panel_bg).set_bg(color),
              };
            }
            if rows < height {
              buf[(x, body.y)].set_char(' ').set_fg(theme.panel_bg).set_bg(theme.panel_bg);
            }
          }
          BarKind::Dots | BarKind::Squares => {
            // Squares keeps the ladder but fills the whole cell and colors it
            // from the gradient rather than the flat zones, which reads as a
            // contribution graph instead of a bar chart.
            let square = kind == BarKind::Squares;
            let lit = level.round() as u16;
            let peak_row = (peak.round() as u16).checked_sub(1);
            for row in 0..rows {
              let cell = &mut buf[(x, bottom - 1 - row)];
              if row < lit || peak_row == Some(row) {
                let color = if square {
                  spectrum_gradient(theme, row as f32 / rows.saturating_sub(1).max(1) as f32)
                } else {
                  zone(theme, row, rows)
                };
                cell.set_char(if square { '█' } else { '●' }).set_fg(color);
              } else {
                cell.set_char('·').set_fg(theme.border);
              }
              cell.set_bg(theme.panel_bg);
            }
          }
        }
      }
    }
  }

  /// Glyph and color of one cell in a vertical bar, including the peak marker.
  pub(super) fn bar_cell(kind: BarKind, theme: &Theme, row: u16, rows: u16, level: f32, peak: f32) -> (char, Color) {
    let units = units(level, row);
    let glyph =
      if units == 0 && peak > 0.05 && row == (peak.ceil() as u16).saturating_sub(1) { '▔' } else { BLOCKS[units] };
    let color = match kind {
      BarKind::Gradient => spectrum_gradient(theme, row as f32 / rows.saturating_sub(1).max(1) as f32),
      BarKind::Mono if glyph == '▔' => theme.fg,
      BarKind::Mono => theme.accent,
      _ => zone(theme, row, rows),
    };
    (glyph, color)
  }
}
