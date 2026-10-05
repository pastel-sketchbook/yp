// Derived from vtamp (MIT, (c) 2026 Jang-Ho Hwang and vtamp contributors).
// See NOTICE at the repository root for the full licence text.
//! Frequency labels positioned against the actual graph geometry.
//!
//! A decade mark has to land under the column that actually shows that
//! frequency. For the bar styles that column is the middle of a bar group, not
//! the geometric position, and for the styles that draw a continuous curve it is
//! simply proportional. Getting this wrong puts "1k" beside a bar that is mostly
//! 4 kHz, which is worse than no label at all.

use super::geometry::bar_layout;
use crate::spectrum::{BANDS, SpectrumFrame};
use crate::theme::Theme;
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

/// How a frequency ratio maps onto a column of the graph.
#[derive(Clone, Copy, Debug)]
pub(super) enum Mapping {
  /// Bars carry bands in groups, so snap to the middle of the group.
  Bars,
  /// Every column is its own interpolated position.
  Continuous,
  /// The figure wraps or stacks, so only the ends of the scale mean anything.
  Ends,
}

/// Narrowest graph that carries decade labels; below it the ends read better.
const MIN_WIDTH: u16 = 12;

/// Column of `graph` showing the frequency at `ratio` of the scale.
fn position(graph: Rect, mapping: Mapping, ratio: f32) -> u16 {
  let offset = match mapping {
    Mapping::Bars => {
      let band = ((ratio * BANDS as f32) as usize).min(BANDS - 1);
      bar_layout(graph.width)
        .find(|bar| bar.bands.contains(&band))
        .map_or(0, |bar| (bar.columns.start + bar.columns.end - 1) / 2)
    }
    _ => (ratio * f32::from(graph.width) - 0.5).round().clamp(0.0, f32::from(graph.width.saturating_sub(1))) as u16,
  };
  graph.x + offset
}

pub(super) fn draw(
  buf: &mut Buffer,
  graph: Rect,
  y: u16,
  theme: &Theme,
  mapping: Mapping,
  frame: Option<&SpectrumFrame>,
) {
  if graph.width == 0 {
    return;
  }
  let style = Style::default().fg(theme.muted).bg(theme.panel_bg);
  // A scale that is not a usable range gets no labels rather than wrong ones.
  let scale = frame.filter(|f| f.low_hz.is_finite() && f.high_hz.is_finite() && f.low_hz > 0.0 && f.high_hz > f.low_hz);
  let mut end = None;
  if graph.width >= MIN_WIDTH
    && !matches!(mapping, Mapping::Ends)
    && let Some(frame) = scale
  {
    let span = (frame.high_hz / frame.low_hz).ln();
    for (hz, label) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
      if hz < frame.low_hz || hz > frame.high_hz {
        continue;
      }
      let ratio = (hz / frame.low_hz).ln() / span;
      let width = label.len() as u16;
      // Centred on the mark, then kept whole inside the graph.
      let x = position(graph, mapping, ratio).saturating_sub(width / 2).clamp(graph.x, graph.right() - width);
      // Two labels wide enough to touch would run together into one word, which
      // is worse than dropping the inner one.
      if end.is_some_and(|end| x <= end) {
        continue;
      }
      buf.set_string(x, y, label, style);
      end = Some(x + width);
    }
  }
  if end.is_none() {
    buf.set_stringn(graph.x, y, "LOW", usize::from(graph.width), style);
    if graph.width >= 9 {
      buf.set_string(graph.right() - 4, y, "HIGH", style);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::theme::THEMES;

  fn render(width: u16, mapping: Mapping, low: f32, high: f32) -> String {
    let mut buf = Buffer::empty(Rect::new(0, 0, width + 3, 2));
    draw(
      &mut buf,
      Rect::new(3, 0, width, 1),
      1,
      &THEMES[0],
      mapping,
      Some(&SpectrumFrame { low_hz: low, high_hz: high, ..SpectrumFrame::default() }),
    );
    (0..width + 3).map(|x| buf[(x, 1)].symbol()).collect()
  }

  #[test]
  fn labels_follow_the_scale_and_never_run_together() {
    let row = render(60, Mapping::Bars, 40.0, 16_000.0);
    assert!(row.contains("100") && row.contains("1k") && row.contains("10k"), "{row}");
    // Any width that fits the labels must show them as whole words.
    for width in MIN_WIDTH..100 {
      let row = render(width, Mapping::Continuous, 40.0, 16_000.0);
      assert!(
        row.split_whitespace().all(|word| ["100", "1k", "10k"].contains(&word)),
        "width {width} merged the labels: {row:?}"
      );
    }
    assert!(!render(60, Mapping::Continuous, 40.0, 8000.0).contains("10k"), "10k is above the scale");
  }

  #[test]
  fn an_unusable_scale_or_a_wrapped_figure_falls_back_to_the_ends() {
    for (width, mapping, low, high) in [
      (11, Mapping::Bars, 40.0, 16_000.0),
      (60, Mapping::Ends, 40.0, 16_000.0),
      (60, Mapping::Bars, 0.0, 0.0),
      (60, Mapping::Bars, f32::NAN, 16_000.0),
      (60, Mapping::Bars, 1000.0, 40.0),
      (60, Mapping::Bars, f32::INFINITY, 16_000.0),
    ] {
      let row = render(width, mapping, low, high);
      assert!(row.contains("LOW") && row.contains("HIGH"), "{width} {mapping:?} {low}..{high}: {row:?}");
    }
  }

  #[test]
  fn bar_marks_snap_to_the_group_showing_the_frequency() {
    for width in [12_u16, 21, 64, 150] {
      let graph = Rect::new(5, 0, width, 1);
      for bar in bar_layout(width) {
        let band_center = (bar.bands.start + bar.bands.end) as f32 / 2.0;
        let x = position(graph, Mapping::Bars, band_center / BANDS as f32);
        assert!(bar.columns.contains(&(x - graph.x)), "width {width}: {x} is not inside {:?}", bar.columns);
      }
    }
  }

  #[test]
  fn labels_stay_inside_the_graph_they_are_given() {
    // The graph can be inset, as it is under the stereo channel labels.
    for inset in 0..4_u16 {
      let width = 20;
      let graph = Rect::new(inset, 0, width, 1);
      let mut buf = Buffer::empty(Rect::new(0, 0, 40, 2));
      draw(
        &mut buf,
        graph,
        1,
        &THEMES[0],
        Mapping::Bars,
        Some(&SpectrumFrame { low_hz: 40.0, high_hz: 16_000.0, ..SpectrumFrame::default() }),
      );
      for x in graph.x..graph.right() {
        let symbol = buf[(x, 1)].symbol().to_string();
        if symbol != " " {
          assert!(
            !symbol.is_empty() && x >= graph.x && x + symbol.chars().count() as u16 <= graph.right(),
            "inset {inset}: {symbol:?} at {x} escaped {graph:?}"
          );
        }
      }
    }
  }
}
