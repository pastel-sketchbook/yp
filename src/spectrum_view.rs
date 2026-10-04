//! Drawing for the spectrum display.
//!
//! Pure rendering plus the interpolation state that smooths the bars between
//! analyzer frames: levels decay toward the newest data and peak markers fall
//! behind them, so the display animates continuously instead of stepping.

use ratatui::{
  Frame,
  buffer::Buffer,
  layout::Rect,
  style::{Color, Style},
  text::Line,
  widgets::{Block, Borders, Paragraph},
};

use crate::{
  spectrum::{BANDS, SpectrumFrame},
  theme::{Theme, spectrum_gradient},
};

/// Eighth-height blocks, used for bars and their partial top cells.
const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Waterfall rows remembered, more than any pane shows.
const HISTORY: usize = 256;
/// How fast bars fall toward their target, in levels per second.
const DECAY: f32 = 1.8;
/// How fast an expired peak marker falls.
const PEAK_DECAY: f32 = 0.8;
/// How long a peak marker holds before it starts falling.
const PEAK_HOLD: std::time::Duration = std::time::Duration::from_millis(180);
/// A frame older than this is treated as stale, so bars fall on a stalled read.
const LIVENESS: std::time::Duration = std::time::Duration::from_millis(300);

/// How the spectrum is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpectrumStyle {
  /// Continuous gradient across bar heights.
  #[default]
  Gradient,
  /// Flat bars colored by height zone.
  Bars,
  /// Single accent color.
  Mono,
  /// Bars mirrored about the vertical center.
  Mirror,
  /// Ladder of lit dots.
  Dots,
  /// Scrolling history of recent frames.
  Waterfall,
}

impl SpectrumStyle {
  /// Cycle order, starting from the [`SpectrumStyle::default`] so the first
  /// `Ctrl+V` press moves to the next style rather than back to the default.
  pub const ALL: [SpectrumStyle; 6] = [
    SpectrumStyle::Gradient,
    SpectrumStyle::Bars,
    SpectrumStyle::Mono,
    SpectrumStyle::Mirror,
    SpectrumStyle::Dots,
    SpectrumStyle::Waterfall,
  ];

  /// Next style in the cycle.
  pub fn next(self) -> Self {
    let index = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
    Self::ALL[(index + 1) % Self::ALL.len()]
  }

  pub fn id(self) -> &'static str {
    match self {
      SpectrumStyle::Gradient => "gradient",
      SpectrumStyle::Bars => "bars",
      SpectrumStyle::Mono => "mono",
      SpectrumStyle::Mirror => "mirror",
      SpectrumStyle::Dots => "dots",
      SpectrumStyle::Waterfall => "waterfall",
    }
  }

  /// Parses a persisted style name, falling back to the default.
  pub fn from_id(id: &str) -> Self {
    Self::ALL.into_iter().find(|s| s.id() == id).unwrap_or_default()
  }

  /// True when the style keeps animating on its own rather than only on new data.
  fn animates_between_frames(self) -> bool {
    self != SpectrumStyle::Waterfall
  }
}

pub struct SpectrumView {
  style: SpectrumStyle,
  frame: Option<SpectrumFrame>,
  received: std::time::Instant,
  updated: std::time::Instant,
  levels: [f32; BANDS],
  peaks: [f32; BANDS],
  hold: [std::time::Instant; BANDS],
  /// Levels of recent active frames, oldest first; the waterfall draws these.
  history: std::collections::VecDeque<[f32; BANDS]>,
  redraw: bool,
}

impl SpectrumView {
  pub fn new(style: SpectrumStyle) -> Self {
    let now = std::time::Instant::now();
    Self {
      style,
      frame: None,
      received: now,
      updated: now,
      levels: [0.0; BANDS],
      peaks: [0.0; BANDS],
      hold: [now; BANDS],
      history: std::collections::VecDeque::with_capacity(HISTORY),
      redraw: true,
    }
  }

  /// Switches rendering only; levels, peaks, and history carry over.
  pub fn set_style(&mut self, style: SpectrumStyle) {
    self.style = style;
    self.redraw = true;
  }

  /// Forgets the current stream, used when playback stops.
  pub fn clear(&mut self) {
    self.frame = None;
    self.history.clear();
    self.reset_levels();
  }

  fn reset_levels(&mut self) {
    self.levels.fill(0.0);
    self.peaks.fill(0.0);
    self.redraw = true;
  }

  /// True while the display still has something to animate.
  pub fn needs_animation(&self) -> bool {
    self.redraw || (self.style.animates_between_frames() && self.levels.iter().chain(&self.peaks).any(|v| *v > 0.0))
  }

  /// Adopts a new analyzer frame, resetting state when the track changes.
  pub fn accept(&mut self, frame: SpectrumFrame) {
    let previous = self.frame.as_ref();
    let new_track = previous.is_some_and(|p| p.current_id != frame.current_id);
    let new_generation = previous.is_some_and(|p| p.generation != frame.generation);
    if new_track {
      self.frame = None;
      self.history.clear();
      self.reset_levels();
    } else if new_generation {
      self.reset_levels();
    }
    if frame.active {
      if self.history.len() == HISTORY {
        self.history.pop_front();
      }
      self.history.push_back(frame.levels);
      self.redraw |= self.style == SpectrumStyle::Waterfall;
    }
    self.frame = Some(frame);
    self.received = std::time::Instant::now();
    self.redraw = true;
  }

  /// Advances decay and peak hold, then draws the body.
  pub fn draw(&mut self, frame: &mut Frame, area: Rect, theme: &Theme, playing: bool) {
    self.redraw = false;
    let inner = self.header(frame, area, theme);
    if inner.width == 0 || inner.height == 0 {
      return;
    }
    let body = Rect { height: inner.height.saturating_sub(1), ..inner };
    if self.style == SpectrumStyle::Waterfall {
      self.draw_waterfall(frame.buffer_mut(), body, theme);
    } else {
      self.advance(playing);
      self.draw_bars(frame.buffer_mut(), body, theme);
    }
    frame.render_widget(
      Paragraph::new("LOW").style(Style::default().fg(theme.muted)),
      Rect::new(inner.x, inner.y + body.height, inner.width.min(3), 1),
    );
    if inner.width >= 9 {
      frame.render_widget(
        Paragraph::new("HIGH").style(Style::default().fg(theme.muted)),
        Rect::new(inner.right() - 4, inner.y + body.height, 4, 1),
      );
    }
  }

  fn header(&self, frame: &mut Frame, area: Rect, theme: &Theme) -> Rect {
    let name = self.style.id();
    let full = format!(" SPECTRUM · {name} ");
    let block = Block::default()
      .borders(Borders::ALL)
      .title(Line::styled(full, Style::default().fg(theme.muted)))
      .border_style(Style::default().fg(theme.border))
      .style(Style::default().bg(theme.panel_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
  }

  /// Bar decay and peak hold, shared by every style with falling peaks.
  fn advance(&mut self, playing: bool) {
    let now = std::time::Instant::now();
    let dt = now.duration_since(self.updated).as_secs_f32().min(0.2);
    self.updated = now;
    let live = playing && self.received.elapsed() < LIVENESS;
    for i in 0..BANDS {
      let target = self.frame.as_ref().filter(|f| live && f.active).map_or(0.0, |f| f.levels[i].clamp(0.0, 1.0));
      self.levels[i] = target.max(self.levels[i] - dt * DECAY);
      if self.levels[i] >= self.peaks[i] {
        self.peaks[i] = self.levels[i];
        self.hold[i] = now + PEAK_HOLD;
      } else if now >= self.hold[i] {
        self.peaks[i] = self.levels[i].max(self.peaks[i] - dt * PEAK_DECAY);
      }
    }
  }

  fn draw_bars(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    let height = body.height;
    if height == 0 || body.width == 0 {
      return;
    }
    // One blank column between bars keeps them visually separate.
    let count = BANDS.min((body.width as usize).div_ceil(2)).max(1);
    let step = (body.width as usize + 1) / count;
    let width = step.saturating_sub(1).max(1);
    let offset = (body.width as usize - (count * step - (step - width))) / 2;
    let rows = if self.style == SpectrumStyle::Mirror { height & !1 } else { height };
    if rows == 0 {
      return;
    }
    let half = rows / 2;
    let scale = f32::from(if self.style == SpectrumStyle::Mirror { half } else { rows });
    let bottom = body.y + height;
    for bar in 0..count {
      // Narrow panes merge several bands into one bar rather than clipping.
      let start = bar * BANDS / count;
      let end = ((bar + 1) * BANDS / count).max(start + 1);
      let level = self.levels[start..end].iter().copied().fold(0.0, f32::max) * scale;
      let peak = self.peaks[start..end].iter().copied().fold(0.0, f32::max) * scale;
      for col in 0..width {
        let x = body.x + (offset + bar * step + col) as u16;
        if x >= body.x + body.width {
          break;
        }
        match self.style {
          SpectrumStyle::Mirror => {
            for row in 0..half {
              let (glyph, color) = self.bar_cell(theme, row, half, level, peak);
              buf[(x, bottom - half - 1 - row)].set_char(glyph).set_fg(color).set_bg(theme.panel_bg);
              // The lower half inverts fg and bg so a partial cell reads as a
              // solid bar without a second glyph set.
              let cell = &mut buf[(x, bottom - half + row)];
              match (glyph, Self::units(level, row)) {
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
          SpectrumStyle::Dots => {
            let lit = level.round() as u16;
            let peak_row = (peak.round() as u16).checked_sub(1);
            for row in 0..rows {
              let cell = &mut buf[(x, bottom - 1 - row)];
              if row < lit || peak_row == Some(row) {
                cell.set_char('●').set_fg(Self::zone(theme, row, rows));
              } else {
                cell.set_char('·').set_fg(theme.border);
              }
              cell.set_bg(theme.panel_bg);
            }
          }
          _ => {
            for row in 0..rows {
              let (glyph, color) = self.bar_cell(theme, row, rows, level, peak);
              buf[(x, bottom - 1 - row)].set_char(glyph).set_fg(color).set_bg(theme.panel_bg);
            }
          }
        }
      }
    }
  }

  /// Eighths of the cell at `row` covered by a bar of `level` rows.
  fn units(level: f32, row: u16) -> usize {
    ((level - row as f32) * 8.0).ceil().clamp(0.0, 8.0) as usize
  }

  /// Flat color for a row, matching the gradient's three zones.
  fn zone(theme: &Theme, row: u16, rows: u16) -> Color {
    let position = row as f32 / rows.max(1) as f32;
    if position < 0.55 {
      theme.spectrum[0]
    } else if position < 0.8 {
      theme.spectrum[1]
    } else {
      theme.spectrum[2]
    }
  }

  /// Glyph and color of one cell in a vertical bar, including the peak marker.
  fn bar_cell(&self, theme: &Theme, row: u16, rows: u16, level: f32, peak: f32) -> (char, Color) {
    let units = Self::units(level, row);
    let glyph =
      if units == 0 && peak > 0.05 && row == (peak.ceil() as u16).saturating_sub(1) { '▔' } else { BLOCKS[units] };
    let color = match self.style {
      SpectrumStyle::Gradient => spectrum_gradient(theme, row as f32 / rows.saturating_sub(1).max(1) as f32),
      SpectrumStyle::Mono if glyph == '▔' => theme.fg,
      SpectrumStyle::Mono => theme.accent,
      _ => Self::zone(theme, row, rows),
    };
    (glyph, color)
  }

  fn draw_waterfall(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
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
        let start = column * BANDS / width;
        let end = ((column + 1) * BANDS / width).max(start + 1);
        let level = levels.map_or(0.0, |levels| levels[start..end].iter().copied().fold(0.0, f32::max)).clamp(0.0, 1.0);
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::theme::THEMES;
  use ratatui::{Terminal, backend::TestBackend};

  fn theme() -> Theme {
    THEMES[0]
  }

  fn backend(width: u16, height: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(width, height)).unwrap()
  }

  fn active(levels: [f32; BANDS], id: Option<&str>) -> SpectrumFrame {
    SpectrumFrame { active: true, levels, current_id: id.map(str::to_owned), ..SpectrumFrame::default() }
  }

  fn render(view: &mut SpectrumView, terminal: &mut Terminal<TestBackend>, playing: bool) {
    terminal.draw(|f| view.draw(f, f.area(), &theme(), playing)).unwrap();
  }

  fn buffer(terminal: &Terminal<TestBackend>) -> Buffer {
    terminal.backend().buffer().clone()
  }

  fn glyph_at(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> String {
    terminal.backend().buffer()[(x, y)].symbol().to_string()
  }

  /// Lets the next draw see a full decay step with peak holds expired.
  fn age(view: &mut SpectrumView) {
    view.updated = std::time::Instant::now() - std::time::Duration::from_millis(200);
    let past = std::time::Instant::now() - std::time::Duration::from_secs(1);
    view.hold.fill(past);
  }

  #[test]
  fn styles_cycle_through_every_style_and_round_trip_by_name() {
    assert_eq!(SpectrumStyle::default(), SpectrumStyle::Gradient, "gradient is the default");
    assert_eq!(SpectrumStyle::ALL[0], SpectrumStyle::default(), "the cycle starts at the default");
    let mut style = SpectrumStyle::default();
    let mut seen = vec![style];
    for _ in 0..SpectrumStyle::ALL.len() - 1 {
      style = style.next();
      assert!(!seen.contains(&style), "cycle repeated {style:?}");
      seen.push(style);
    }
    assert_eq!(seen.len(), SpectrumStyle::ALL.len());
    assert_eq!(style.next(), SpectrumStyle::default(), "the cycle must wrap back to the default");
    for style in SpectrumStyle::ALL {
      assert_eq!(SpectrumStyle::from_id(style.id()), style);
      assert!(!style.id().is_empty());
    }
    assert_eq!(SpectrumStyle::from_id("nonsense"), SpectrumStyle::Gradient, "unknown names fall back to the default");
  }

  #[test]
  fn bars_sleep_once_they_decay_and_wake_for_new_audio() {
    let mut view = SpectrumView::new(SpectrumStyle::Bars);
    let mut terminal = backend(40, 12);
    render(&mut view, &mut terminal, false);
    assert!(!view.needs_animation(), "an empty view is idle");
    view.accept(active([1.0; BANDS], None));
    assert!(view.needs_animation(), "new audio must wake the display");
    render(&mut view, &mut terminal, true);
    for _ in 0..20 {
      age(&mut view);
      render(&mut view, &mut terminal, false);
    }
    assert!(!view.needs_animation(), "bars must settle instead of spinning forever");
    view.accept(SpectrumFrame::default());
    assert!(view.needs_animation(), "a pause must let bars fall");
  }

  #[test]
  fn gradient_blends_across_every_row_of_a_full_bar() {
    let theme = theme();
    let mut view = SpectrumView::new(SpectrumStyle::Gradient);
    let mut terminal = backend(40, 12);
    view.accept(active([1.0; BANDS], None));
    render(&mut view, &mut terminal, true);
    // Body rows are y = 1..=9; the border occupies row 0 and the axis row 10.
    let buf = buffer(&terminal);
    let column: Vec<Color> = (1..=9).map(|y| buf[(1, y)].fg).collect();
    assert!((1..=9).all(|y| buf[(1, y)].symbol() == "█"), "a full bar fills every row");
    assert_eq!(column[8], theme.spectrum[0], "the bottom row is the low stop");
    assert_eq!(column[0], theme.spectrum[2], "the top row is the high stop");
    assert!(
      column.iter().any(|c| !theme.spectrum.contains(c)),
      "intermediate rows must blend the stops, got {column:?}"
    );
    let mut distinct = column.clone();
    distinct.dedup();
    assert_eq!(distinct.len(), column.len(), "every row needs its own color: {column:?}");
  }

  #[test]
  fn mono_uses_the_accent_and_marks_peaks_with_text_color() {
    let theme = theme();
    let mut view = SpectrumView::new(SpectrumStyle::Mono);
    let mut terminal = backend(40, 12);
    view.accept(active([0.5; BANDS], None));
    render(&mut view, &mut terminal, true);
    // Spaces must be excluded: an unwritten cell is blank, not a bar.
    let is_bar = |symbol: &str| symbol != " " && symbol.chars().all(|c| BLOCKS.contains(&c));
    let buf = buffer(&terminal);
    assert!(buf.content().iter().any(|c| is_bar(c.symbol())), "bars must be drawn");
    assert!(
      buf.content().iter().filter(|c| is_bar(c.symbol())).all(|c| c.fg == theme.accent),
      "mono bars use the accent color"
    );
    assert!(!buf.content().iter().any(|c| c.symbol() == "▔"), "no peak marker yet");
    // Drop the level so the held peak separates from the bar.
    view.accept(active([0.05; BANDS], None));
    age(&mut view);
    render(&mut view, &mut terminal, true);
    let buf = buffer(&terminal);
    let markers: Vec<Color> = buf.content().iter().filter(|c| c.symbol() == "▔").map(|c| c.fg).collect();
    assert!(!markers.is_empty(), "a falling level must leave a peak marker");
    assert!(markers.iter().all(|c| *c == theme.fg), "peak markers use the text color");
    assert!(
      buf.content().iter().filter(|c| is_bar(c.symbol())).all(|c| c.fg == theme.accent),
      "bars stay on the accent while the marker uses text"
    );
  }

  #[test]
  fn bars_keep_a_blank_column_between_them() {
    let mut view = SpectrumView::new(SpectrumStyle::Bars);
    let mut terminal = backend(61, 12);
    view.accept(active([1.0; BANDS], None));
    render(&mut view, &mut terminal, true);
    // An odd width must still alternate bar, gap, bar...
    for x in 1..60 {
      let expected = if x % 2 == 1 { "█" } else { " " };
      assert_eq!(glyph_at(&terminal, x, 9), expected, "column {x}");
    }
  }

  #[test]
  fn mirror_is_symmetric_and_inverts_partial_lower_cells() {
    let theme = theme();
    let mut view = SpectrumView::new(SpectrumStyle::Mirror);
    let mut terminal = backend(40, 12);
    view.accept(active([0.6; BANDS], None));
    render(&mut view, &mut terminal, true);
    let buf = buffer(&terminal);
    assert_eq!(buf[(1, 1)].symbol(), " ", "an odd top row stays blank");
    for y in [4, 5, 6, 7] {
      assert_eq!(buf[(1, y)].symbol(), "█", "row {y}");
      assert_eq!(buf[(1, y)].bg, theme.panel_bg, "row {y}");
    }
    let upper = &buf[(1, 3)];
    let lower = &buf[(1, 8)];
    assert_eq!(upper.symbol(), "▄", "0.4 of a row is four eighths");
    assert_eq!(upper.bg, theme.panel_bg);
    assert_eq!(lower.symbol(), "▄", "the mirrored cell is painted inverted");
    assert_eq!(lower.fg, theme.panel_bg, "inverted cells paint the canvas color");
    assert_eq!(lower.bg, upper.fg, "inverted cells fill with the bar color");
    assert_eq!(buf[(1, 2)].symbol(), " ");
    assert_eq!(buf[(1, 9)].symbol(), " ");
  }

  #[test]
  fn a_single_body_row_cannot_host_a_mirror() {
    let mut view = SpectrumView::new(SpectrumStyle::Mirror);
    let mut terminal = backend(40, 4);
    view.accept(active([1.0; BANDS], None));
    render(&mut view, &mut terminal, true);
    assert_eq!(glyph_at(&terminal, 1, 1), " ", "nothing is drawn rather than panicking");
  }

  #[test]
  fn dots_light_whole_segments_and_hold_a_peak_dot() {
    let theme = theme();
    let mut view = SpectrumView::new(SpectrumStyle::Dots);
    let mut terminal = backend(40, 12);
    view.accept(active([0.5; BANDS], None));
    render(&mut view, &mut terminal, true);
    let buf = buffer(&terminal);
    // 0.5 × 9 rows rounds to 5 lit segments from the bottom.
    let lit: Vec<u16> = (1..=9).filter(|y| buf[(1, *y)].symbol() == "●").collect();
    assert_eq!(lit, vec![5, 6, 7, 8, 9]);
    assert!((1..=4).all(|y| buf[(1, y)].symbol() == "·" && buf[(1, y)].fg == theme.border), "unlit segments are dim");
    assert_eq!(buf[(1, 9)].fg, theme.spectrum[0], "the lowest segment is the low stop");
    view.accept(active([0.1; BANDS], None));
    age(&mut view);
    render(&mut view, &mut terminal, true);
    let buf = buffer(&terminal);
    let lit: Vec<u16> = (1..=9).filter(|y| buf[(1, *y)].symbol() == "●").collect();
    assert_eq!(lit.len(), 2, "one lit segment plus a held peak dot: {lit:?}");
    assert_eq!(lit[1], 9, "the lit segment stays on the bottom row");
    assert!(lit[0] < 8, "the peak dot floats above the bar");
  }

  #[test]
  fn waterfall_scrolls_per_frame_and_freezes_without_them() {
    let theme = theme();
    let mut view = SpectrumView::new(SpectrumStyle::Waterfall);
    let mut terminal = backend(40, 12);
    render(&mut view, &mut terminal, true);
    assert!(!view.needs_animation(), "an empty waterfall is idle");
    view.accept(active([1.0; BANDS], None));
    assert!(view.needs_animation(), "a frame requests a draw");
    render(&mut view, &mut terminal, true);
    assert!(!view.needs_animation(), "nothing moves until the next frame");
    let buf = buffer(&terminal);
    assert!((1..=38).all(|x| buf[(x, 9)].symbol() == "█"), "the newest row fills the width");
    assert_eq!(buf[(4, 9)].fg, theme.spectrum[2], "full scale uses the top of the gradient");
    assert_eq!(buf[(4, 8)].symbol(), " ", "the row above is still empty");
    view.accept(active([0.3; BANDS], None));
    view.accept(active([0.0; BANDS], None));
    render(&mut view, &mut terminal, true);
    let buf = buffer(&terminal);
    assert_eq!(buf[(4, 9)].symbol(), " ", "silence leaves a blank row");
    assert_eq!(buf[(4, 9)].fg, theme.panel_bg, "blank cells keep a constant color");
    assert_eq!(buf[(4, 8)].symbol(), "▒");
    assert_eq!(buf[(4, 7)].symbol(), "█");
    for _ in 0..5 {
      age(&mut view);
      render(&mut view, &mut terminal, false);
    }
    assert_eq!(glyph_at(&terminal, 4, 7), "█", "pausing freezes the history");
    assert!(!view.needs_animation());
  }

  #[test]
  fn waterfall_keeps_a_bounded_history_and_fills_narrow_panes() {
    let mut view = SpectrumView::new(SpectrumStyle::Waterfall);
    for _ in 0..HISTORY + 10 {
      view.accept(active([0.2; BANDS], None));
    }
    assert_eq!(view.history.len(), HISTORY, "history must not grow without bound");
    view.clear();
    assert!(view.history.is_empty());
    view.accept(active([0.2; BANDS], None));
    // A pane narrower than the band count merges bands instead of clipping them.
    let mut narrow = backend(20, 12);
    render(&mut view, &mut narrow, true);
    let row: Vec<String> = (1..19).map(|x| glyph_at(&narrow, x, 9)).collect();
    assert!(row.iter().all(|s| s == "░"), "every column must be filled: {row:?}");
  }

  #[test]
  fn a_new_track_clears_the_history_but_a_resume_keeps_it() {
    let mut view = SpectrumView::new(SpectrumStyle::Waterfall);
    view.accept(active([0.9; BANDS], Some("first")));
    view.accept(active([0.9; BANDS], Some("first")));
    assert_eq!(view.history.len(), 2);
    // A new generation on the same track is a seek: the past was heard.
    view.accept(SpectrumFrame {
      generation: 1,
      active: true,
      levels: [0.5; BANDS],
      current_id: Some("first".into()),
      ..SpectrumFrame::default()
    });
    view.accept(active([0.5; BANDS], Some("first")));
    assert!(!view.history.is_empty(), "a seek keeps the waterfall");
    view.accept(active([0.9; BANDS], Some("second")));
    assert_eq!(view.history.len(), 1, "a new track starts a fresh history");
    view.clear();
    assert!(view.levels.iter().all(|l| *l == 0.0), "clearing drops the bars");
  }

  #[test]
  fn switching_styles_keeps_levels_and_history() {
    let mut view = SpectrumView::new(SpectrumStyle::Bars);
    let mut terminal = backend(40, 12);
    view.accept(active([0.8; BANDS], None));
    render(&mut view, &mut terminal, true);
    assert!(view.levels.iter().all(|v| *v > 0.0));
    let collected = view.history.len();
    view.set_style(SpectrumStyle::Waterfall);
    assert_eq!(view.style, SpectrumStyle::Waterfall, "set_style must record the new style");
    assert!(view.needs_animation(), "a style change redraws once");
    assert_eq!(view.history.len(), collected, "history collected in bars carries over");
    assert!(view.levels.iter().all(|v| *v > 0.0), "levels carry over");
    render(&mut view, &mut terminal, true);
    view.set_style(SpectrumStyle::Dots);
    assert_eq!(view.history.len(), collected);
    assert!(view.levels.iter().all(|v| *v > 0.0));
  }

  #[test]
  fn narrow_and_degenerate_areas_draw_without_panicking() {
    let mut view = SpectrumView::new(SpectrumStyle::Gradient);
    view.accept(active([1.0; BANDS], None));
    for (width, height) in [(1, 1), (2, 3), (10, 2), (3, 20), (80, 30)] {
      let mut terminal = backend(width, height);
      render(&mut view, &mut terminal, true);
    }
    for style in SpectrumStyle::ALL {
      let mut view = SpectrumView::new(style);
      view.accept(active([1.0; BANDS], None));
      let mut terminal = backend(6, 5);
      render(&mut view, &mut terminal, true);
    }
  }

  #[test]
  fn every_style_renders_every_theme() {
    // A theme with an odd palette must not break blending or glyph choice.
    for theme in THEMES {
      for style in SpectrumStyle::ALL {
        let mut view = SpectrumView::new(style);
        view.accept(active([0.7; BANDS], None));
        let mut terminal = backend(40, 12);
        terminal.draw(|f| view.draw(f, f.area(), theme, true)).unwrap();
      }
    }
  }
}
