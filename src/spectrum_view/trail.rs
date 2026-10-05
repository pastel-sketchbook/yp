// Derived from vtamp (MIT, (c) 2026 Jang-Ho Hwang and vtamp contributors).
// See NOTICE at the repository root for the full licence text.
//! The most recent frame tops; old frames fade and new frames win collisions.
//!
//! Reuses the waterfall's frame history, so this costs no extra state: a
//! transient leaves a visible streak that decays over the trail length. Older
//! frames blend toward the panel background rather than snapping to one dim
//! color, which is what makes the tail read as fading instead of switching off.

use super::bars::zone;
use super::geometry::{bar_layout, merged};
use crate::spectrum::BANDS;
use crate::theme::{Theme, blend};
use ratatui::{buffer::Buffer, layout::Rect};
use std::collections::VecDeque;

/// Recent frames drawn behind the bars.
const LENGTH: usize = 6;

pub(super) fn draw(buf: &mut Buffer, body: Rect, theme: &Theme, history: &VecDeque<[f32; BANDS]>) {
  if body.height == 0 {
    return;
  }
  // Oldest first, so the newest frame lands on top where they collide.
  for (age, levels) in history.iter().rev().take(LENGTH).enumerate().rev() {
    for bar in bar_layout(body.width) {
      let level = merged(levels, bar.bands);
      if level <= 0.0 {
        continue;
      }
      let row = ((level * f32::from(body.height)).ceil() as u16).saturating_sub(1).min(body.height - 1);
      let color = blend(zone(theme, row, body.height), theme.panel_bg, age as f32 / LENGTH as f32);
      for x in bar.columns {
        buf[(body.x + x, body.bottom() - 1 - row)].set_char('▄').set_fg(color).set_bg(theme.panel_bg);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::theme::THEMES;

  fn history(levels: &[[f32; BANDS]]) -> VecDeque<[f32; BANDS]> {
    levels.iter().copied().collect()
  }

  #[test]
  fn older_frames_fade_toward_the_panel_and_the_newest_keeps_its_zone() {
    let body = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(body);
    let frames = history(&[[0.2; BANDS], [1.0; BANDS]]);
    draw(&mut buf, body, &THEMES[0], &frames);
    let x = body.x + bar_layout(body.width).next().expect("a bar").columns.start;
    // Each frame marks the row its level reaches, counted up from the body.
    let row_of = |level: f32| ((level * f32::from(body.height)).ceil() as u16).saturating_sub(1);
    let (newest_row, older_row) = (row_of(1.0), row_of(0.2));
    let newest = buf[(x, body.bottom() - 1 - newest_row)].clone();
    let older = buf[(x, body.bottom() - 1 - older_row)].clone();
    assert_eq!(newest.symbol(), "▄", "the newest frame");
    assert_eq!(older.symbol(), "▄", "the older frame trails below it");
    assert!(
      (0..body.height).all(|y| y == body.bottom() - 1 - newest_row
        || y == body.bottom() - 1 - older_row
        || buf[(x, y)].symbol() != "▄"),
      "nothing is drawn between the two frames"
    );
    assert_eq!(newest.fg, zone(&THEMES[0], newest_row, body.height), "the newest frame keeps a zone color");
    assert_ne!(older.fg, newest.fg, "an older frame must be dimmer");
    // Fading means blending toward the panel, not jumping to a fixed dim role.
    let distance = |color: ratatui::style::Color| {
      let to_panel = |c| {
        let [r, g, b] = crate::theme::channels(c).unwrap_or([0; 3]);
        i32::from(r) + i32::from(g) + i32::from(b)
      };
      (to_panel(color) - to_panel(THEMES[0].panel_bg)).abs()
    };
    assert!(distance(older.fg) < distance(newest.fg), "older frames sit closer to the panel");
  }

  #[test]
  fn silence_leaves_no_marks_and_the_newest_frame_wins_a_collision() {
    let body = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(body);
    draw(&mut buf, body, &THEMES[0], &history(&[[0.0; BANDS]]));
    assert!(buf.content().iter().all(|cell| cell.symbol() != "▄"), "a silent frame must not draw a trail mark");
    // Two frames at the same height: the newer one owns the row.
    let mut buf = Buffer::empty(body);
    draw(&mut buf, body, &THEMES[0], &history(&[[0.5; BANDS], [0.5; BANDS]]));
    let x = body.x + bar_layout(body.width).next().expect("a bar").columns.start;
    let row = ((0.5 * f32::from(body.height)).ceil() as u16).saturating_sub(1);
    assert_eq!(
      buf[(x, body.bottom() - 1 - row)].fg,
      zone(&THEMES[0], row, body.height),
      "the newest frame is drawn last"
    );
  }

  #[test]
  fn a_full_height_frame_stays_inside_the_body() {
    // The row comes from rounding the level, so the tallest frame is the one
    // that could land a row below the body.
    for height in [1_u16, 2, 3, 9, 12] {
      let body = Rect::new(0, 0, 40, height);
      let mut buf = Buffer::empty(body);
      draw(&mut buf, body, &THEMES[0], &history(&[[1.0; BANDS]]));
      for y in 0..body.y {
        assert!(
          (0..body.width).all(|x| buf[(x, y)].symbol() != "▄"),
          "height {height}: a trail mark escaped above the body at row {y}"
        );
      }
      for y in body.bottom()..buf.area.height {
        assert!(
          (0..body.width).all(|x| buf[(x, y)].symbol() != "▄"),
          "height {height}: a trail mark escaped below the body at row {y}"
        );
      }
    }
  }
}
