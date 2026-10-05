// Derived from vtamp (MIT, (c) 2026 Jang-Ho Hwang and vtamp contributors).
// See NOTICE at the repository root for the full licence text.
//! Independent L/R envelopes, mirrored about the middle with channel labels.
//!
//! The two halves are labelled, because an upper-lower mirror is otherwise
//! ambiguous: there is nothing in the shape that says which side is the left
//! channel. The labels take the first two columns, so the graph is inset and the
//! frequency axis follows it rather than the pane.

use super::bars::{BLOCKS, units, zone};
use super::geometry::{bar_layout, merged};
use crate::spectrum::BANDS;
use crate::spectrum::SpectrumChannels;
use crate::theme::Theme;
use ratatui::{buffer::Buffer, layout::Rect};

/// Columns taken by the L and R labels.
const GUTTER: u16 = 2;
/// Bar fall per second, so the two envelopes read at the same rate as the bars.
const DECAY: f32 = 1.8;

/// Per-channel envelopes, decayed independently of the combined levels.
#[derive(Default)]
pub(super) struct Stereo {
  left: [f32; BANDS],
  right: [f32; BANDS],
}

impl Stereo {
  pub(super) fn reset(&mut self) {
    *self = Self::default();
  }

  /// True while either envelope still has height to fall from.
  pub(super) fn is_active(&self) -> bool {
    self.left.iter().chain(&self.right).any(|level| *level > 0.0)
  }

  /// Steps both envelopes toward `target`, or toward silence when it is gone.
  pub(super) fn advance(&mut self, target: Option<&SpectrumChannels>, dt: f32) {
    for band in 0..BANDS {
      self.left[band] =
        target.map_or(0.0, |channels| channels.left[band].clamp(0.0, 1.0)).max(self.left[band] - dt * DECAY);
      self.right[band] =
        target.map_or(0.0, |channels| channels.right[band].clamp(0.0, 1.0)).max(self.right[band] - dt * DECAY);
    }
  }

  pub(super) fn draw(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    let graph = graph(body);
    let height = graph.height & !1;
    if height == 0 {
      return;
    }
    let half = height / 2;
    let middle = graph.bottom() - half;
    // The labels sit in the gutter, one per half, and name their channel.
    buf[(body.x, middle - 1)].set_char('L').set_fg(theme.muted).set_bg(theme.panel_bg);
    buf[(body.x, middle)].set_char('R').set_fg(theme.muted).set_bg(theme.panel_bg);
    for bar in bar_layout(graph.width) {
      let left = merged(&self.left, bar.bands.clone()) * f32::from(half);
      let right = merged(&self.right, bar.bands) * f32::from(half);
      for x in bar.columns {
        for row in 0..half {
          let color = zone(theme, row, half);
          let upper = units(left, row);
          buf[(graph.x + x, middle - 1 - row)]
            .set_char(BLOCKS[upper])
            .set_fg(if upper == 0 { theme.panel_bg } else { color })
            .set_bg(theme.panel_bg);
          // The lower half inverts fg and bg so a partial cell reads as solid,
          // matching the mirror style.
          let lower = units(right, row);
          let cell = &mut buf[(graph.x + x, middle + row)];
          match lower {
            0 => cell.set_char(' ').set_fg(theme.panel_bg).set_bg(theme.panel_bg),
            8 => cell.set_char('█').set_fg(color).set_bg(theme.panel_bg),
            _ => cell.set_char(BLOCKS[8 - lower]).set_fg(theme.panel_bg).set_bg(color),
          };
        }
      }
    }
  }
}

/// Whether there is room for two labelled halves rather than a stub.
pub(super) fn fits(body: Rect) -> bool {
  body.width >= 10 && body.height >= 4
}

/// The graph area, inset past the label gutter.
pub(super) fn graph(body: Rect) -> Rect {
  Rect::new(body.x + body.width.min(GUTTER), body.y, body.width.saturating_sub(GUTTER), body.height)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::theme::THEMES;

  fn first_column(body: Rect) -> u16 {
    graph(body).x + bar_layout(graph(body).width).next().expect("a bar").columns.start
  }

  #[test]
  fn labels_orientation_and_partial_blocks_are_all_readable() {
    let body = Rect::new(0, 0, 14, 8);
    let mut view = Stereo::default();
    let mut buf = Buffer::empty(body);
    view.advance(Some(&SpectrumChannels { left: [0.375; BANDS], right: [0.375; BANDS] }), 0.0);
    view.draw(&mut buf, body, &THEMES[0]);
    let x = first_column(body);
    assert_eq!(buf[(0, 3)].symbol(), "L", "the upper half is the left channel");
    assert_eq!(buf[(0, 4)].symbol(), "R", "the lower half is the right channel");
    assert_eq!(buf[(x, 2)].symbol(), "▄");
    assert_eq!(buf[(x, 5)].symbol(), "▄");
    // The lower half fills with the color the upper one draws in.
    assert_eq!(buf[(x, 2)].fg, buf[(x, 5)].bg);
    assert_eq!(buf[(x, 2)].bg, buf[(x, 5)].fg);
  }

  #[test]
  fn a_wide_image_opens_and_a_mono_one_closes() {
    let body = Rect::new(0, 0, 40, 12);
    let x = first_column(body);
    let graph = graph(body);
    let middle = graph.bottom() - (graph.height & !1) / 2;

    // A fresh view per case: an envelope holds its level rather than dropping to
    // a new one instantly, so reusing one would not be a fair comparison.
    let mut wide = Stereo::default();
    wide.advance(Some(&SpectrumChannels { left: [1.0; BANDS], right: [0.0; BANDS] }), 0.0);
    let mut buf = Buffer::empty(body);
    wide.draw(&mut buf, body, &THEMES[0]);
    assert_eq!(buf[(x, graph.y)].symbol(), "█", "a loud left channel fills the upper half");
    assert_eq!(buf[(x, graph.bottom() - 1)].symbol(), " ", "a silent right channel leaves the lower half empty");

    let mut mono = Stereo::default();
    mono.advance(Some(&SpectrumChannels { left: [0.7; BANDS], right: [0.7; BANDS] }), 0.0);
    let mut buf = Buffer::empty(body);
    mono.draw(&mut buf, body, &THEMES[0]);
    // The cells differ because a partial lower cell is painted inverted, so the
    // claim is that the two halves cover the same number of rows.
    let filled = |from: u16, to: u16| (from..to).filter(|y| buf[(x, *y)].symbol() != " ").count();
    assert!(filled(graph.y, middle - 1) > 0, "the upper half draws");
    assert_eq!(filled(graph.y, middle - 1), filled(middle + 1, graph.bottom()), "a mono source must be symmetric");
  }

  #[test]
  fn an_envelope_falls_over_time_rather_than_jumping_down() {
    // A band that drops from loud to quiet must still be seen falling, or the
    // meters would flicker instead of decaying.
    let body = Rect::new(0, 0, 40, 12);
    let mut view = Stereo::default();
    view.advance(Some(&SpectrumChannels { left: [1.0; BANDS], right: [1.0; BANDS] }), 0.0);
    assert_eq!(view.left, [1.0; BANDS]);
    view.advance(Some(&SpectrumChannels { left: [0.1; BANDS], right: [0.1; BANDS] }), 0.0);
    assert_eq!(view.left, [1.0; BANDS], "no time passed, so nothing fell");
    view.advance(Some(&SpectrumChannels { left: [0.1; BANDS], right: [0.1; BANDS] }), 0.2);
    assert!(view.left[0] < 1.0 && view.left[0] > 0.1, "it falls toward the new level, not onto it");
    let mut buf = Buffer::empty(body);
    view.draw(&mut buf, body, &THEMES[0]);
    let x = first_column(body);
    let graph = graph(body);
    assert_eq!(buf[(x, graph.y)].symbol(), " ", "a decaying envelope no longer fills the top row");
  }

  #[test]
  fn the_envelopes_fall_independently_and_then_sleep() {
    let mut view = Stereo::default();
    view.advance(Some(&SpectrumChannels { left: [0.8; BANDS], right: [0.0; BANDS] }), 0.3);
    assert_eq!(view.right, [0.0; BANDS], "a channel that was never loud has nothing to fall");
    assert_eq!(view.left, [0.8; BANDS]);
    view.advance(None, 1.0);
    assert!(!view.is_active(), "silence must stop the animation");
    assert_eq!(view.left, [0.0; BANDS]);
  }

  #[test]
  fn resetting_clears_both_envelopes() {
    let mut view = Stereo::default();
    view.advance(Some(&SpectrumChannels { left: [1.0; BANDS], right: [1.0; BANDS] }), 0.0);
    assert!(view.is_active());
    view.reset();
    assert!(!view.is_active());
  }

  #[test]
  fn a_pane_too_small_for_labels_is_declined_rather_than_drawn() {
    for (width, height) in [(9_u16, 8_u16), (14, 3), (4, 2), (1, 1), (0, 0)] {
      let body = Rect::new(0, 0, width, height);
      assert!(!fits(body), "{width}x{height} cannot hold two labelled halves");
      // Declining means the caller draws bars instead, so nothing here runs.
      let mut view = Stereo::default();
      view.advance(Some(&SpectrumChannels { left: [0.9; BANDS], right: [0.9; BANDS] }), 0.0);
      let mut buf = Buffer::empty(Rect::new(0, 0, width.max(1), height.max(1)));
      view.draw(&mut buf, body, &THEMES[0]);
    }
  }
}
