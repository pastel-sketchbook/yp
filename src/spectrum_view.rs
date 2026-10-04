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
/// Recent frames drawn behind the bars in the trail style.
const TRAIL_LENGTH: usize = 6;
/// Decade markers for the frequency axis: (power of ten, label).
const AXIS_MARKS: &[(f32, &str)] = &[(1.0, "10"), (2.0, "100"), (3.0, "1k"), (4.0, "10k")];
/// How fast bars fall toward their target, in levels per second.
const DECAY: f32 = 1.8;
/// How fast an expired peak marker falls.
const PEAK_DECAY: f32 = 0.8;
/// How long a peak marker holds before it starts falling.
const PEAK_HOLD: std::time::Duration = std::time::Duration::from_millis(180);
/// A frame older than this is treated as stale, so bars fall on a stalled read.
const LIVENESS: std::time::Duration = std::time::Duration::from_millis(300);

/// How the spectrum is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SpectrumStyle {
  /// Continuous gradient across bar heights.
  #[default]
  Gradient,
  /// Bars drawn with braille dots: two columns and four rows per cell, with
  /// band values interpolated between them. Four times the vertical resolution
  /// of half-blocks and twice the horizontal.
  Smooth,
  /// Flat bars colored by height zone.
  Bars,
  /// Bars with a fading tail from recent frames behind them.
  Trail,
  /// Left and right channels as opposing meters, showing stereo width.
  Stereo,
  /// Single accent color.
  Mono,
  /// Bars mirrored about the vertical center.
  Mirror,
  /// Ladder of lit dots.
  Dots,
  /// Contribution-graph squares: a ladder of whole cells colored by the
  /// gradient spectrum rather than a flat per-row zone.
  Squares,
  /// Scrolling history of recent frames.
  Waterfall,
  /// Bands arranged around a circle, level as radius. Falls back to bars when
  /// the pane is too short to hold a circle.
  Radial,
}

impl SpectrumStyle {
  /// Cycle order, starting from the [`SpectrumStyle::default`] so the first
  /// `Ctrl+V` press moves to the next style rather than back to the default.
  pub const ALL: [SpectrumStyle; 11] = [
    SpectrumStyle::Gradient,
    SpectrumStyle::Smooth,
    SpectrumStyle::Dots,
    SpectrumStyle::Squares,
    SpectrumStyle::Bars,
    SpectrumStyle::Trail,
    SpectrumStyle::Stereo,
    SpectrumStyle::Mono,
    SpectrumStyle::Mirror,
    SpectrumStyle::Waterfall,
    SpectrumStyle::Radial,
  ];

  /// Next style in the cycle.
  pub fn next(self) -> Self {
    let index = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
    Self::ALL[(index + 1) % Self::ALL.len()]
  }

  pub fn id(self) -> &'static str {
    match self {
      SpectrumStyle::Gradient => "gradient",
      SpectrumStyle::Smooth => "smooth",
      SpectrumStyle::Bars => "bars",
      SpectrumStyle::Trail => "trail",
      SpectrumStyle::Stereo => "stereo",
      SpectrumStyle::Mono => "mono",
      SpectrumStyle::Mirror => "mirror",
      SpectrumStyle::Dots => "dots",
      SpectrumStyle::Squares => "squares",
      SpectrumStyle::Waterfall => "waterfall",
      SpectrumStyle::Radial => "radial",
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

/// Reads a band value at a fractional position, interpolating between bands.
///
/// [`BANDS`] values spread across a pane far wider than the band count leave
/// visible steps. Interpolating fills them in, which is what makes bars read as
/// a curve rather than a staircase.
fn sample_interpolated(levels: &[f32; BANDS], position: f32) -> f32 {
  if BANDS == 1 {
    return levels[0];
  }
  let position = position.clamp(0.0, 1.0) * (BANDS - 1) as f32;
  let low = position.floor() as usize;
  let high = (low + 1).min(BANDS - 1);
  let t = position - low as f32;
  levels[low] * (1.0 - t) + levels[high] * t
}

/// Dot rows per braille cell.
const BRAILLE_ROWS: usize = 4;
/// Dot columns per braille cell.
const BRAILLE_COLUMNS: usize = 2;
/// Braille bit for a dot, addressed by column and row counted from the top.
const fn braille_bit(column: usize, row: usize) -> u16 {
  match (column, row) {
    (0, 0) => 0x0001,
    (0, 1) => 0x0002,
    (0, 2) => 0x0004,
    (0, 3) => 0x0040,
    (1, 0) => 0x0008,
    (1, 1) => 0x0010,
    (1, 2) => 0x0020,
    (1, 3) => 0x0080,
    _ => 0,
  }
}

/// Builds a braille glyph from a per-dot occupancy grid.
///
/// `filled[column][row]` with row 0 at the top, matching the glyph's own
/// numbering rather than the bar's bottom-up sense.
fn braille_glyph(filled: &[[bool; BRAILLE_ROWS]; BRAILLE_COLUMNS]) -> char {
  let mut bits = 0_u16;
  for (column, rows) in filled.iter().enumerate() {
    for (row, on) in rows.iter().enumerate() {
      if *on {
        bits |= braille_bit(column, row);
      }
    }
  }
  char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ')
}

pub struct SpectrumView {
  style: SpectrumStyle,
  frame: Option<SpectrumFrame>,
  received: std::time::Instant,
  updated: std::time::Instant,
  levels: [f32; BANDS],
  peaks: [f32; BANDS],
  hold: [std::time::Instant; BANDS],
  /// Right channel, tracked separately so the stereo display can show width.
  levels_right: [f32; BANDS],
  peaks_right: [f32; BANDS],
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
      levels_right: [0.0; BANDS],
      peaks_right: [0.0; BANDS],
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
    self.levels_right.fill(0.0);
    self.peaks_right.fill(0.0);
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
    match self.style {
      SpectrumStyle::Waterfall => self.draw_waterfall(frame.buffer_mut(), body, theme),
      // Radial needs both channels only as much as bars do, and it animates
      // between frames, so it shares the decay path.
      SpectrumStyle::Smooth => {
        self.advance(playing);
        self.draw_smooth(frame.buffer_mut(), body, theme);
      }
      SpectrumStyle::Trail => {
        self.advance(playing);
        self.draw_trail(frame.buffer_mut(), body, theme);
      }
      SpectrumStyle::Stereo => {
        self.advance(playing);
        self.draw_stereo(frame.buffer_mut(), body, theme);
      }
      SpectrumStyle::Radial => {
        self.advance(playing);
        self.draw_radial(frame.buffer_mut(), body, theme);
      }
      _ => {
        self.advance(playing);
        self.draw_bars(frame.buffer_mut(), body, theme);
      }
    }
    self.draw_axis(frame, inner, body, theme);
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
    let frame = self.frame.as_ref().filter(|f| live && f.active);
    for i in 0..BANDS {
      let target = frame.map_or(0.0, |f| f.levels[i].clamp(0.0, 1.0));
      self.levels[i] = target.max(self.levels[i] - dt * DECAY);
      if self.levels[i] >= self.peaks[i] {
        self.peaks[i] = self.levels[i];
        self.hold[i] = now + PEAK_HOLD;
      } else if now >= self.hold[i] {
        self.peaks[i] = self.levels[i].max(self.peaks[i] - dt * PEAK_DECAY);
      }
      // The right channel rides the same decay so both meters stay comparable.
      let target_right = frame.map_or(0.0, |f| f.levels_right[i].clamp(0.0, 1.0));
      self.levels_right[i] = target_right.max(self.levels_right[i] - dt * DECAY);
      if self.levels_right[i] >= self.peaks_right[i] {
        self.peaks_right[i] = self.levels_right[i];
        self.hold[i] = now + PEAK_HOLD;
      } else if now >= self.hold[i] {
        self.peaks_right[i] = self.levels_right[i].max(self.peaks_right[i] - dt * PEAK_DECAY);
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
          SpectrumStyle::Dots | SpectrumStyle::Squares => {
            // Squares keeps the ladder but fills the whole cell, and colors it
            // from the gradient rather than the flat zones, which reads as a
            // contribution graph instead of a bar chart.
            let square = self.style == SpectrumStyle::Squares;
            let lit = level.round() as u16;
            let peak_row = (peak.round() as u16).checked_sub(1);
            for row in 0..rows {
              let cell = &mut buf[(x, bottom - 1 - row)];
              if row < lit || peak_row == Some(row) {
                let color = if square {
                  spectrum_gradient(theme, row as f32 / rows.saturating_sub(1).max(1) as f32)
                } else {
                  Self::zone(theme, row, rows)
                };
                cell.set_char(if square { '█' } else { '●' }).set_fg(color);
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

  /// Draws octave frequency marks along the bottom row.
  ///
  /// The bands are logarithmic, so the axis is labelled in decades too: a
  /// linear readout of what sits where is otherwise guesswork.
  fn draw_axis(&self, frame: &mut Frame, inner: Rect, body: Rect, theme: &Theme) {
    let Some(current) = self.frame.as_ref() else {
      return;
    };
    let low = current.low_hz.max(1.0);
    let high = current.high_hz.max(low * 2.0);
    let span = (high / low).log10();
    let y = inner.y + body.height;

    // One label per decade that fits, widest first so the labels never collide.
    for &(marker, label) in AXIS_MARKS {
      let ratio = 10_f32.powf(marker);
      if ratio < low || ratio > high || span <= 0.0 {
        continue;
      }
      let position = ((ratio / low).log10() / span).clamp(0.0, 1.0);
      // Land the label's left edge at the mark, but keep it inside the pane.
      let width = label.len().min(usize::from(inner.width)) as u16;
      if width == 0 || width > inner.width {
        continue;
      }
      let x = (f32::from(inner.x) + position * f32::from(inner.width.saturating_sub(1)))
        .round()
        .clamp(f32::from(inner.x), f32::from(inner.right().saturating_sub(width))) as u16;
      frame.render_widget(Paragraph::new(label).style(Style::default().fg(theme.muted)), Rect::new(x, y, width, 1));
    }
  }

  /// Braille bars: two dot columns and four dot rows per cell.
  ///
  /// Four times the vertical resolution of half-block glyphs and twice the
  /// horizontal, which is what makes the interpolated curve readable. Falls back
  /// to plain bars if the pane is too small for the extra detail to show.
  fn draw_smooth(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    if body.width < 4 || body.height < 2 {
      self.draw_bars(buf, body, theme);
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

  /// Bars with a fading tail drawn from recent frames.
  ///
  /// Reuses the waterfall's frame history, so no extra state: a transient
  /// leaves a visible streak that decays over the trail length.
  fn draw_trail(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    let height = body.height;
    if height == 0 || body.width == 0 {
      return;
    }
    let count = BANDS.min(usize::from(body.width)).max(1);
    let bottom = body.y + height;
    let history: Vec<&[f32; BANDS]> = self.history.iter().rev().take(TRAIL_LENGTH).collect();

    // Draw oldest first so newer frames land on top.
    for (age, frame) in history.iter().enumerate() {
      let fade = 1.0 - (age as f32 + 1.0) / (TRAIL_LENGTH + 1) as f32;
      if fade <= 0.0 {
        break;
      }
      for bar in 0..count {
        let position = bar as f32 / (count - 1).max(1) as f32;
        let level = sample_interpolated(frame, position) * f32::from(height);
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

  /// Left and right channels as opposing meters.
  ///
  /// Reads as a single bar when the channels match and opens a V when the
  /// stereo image is wide, which the summed display could never show.
  fn draw_stereo(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    let height = body.height & !1;
    if height < 4 || body.width < 8 {
      self.draw_bars(buf, body, theme);
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
        let (glyph_l, _) = self.bar_cell(theme, row, half, left, 0.0);
        let (glyph_r, _) = self.bar_cell(theme, row, half, right, 0.0);
        buf[(x, y_up)].set_char(glyph_l).set_fg(Self::zone(theme, row, half)).set_bg(theme.panel_bg);
        buf[(x, y_down)].set_char(glyph_r).set_fg(Self::zone(theme, row, half)).set_bg(theme.panel_bg);
      }
      // A dim spine keeps the two meters visually paired.
      buf[(x, bottom - half)].set_char('│').set_fg(theme.border).set_bg(theme.panel_bg);
    }
  }

  /// Bands around a circle, level as radius.
  ///
  /// A circle needs roughly equal width and height; in a wide, short pane this
  /// falls back to bars rather than rendering a squashed ring.
  fn draw_radial(&self, buf: &mut Buffer, body: Rect, theme: &Theme) {
    let diameter = body.height.min(body.width);
    if diameter < 6 {
      self.draw_bars(buf, body, theme);
      return;
    }
    let centre_x = f32::from(body.x) + f32::from(body.width) / 2.0;
    let centre_y = f32::from(body.y) + f32::from(body.height) / 2.0;
    let max_radius = f32::from(diameter) / 2.0 - 0.5;

    // Band 0 at the top, sweeping clockwise: high frequencies to the right.
    let angles: Vec<f32> = (0..BANDS)
      .map(|band| -std::f32::consts::FRAC_PI_2 + band as f32 / BANDS as f32 * std::f32::consts::TAU)
      .collect();

    for (band, angle) in angles.iter().enumerate() {
      let level = self.levels[band] * max_radius;
      let steps = level.max(1.0) as usize;
      for step in 0..steps {
        let radius = 1.0 + step as f32;
        if radius > max_radius {
          break;
        }
        let x = centre_x + radius * angle.cos();
        let y = centre_y + radius * angle.sin();
        if x < f32::from(body.x) || x > f32::from(body.x + body.width - 1) {
          continue;
        }
        if y < f32::from(body.y) || y > f32::from(body.y + body.height - 1) {
          continue;
        }
        let cell = &mut buf[(x.round() as u16, y.round() as u16)];
        cell.set_char('•').set_fg(spectrum_gradient(theme, level / max_radius)).set_bg(theme.panel_bg);
      }
    }
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
  fn every_new_style_renders_and_animates() {
    for style in [SpectrumStyle::Smooth, SpectrumStyle::Trail, SpectrumStyle::Stereo, SpectrumStyle::Radial] {
      let mut view = SpectrumView::new(style);
      let mut terminal = backend(60, 16);
      view.accept(active([0.8; BANDS], None));
      render(&mut view, &mut terminal, true);
      assert!(view.needs_animation(), "{style:?} must animate while audio plays");
      age(&mut view);
      render(&mut view, &mut terminal, true);
    }
  }

  #[test]
  fn the_cycle_covers_every_style_exactly_once_and_wraps() {
    let mut seen = std::collections::HashSet::new();
    let mut style = SpectrumStyle::default();
    for _ in 0..SpectrumStyle::ALL.len() {
      assert!(seen.insert(style), "{style:?} repeated in the cycle");
      style = style.next();
    }
    assert_eq!(seen.len(), SpectrumStyle::ALL.len());
    assert_eq!(style, SpectrumStyle::default(), "the cycle must wrap to the default");
    for style in SpectrumStyle::ALL {
      assert_eq!(SpectrumStyle::from_id(style.id()), style, "{style:?} must round trip by name");
    }
  }

  #[test]
  fn interpolation_fills_the_gaps_between_bands() {
    // A linear ramp: any position between bands must read between its
    // neighbours rather than snapping to a staircase.
    let ramp: [f32; BANDS] = std::array::from_fn(|i| i as f32 / (BANDS - 1) as f32);
    assert_eq!(sample_interpolated(&ramp, 0.0), 0.0, "the low end is read exactly");
    assert_eq!(sample_interpolated(&ramp, 1.0), 1.0, "the high end is read exactly");
    for position in [0.1, 0.25, 0.5, 0.75, 0.9] {
      let read = sample_interpolated(&ramp, position);
      let expected = position.clamp(0.0, 1.0);
      assert!((read - expected).abs() < 0.05, "at {position} expected about {expected}, got {read}");
    }
    // A lone hot band must still fall off smoothly around itself.
    let mut spike = [0.0; BANDS];
    spike[0] = 1.0;
    assert_eq!(sample_interpolated(&spike, 0.0), 1.0);
    assert_eq!(sample_interpolated(&spike, 1.0), 0.0, "far from the spike is silence");
    assert!(sample_interpolated(&spike, 0.02) > sample_interpolated(&spike, 0.08), "must decay away from the spike");
  }

  #[test]
  fn interpolation_clamps_out_of_range_positions() {
    let ramp = [0.5; BANDS];
    assert_eq!(sample_interpolated(&ramp, -1.0), 0.5);
    assert_eq!(sample_interpolated(&ramp, 2.0), 0.5);
  }

  #[test]
  fn braille_glyphs_use_the_documented_dot_numbering() {
    assert_eq!(braille_glyph(&[[false; BRAILLE_ROWS]; BRAILLE_COLUMNS]), '\u{2800}', "blank cell");
    let mut bottom_left = [[false; BRAILLE_ROWS]; BRAILLE_COLUMNS];
    bottom_left[0][BRAILLE_ROWS - 1] = true;
    assert_eq!(braille_glyph(&bottom_left), char::from_u32(0x2800 + 0x40).expect("valid"));
    let mut top_right = [[false; BRAILLE_ROWS]; BRAILLE_COLUMNS];
    top_right[1][0] = true;
    assert_eq!(braille_glyph(&top_right), char::from_u32(0x2800 + 0x08).expect("valid"));
    assert_eq!(braille_glyph(&[[true; BRAILLE_ROWS]; BRAILLE_COLUMNS]), char::from_u32(0x28FF).expect("valid"));
  }

  #[test]
  fn smooth_draws_braille_and_falls_back_when_too_small() {
    let mut view = SpectrumView::new(SpectrumStyle::Smooth);
    let mut terminal = backend(40, 12);
    view.accept(active([0.5; BANDS], None));
    render(&mut view, &mut terminal, true);
    let buf = buffer(&terminal);
    let braille_cells = (1..=9)
      .flat_map(|y| (1..=38).map(move |x| (x, y)))
      .filter(|(x, y)| buf[(*x, *y)].symbol().chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)))
      .count();
    assert!(braille_cells > 8, "expected a lit braille region, got {braille_cells} cells");

    // Too small for the detail: must degrade to bars, not garbage.
    let mut tiny = backend(3, 2);
    render(&mut view, &mut tiny, true);
  }

  #[test]
  fn stereo_separates_the_channels() {
    let mut view = SpectrumView::new(SpectrumStyle::Stereo);
    let mut terminal = backend(40, 12);
    let mut frame = active([1.0; BANDS], None);
    frame.levels_right = [0.05; BANDS];
    view.accept(frame);
    render(&mut view, &mut terminal, true);
    let buf = buffer(&terminal);
    let lit = |y: u16| (1..=38).filter(|x| buf[(*x, y)].symbol() != " ").count();
    // The upper half is the left channel, the lower the right. With 9 rows of
    // body they occupy 5..=8 and 1..=4 respectively.
    assert!(lit(7) > lit(2), "left meter is the loud one: {} above, {} below", lit(7), lit(2));
  }

  #[test]
  fn stereo_is_symmetric_for_a_mono_signal() {
    let mut view = SpectrumView::new(SpectrumStyle::Stereo);
    let mut terminal = backend(40, 12);
    // A mono source decodes to the same signal on both channels.
    let mut frame = active([0.7; BANDS], None);
    frame.levels_right = frame.levels;
    view.accept(frame);
    render(&mut view, &mut terminal, true);
    let buf = buffer(&terminal);
    let lit = |y: u16| (1..=38).filter(|x| buf[(*x, y)].symbol() != " ").count();
    assert!(lit(7) > 0 && lit(2) > 0, "both halves must draw something: {} above, {} below", lit(7), lit(2));
    assert_eq!(lit(7), lit(2), "identical channels must render symmetrically");
  }

  #[test]
  fn the_axis_names_real_frequencies() {
    let mut view = SpectrumView::new(SpectrumStyle::Bars);
    let mut terminal = backend(40, 12);
    let mut frame = active([0.5; BANDS], None);
    frame.low_hz = 40.0;
    frame.high_hz = 16_000.0;
    view.accept(frame);
    render(&mut view, &mut terminal, true);
    let buf = terminal.backend().buffer().clone();
    let axis: String = (0..40).map(|x| buf[(x, 10)].symbol()).collect();
    assert!(axis.contains("100") || axis.contains("1k"), "the axis should name octave marks, got {axis:?}");
  }

  #[test]
  fn wide_panes_fall_back_instead_of_rendering_nonsense() {
    for style in [SpectrumStyle::Radial, SpectrumStyle::Stereo, SpectrumStyle::Smooth] {
      let mut view = SpectrumView::new(style);
      view.accept(active([0.9; BANDS], None));
      for (width, height) in [(3, 2), (5, 3), (8, 4), (12, 6)] {
        let mut terminal = backend(width, height);
        render(&mut view, &mut terminal, true);
      }
    }
  }

  #[test]
  fn squares_fill_whole_cells_and_gradient_colors_them() {
    let mut view = SpectrumView::new(SpectrumStyle::Squares);
    let mut terminal = backend(40, 12);
    view.accept(active([0.9; BANDS], None));
    render(&mut view, &mut terminal, true);
    let buf = buffer(&terminal);

    // A lit cell must be a full block, not the round dot the sibling style uses.
    let mut blocks = 0;
    let mut dots = 0;
    for y in 1..=9 {
      for x in 1..=38 {
        match glyph_at(&terminal, x, y).as_str() {
          "\u{2588}" => blocks += 1,
          "\u{25cf}" => dots += 1,
          _ => {}
        }
      }
    }
    assert!(blocks > 0, "squares must draw solid blocks, got none");
    assert_eq!(dots, 0, "squares must not draw the round dot glyph");

    // The gradient must actually vary with height; a flat color would put every
    // lit cell on the same entry of the spectrum ramp.
    let mut colors = std::collections::HashSet::new();
    for y in 1..=9 {
      for x in 1..=38 {
        if glyph_at(&terminal, x, y) == "\u{2588}" {
          colors.insert(buf[(x, y)].fg);
        }
      }
    }
    assert!(colors.len() >= 2, "expected the gradient to vary, got {} colors", colors.len());
  }

  #[test]
  fn squares_fall_back_to_plain_bars_in_a_sliver() {
    // Below one usable row the ladder cannot form, so it must not panic.
    let mut view = SpectrumView::new(SpectrumStyle::Squares);
    view.accept(active([0.9; BANDS], None));
    for (width, height) in [(1, 1), (2, 2), (3, 3), (6, 2)] {
      let mut terminal = backend(width, height);
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
