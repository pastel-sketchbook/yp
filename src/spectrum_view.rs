//! The Now Playing spectrum panel.
//!
//! Each style lives in its own file under `spectrum_view/`; this module owns the
//! style list, the shared animation state, and the dispatch that picks a
//! renderer. Anything two styles agree on — bar geometry, band merging, band
//! sampling — is in `geometry.rs`; anything that draws dots at braille
//! resolution is in `braille.rs`.
//!
//! Most of the styles, and the per-channel analyzer split behind them, are
//! derived from vtamp. See `NOTICE` at the repository root.

mod axis;
mod bars;
mod braille;
mod fire;
mod geometry;
mod radial;
mod ridge;
mod smooth;
mod sparks;
mod squares;
mod stereo;
mod trail;
mod waterfall;

use crate::spectrum::{BANDS, SpectrumFrame};
use crate::theme::Theme;
use bars::BarKind;
use fire::Fire;
use radial::Radial;
use rand::{SeedableRng, rngs::SmallRng};
use ratatui::{
  Frame,
  layout::Rect,
  style::Style,
  text::Line,
  widgets::{Block, Borders},
};
use sparks::Sparks;
use stereo::Stereo;

/// Rows the waterfall and ridge remember; more than any pane shows, bounded for memory.
const HISTORY: usize = 256;
const DECAY: f32 = 1.8;
const PEAK_DECAY: f32 = 0.8;
const PEAK_HOLD: std::time::Duration = std::time::Duration::from_millis(180);
const LIVENESS: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SpectrumStyle {
  /// Continuous gradient across bar heights.
  #[default]
  Gradient,
  /// Bars drawn with braille dots: two columns and four rows per cell, with
  /// band values interpolated between them.
  Smooth,
  /// Ladder of lit dots.
  Dots,
  /// Contribution-graph squares: a ladder of whole cells colored by the
  /// gradient spectrum rather than a flat per-row zone.
  Squares,
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
  /// Scrolling history of recent frames.
  Waterfall,
  /// Doom-style fire climbing the bands on half-block pixels.
  Fire,
  /// Ridgelines of recent frames, nearest at the bottom.
  Ridge,
  /// Sparks thrown from the bar tops when a band jumps.
  Sparks,
  /// Polar petals around a ring, with onset waves.
  Radial,
}

impl SpectrumStyle {
  pub const ALL: [SpectrumStyle; 14] = [
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
    SpectrumStyle::Fire,
    SpectrumStyle::Ridge,
    SpectrumStyle::Sparks,
    SpectrumStyle::Radial,
  ];

  /// Next style in the cycle.
  pub fn next(self) -> Self {
    let index = Self::ALL.iter().position(|style| *style == self).unwrap_or(0);
    Self::ALL[(index + 1) % Self::ALL.len()]
  }

  /// Stable name for the preferences file.
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
      SpectrumStyle::Fire => "fire",
      SpectrumStyle::Ridge => "ridge",
      SpectrumStyle::Sparks => "sparks",
      SpectrumStyle::Radial => "radial",
    }
  }

  /// Reads a style back from its stored name, defaulting when unrecognized.
  pub fn from_id(id: &str) -> Self {
    Self::ALL.iter().copied().find(|style| style.id() == id).unwrap_or_default()
  }

  /// Styles drawn from the frame history; they change only when a frame arrives.
  fn follows_frames(self) -> bool {
    matches!(self, SpectrumStyle::Waterfall | SpectrumStyle::Ridge | SpectrumStyle::Trail)
  }

  /// The bar look a style draws, for the styles that are a bar variant.
  fn bar_kind(self) -> Option<BarKind> {
    Some(match self {
      SpectrumStyle::Bars => BarKind::Zoned,
      SpectrumStyle::Gradient => BarKind::Gradient,
      SpectrumStyle::Mono => BarKind::Mono,
      SpectrumStyle::Mirror => BarKind::Mirror,
      SpectrumStyle::Dots => BarKind::Dots,
      SpectrumStyle::Squares => BarKind::Squares,
      _ => return None,
    })
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
  /// Levels of recent active frames, oldest first; waterfall and ridge draw these.
  history: std::collections::VecDeque<[f32; BANDS]>,
  /// Frames ever pushed, so ridge can keep its depth stable as old rows drop.
  pushed: u64,
  redraw: bool,
  /// Styles that carry their own animation state rather than reading the frame.
  fire: Fire,
  radial: Radial,
  sparks: Sparks,
  /// The two channel envelopes, which fall on their own clock.
  stereo: Stereo,
  rng: SmallRng,
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
      pushed: 0,
      redraw: true,
      fire: Fire::default(),
      radial: Radial::default(),
      sparks: Sparks::default(),
      stereo: Stereo::default(),
      rng: SmallRng::seed_from_u64(0x5EED),
    }
  }

  /// Switches rendering only; levels, peaks, and history carry over. Styles that
  /// animate on their own start cold, since their state describes a look rather
  /// than the audio.
  pub fn set_style(&mut self, style: SpectrumStyle) {
    self.style = style;
    self.fire.reset();
    self.radial.reset();
    self.sparks.reset();
    self.redraw = true;
  }

  /// Forgets the current stream, used when playback stops.
  pub fn clear(&mut self) {
    self.frame = None;
    self.history.clear();
    self.pushed = 0;
    self.reset_levels();
    self.fire.reset();
    self.radial.reset();
    self.sparks.reset();
  }

  fn reset_levels(&mut self) {
    self.levels.fill(0.0);
    self.peaks.fill(0.0);
    self.stereo.reset();
    self.redraw = true;
  }

  /// True while the display still has something to animate.
  ///
  /// The particle styles keep moving after the last frame, so they report
  /// themselves active rather than relying on the levels being nonzero.
  pub fn needs_animation(&self) -> bool {
    if self.style.follows_frames() {
      // Rows only appear with frames; nothing moves between them.
      return self.redraw;
    }
    self.redraw
      || self.fire.is_hot()
      || self.radial.is_active()
      || self.sparks.is_active()
      || self.stereo.is_active()
      || self.levels.iter().chain(&self.peaks).any(|value| *value > 0.0)
  }

  /// Adopts a new analyzer frame, resetting state when the track changes.
  pub fn accept(&mut self, frame: SpectrumFrame) {
    let previous = self.frame.as_ref();
    let new_track = previous.is_some_and(|p| p.current_id != frame.current_id);
    let new_generation = previous.is_some_and(|p| p.generation != frame.generation);
    if new_track {
      self.frame = None;
      self.history.clear();
      self.pushed = 0;
      self.reset_levels();
    } else if new_generation {
      self.reset_levels();
      // A seek, pause, or resume is not a musical onset, so the styles that
      // compare frames drop what they were holding.
      self.fire.reset();
      self.radial.reset();
      self.sparks.reset();
    }
    if frame.active {
      if self.history.len() == HISTORY {
        self.history.pop_front();
      }
      self.history.push_back(frame.levels);
      self.pushed += 1;
      self.redraw |= self.style.follows_frames();
      // Onsets compare two consecutive frames of one stream; a reset or a gap
      // starts the comparison over.
      if self.received.elapsed() < LIVENESS
        && let Some(previous) = self.frame.as_ref().filter(|f| f.active && f.generation == frame.generation)
      {
        match self.style {
          SpectrumStyle::Radial => self.radial.observe(&previous.levels, &frame.levels),
          SpectrumStyle::Sparks => self.sparks.observe(&previous.levels, &frame.levels),
          _ => {}
        }
      }
    }
    self.frame = Some(frame);
    self.received = std::time::Instant::now();
    self.redraw = true;
  }

  /// Advances decay and peak hold, then draws the body.
  ///
  /// `cell` is the terminal cell size in pixels, which is what keeps the radial
  /// style round on a terminal whose cells are not square.
  pub fn draw(&mut self, frame: &mut Frame, area: Rect, theme: &Theme, playing: bool, cell: (u16, u16)) {
    self.redraw = false;
    let inner = self.header(frame, area, theme);
    if inner.width == 0 || inner.height == 0 {
      return;
    }
    let body = Rect { height: inner.height.saturating_sub(1), ..inner };
    // History styles have nothing to decay between frames, and advancing them
    // would let peaks fall out from under a line that is already drawn.
    let dt = if self.style.follows_frames() { 0.0 } else { self.advance(playing) };
    // The channel envelopes ride their own decay, which only the stereo style
    // reads, so they step even when the bars are not advancing.
    if self.style == SpectrumStyle::Stereo && !self.style.follows_frames() {
      let live = playing && self.received.elapsed() < LIVENESS;
      let channels = self.frame.as_ref().filter(|f| live && f.active).map(|f| &f.channels);
      self.stereo.advance(channels, dt);
    }
    let buf = frame.buffer_mut();
    match self.style {
      SpectrumStyle::Smooth if smooth::fits(body) => smooth::draw(buf, body, theme, &self.levels),
      SpectrumStyle::Smooth => bars::draw(self, buf, body, theme, BarKind::Zoned),
      SpectrumStyle::Squares => squares::draw(buf, body, theme, &self.levels, &self.peaks),
      SpectrumStyle::Stereo if stereo::fits(body) => self.stereo.draw(buf, body, theme),
      SpectrumStyle::Stereo => self.draw_bars(buf, body, theme, BarKind::Zoned),
      SpectrumStyle::Trail => trail::draw(buf, body, theme, &self.history),
      SpectrumStyle::Waterfall => waterfall::draw(self, buf, body, theme),
      SpectrumStyle::Radial => {
        self.radial.advance(dt);
        self.radial.draw(buf, body, theme, &self.levels, &self.peaks, cell);
      }
      SpectrumStyle::Fire => self.fire.draw(buf, body, theme, &self.levels, dt, &mut self.rng),
      SpectrumStyle::Ridge => ridge::draw(buf, body, theme, &self.history, self.pushed),
      SpectrumStyle::Sparks => {
        self.draw_bars(buf, body, theme, BarKind::Zoned);
        self.sparks.draw(buf, body, theme, &self.levels, dt, &mut self.rng);
      }
      // The rest are bar variants and share one renderer.
      _ => self.draw_bars(buf, body, theme, self.style.bar_kind().unwrap_or(BarKind::Zoned)),
    }
    // The axis has to line up with what was actually drawn, which is not always
    // the whole pane: stereo insets a gutter for its channel labels, and the
    // styles that wrap or stack the bands have no meaningful horizontal scale.
    let (graph, mapping) = match self.style {
      SpectrumStyle::Radial => (body, axis::Mapping::Ends),
      SpectrumStyle::Stereo if stereo::fits(body) => (stereo::graph(body), axis::Mapping::Bars),
      SpectrumStyle::Smooth if smooth::fits(body) => (body, axis::Mapping::Continuous),
      SpectrumStyle::Waterfall | SpectrumStyle::Fire | SpectrumStyle::Ridge => (body, axis::Mapping::Continuous),
      _ => (body, axis::Mapping::Bars),
    };
    axis::draw(buf, graph, body.bottom(), theme, mapping, self.frame.as_ref());
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
  ///
  /// Returns the seconds since the previous call, which the styles with their own
  /// physics use to step forward.
  fn advance(&mut self, playing: bool) -> f32 {
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
    }
    dt
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::spectrum::SpectrumChannels;
  use crate::theme::THEMES;
  use rand::SeedableRng;
  use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::{Buffer, Cell},
  };
  use std::ops::Range;

  /// A typical terminal cell: twice as tall as it is wide.
  const CELL: (u16, u16) = (10, 20);

  fn styled(style: SpectrumStyle) -> SpectrumView {
    let mut view = SpectrumView::new(style);
    // The particle styles are random; a fixed seed keeps their tests repeatable.
    view.rng = SmallRng::seed_from_u64(7);
    view
  }
  fn backend(width: u16, height: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(width, height)).expect("test backend")
  }
  fn render(view: &mut SpectrumView, terminal: &mut Terminal<TestBackend>, playing: bool) {
    terminal.draw(|f| view.draw(f, f.area(), &THEMES[0], playing, CELL)).expect("draw");
  }
  fn active(levels: [f32; BANDS]) -> SpectrumFrame {
    SpectrumFrame { active: true, levels, ..SpectrumFrame::default() }
  }
  /// An active frame carrying a realistic frequency range, for tests that compare
  /// whole buffers and would otherwise differ on the axis row alone.
  fn scaled(levels: [f32; BANDS]) -> SpectrumFrame {
    SpectrumFrame { low_hz: 40.0, high_hz: 16_000.0, ..active(levels) }
  }
  /// An active frame with independent channels.
  fn stereo(levels: [f32; BANDS], left: [f32; BANDS], right: [f32; BANDS]) -> SpectrumFrame {
    SpectrumFrame { channels: SpectrumChannels { left, right }, ..active(levels) }
  }
  /// A frame of `level` in `bands` and silence elsewhere.
  fn bands_at(range: Range<usize>, level: f32) -> [f32; BANDS] {
    std::array::from_fn(|band| if range.contains(&band) { level } else { 0.0 })
  }
  fn top_row(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.width).map(|x| buffer[(x, 0)].symbol().to_string()).collect()
  }
  fn row_symbols(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect()
  }
  fn glyph_at(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> String {
    terminal.backend().buffer()[(x, y)].symbol().to_string()
  }
  /// Cells of `y` that are not blank, which is how a shape is measured.
  fn lit(terminal: &Terminal<TestBackend>, y: u16) -> usize {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.width).filter(|x| buffer[(*x, y)].symbol() != " ").count()
  }
  /// Lets the next draw see 200 ms of decay with an expired peak hold.
  fn age(view: &mut SpectrumView) {
    view.updated = std::time::Instant::now() - std::time::Duration::from_millis(200);
    view.hold.fill(std::time::Instant::now() - std::time::Duration::from_secs(1));
  }
  /// Lets frames go stale and the envelope, fire, and sparks settle while paused.
  fn settle(view: &mut SpectrumView, terminal: &mut Terminal<TestBackend>) {
    view.received = std::time::Instant::now() - std::time::Duration::from_secs(1);
    for _ in 0..40 {
      age(view);
      render(view, terminal, false);
    }
  }
  fn braille_glyphs(buffer: &Buffer) -> usize {
    buffer.content().iter().filter(|cell| cell.symbol().chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c))).count()
  }
  fn is_bar(cell: &Cell) -> bool {
    cell.symbol() != " " && cell.symbol().chars().all(|c| ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'].contains(&c))
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
  fn an_unknown_style_name_falls_back_to_the_default() {
    // A preferences file written by a newer build must not wedge the panel.
    assert_eq!(SpectrumStyle::from_id("plasma"), SpectrumStyle::default());
    assert_eq!(SpectrumStyle::from_id(""), SpectrumStyle::default());
  }

  #[test]
  fn animation_sleeps_after_decay_and_wakes_for_new_audio() {
    let mut view = styled(SpectrumStyle::Bars);
    let mut terminal = backend(40, 12);
    render(&mut view, &mut terminal, false);
    view.accept(active([1.0; BANDS]));
    assert!(view.needs_animation());
    render(&mut view, &mut terminal, true);
    settle(&mut view, &mut terminal);
    assert!(!view.needs_animation(), "the panel must stop asking for frames");
    view.accept(active([0.5; BANDS]));
    assert!(view.needs_animation(), "resume wakes animation");
    render(&mut view, &mut terminal, true);
    assert!(view.needs_animation(), "live levels keep the panel awake");
    // Audio that stopped arriving must not hold the panel open at full rate.
    view.received = std::time::Instant::now() - std::time::Duration::from_secs(1);
    settle(&mut view, &mut terminal);
    assert!(!view.needs_animation(), "stale audio cannot hold the panel awake");
  }

  #[test]
  fn titles_name_the_style() {
    let mut view = styled(SpectrumStyle::Gradient);
    let mut terminal = backend(44, 12);
    terminal.draw(|f| view.draw(f, f.area(), &THEMES[0], false, CELL)).expect("draw");
    assert!(top_row(&terminal).contains("SPECTRUM · gradient"), "{}", top_row(&terminal));
  }

  #[test]
  fn gradient_runs_between_the_theme_roles() {
    let theme = THEMES[0];
    let mut view = styled(SpectrumStyle::Gradient);
    let mut terminal = backend(40, 12);
    view.accept(active([1.0; BANDS]));
    render(&mut view, &mut terminal, true);
    // Body rows are y = 1..=9 (title row 0, axis row 10, border row 11).
    let buffer = terminal.backend().buffer();
    let column: Vec<ratatui::style::Color> = (1..=9).map(|y| buffer[(1, y)].fg).collect();
    assert!((1..=9).all(|y| buffer[(1, y)].symbol() == "█"));
    assert_eq!(column[8], theme.spectrum[0], "bottom row is the low role");
    assert_eq!(column[0], theme.spectrum[2], "top row is the high role");
    assert!(column.iter().any(|c| !theme.spectrum.contains(c)), "intermediate rows blend the roles");
    let mut distinct = column.clone();
    distinct.dedup();
    assert_eq!(distinct.len(), column.len(), "every row has its own color");
  }

  #[test]
  fn mono_uses_the_accent_and_marks_peaks_with_the_text_role() {
    let theme = THEMES[0];
    let mut view = styled(SpectrumStyle::Mono);
    let mut terminal = backend(40, 12);
    view.accept(active([0.5; BANDS]));
    render(&mut view, &mut terminal, true);
    let buffer = terminal.backend().buffer();
    assert!(buffer.content().iter().any(is_bar));
    assert!(
      buffer.content().iter().filter(|c| is_bar(c)).all(|c| c.fg == theme.accent),
      "every bar cell is the accent role"
    );
    assert!(!buffer.content().iter().any(|c| c.symbol() == "▔"));
    // Let the level fall below the held peak so the marker appears.
    view.accept(active([0.1; BANDS]));
    age(&mut view);
    render(&mut view, &mut terminal, true);
    let buffer = terminal.backend().buffer();
    let markers: Vec<_> = buffer.content().iter().filter(|c| c.symbol() == "▔").map(|c| c.fg).collect();
    assert!(!markers.is_empty(), "a falling level must show a peak marker");
    assert!(markers.iter().all(|c| *c == theme.fg), "the peak marker uses the text role");
  }

  #[test]
  fn bars_keep_a_blank_column_between_them() {
    let mut view = styled(SpectrumStyle::Bars);
    view.accept(active([1.0; BANDS]));
    // Odd inner widths are where an off-by-one in the spacing would show up.
    for width in [41_u16, 61, 80, 121] {
      let mut terminal = backend(width, 12);
      render(&mut view, &mut terminal, true);
      let buffer = terminal.backend().buffer();
      // Group the bottom body row into runs of filled columns.
      let filled: Vec<bool> = (1..width - 1).map(|x| buffer[(x, 9)].symbol() != " ").collect();
      let mut runs: Vec<(usize, usize)> = Vec::new();
      let mut previous = false;
      for (index, on) in filled.iter().enumerate() {
        match (*on, previous) {
          (true, true) => {
            if let Some(last) = runs.last_mut() {
              last.1 += 1;
            }
          }
          (true, false) => runs.push((index, 1)),
          _ => {}
        }
        previous = *on;
      }
      assert!(runs.len() > 1, "width {width} should draw several bars");
      let widths: Vec<usize> = runs.iter().map(|(_, length)| *length).collect();
      assert!(widths.iter().all(|length| *length == widths[0]), "width {width} drew uneven bars: {widths:?}");
      // Every bar must be separated from the next by at least one blank column,
      // which is what stops the bars reading as one solid block.
      for pair in runs.windows(2) {
        let (start, length) = pair[0];
        assert!(start + length < pair[1].0, "width {width} ran the bars at {start} and {} together", pair[1].0);
      }
    }
  }

  #[test]
  fn mirror_is_symmetric_and_inverts_partial_lower_cells() {
    let theme = THEMES[0];
    let mut view = styled(SpectrumStyle::Mirror);
    let mut terminal = backend(40, 12);
    view.accept(active([0.6; BANDS]));
    render(&mut view, &mut terminal, true);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(1, 1)].symbol(), " ", "the odd top row stays blank");
    for y in [4, 5, 6, 7] {
      assert_eq!(buffer[(1, y)].symbol(), "█", "row {y}");
    }
    let upper = &buffer[(1, 3)];
    let lower = &buffer[(1, 8)];
    assert_eq!(upper.symbol(), "▄", "0.4 of a row is four eighths");
    assert_eq!(lower.bg, upper.fg, "the lower half fills with the zone color");
    assert_eq!(lower.fg, theme.panel_bg, "and paints over it in the canvas color");
  }

  #[test]
  fn a_single_body_row_cannot_host_a_mirror() {
    let mut view = styled(SpectrumStyle::Mirror);
    view.accept(active([1.0; BANDS]));
    let mut terminal = backend(40, 4);
    render(&mut view, &mut terminal, true);
    assert_eq!(terminal.backend().buffer()[(1, 1)].symbol(), " ");
  }

  #[test]
  fn dots_light_whole_segments_and_hold_a_peak_dot() {
    let theme = THEMES[0];
    let mut view = styled(SpectrumStyle::Dots);
    let mut terminal = backend(40, 12);
    view.accept(active([0.5; BANDS]));
    render(&mut view, &mut terminal, true);
    let buffer = terminal.backend().buffer();
    // Body rows y = 1..=9; 0.5 × 9 = 4.5 rounds to 5 lit segments from the bottom.
    let lit_rows: Vec<u16> = (1..=9).filter(|y| buffer[(1, *y)].symbol() == "●").collect();
    assert_eq!(lit_rows, vec![5, 6, 7, 8, 9]);
    assert!((1..=4).all(|y| buffer[(1, y)].symbol() == "·" && buffer[(1, y)].fg == theme.border));
    assert_eq!(buffer[(1, 9)].fg, theme.spectrum[0]);
  }

  #[test]
  fn squares_fill_whole_cells_and_gradient_colors_them() {
    let mut view = styled(SpectrumStyle::Squares);
    let mut terminal = backend(40, 12);
    view.accept(active([0.9; BANDS]));
    render(&mut view, &mut terminal, true);
    let buffer = terminal.backend().buffer();
    let mut blocks = 0;
    let mut dots = 0;
    for y in 1..=9 {
      for x in 1..=38 {
        match buffer[(x, y)].symbol() {
          "█" => blocks += 1,
          "●" => dots += 1,
          _ => {}
        }
      }
    }
    assert!(blocks > 0, "squares must draw solid blocks, got none");
    assert_eq!(dots, 0, "squares must not draw the round dot glyph");
    let colors: std::collections::HashSet<_> = (1..=9)
      .flat_map(|y| (1..=38).map(move |x| (x, y)))
      .filter(|(x, y)| buffer[(*x, *y)].symbol() == "█")
      .map(|(x, y)| buffer[(x, y)].fg)
      .collect();
    assert!(colors.len() >= 2, "expected the gradient to vary, got {} colors", colors.len());
  }

  #[test]
  fn smooth_draws_braille_and_falls_back_when_too_small() {
    let mut view = styled(SpectrumStyle::Smooth);
    let mut terminal = backend(40, 12);
    view.accept(active([0.5; BANDS]));
    render(&mut view, &mut terminal, true);
    assert!(braille_glyphs(terminal.backend().buffer()) > 8, "expected a lit braille region");
    // Too small for the detail: it must degrade to bars, not braille noise.
    let mut tiny = backend(3, 2);
    render(&mut view, &mut tiny, true);
    assert_eq!(braille_glyphs(tiny.backend().buffer()), 0, "a sliver must fall back to bars");
  }

  #[test]
  fn the_stereo_style_routes_through_the_channel_envelopes() {
    let mut view = styled(SpectrumStyle::Stereo);
    let mut terminal = backend(40, 12);
    view.accept(SpectrumFrame {
      channels: SpectrumChannels { left: [1.0; BANDS], right: [0.05; BANDS] },
      low_hz: 40.0,
      high_hz: 16_000.0,
      ..active([1.0; BANDS])
    });
    render(&mut view, &mut terminal, true);
    // The view must feed the envelopes and label which half is which.
    assert!(view.stereo.is_active());
    assert_eq!(glyph_at(&terminal, 1, 5), "L", "the upper half is the left channel");
    assert_eq!(glyph_at(&terminal, 1, 6), "R", "the lower half is the right channel");
    assert!(lit(&terminal, 2) > 0, "the loud left channel fills the upper half");
    // The axis follows the inset graph, not the whole pane.
    assert!(row_symbols(&terminal, 10).contains("100"), "the labels sit under the graph");
  }

  #[test]
  fn a_pane_too_small_for_the_stereo_style_falls_back_to_bars() {
    let mut view = styled(SpectrumStyle::Stereo);
    view.accept(stereo([0.9; BANDS], [0.9; BANDS], [0.9; BANDS]));
    for (width, height) in [(3, 2), (5, 3), (9, 4), (12, 6)] {
      let mut terminal = backend(width, height);
      render(&mut view, &mut terminal, true);
    }
  }

  #[test]
  fn the_stereo_envelopes_settle_so_a_stopped_track_stops_the_panel() {
    let mut view = styled(SpectrumStyle::Stereo);
    let mut terminal = backend(40, 12);
    view.accept(stereo([1.0; BANDS], [1.0; BANDS], [1.0; BANDS]));
    render(&mut view, &mut terminal, true);
    assert!(view.needs_animation(), "live channels keep the panel awake");
    settle(&mut view, &mut terminal);
    assert!(!view.stereo.is_active(), "the envelopes must fall to nothing");
    assert!(!view.needs_animation(), "and the panel must stop asking for frames");
  }

  #[test]
  fn trail_leaves_a_streak_of_recent_frames_and_sleeps_without_them() {
    let theme = &THEMES[0];
    let mut view = styled(SpectrumStyle::Trail);
    let mut terminal = backend(40, 12);
    render(&mut view, &mut terminal, true);
    assert!(!view.needs_animation(), "an empty trail is idle");
    view.accept(active([0.2; BANDS]));
    assert!(view.needs_animation(), "a frame requests one draw");
    view.accept(active([1.0; BANDS]));
    render(&mut view, &mut terminal, false);
    // The body is nine rows at y = 1..=9, and each frame marks the row its level
    // reaches, so a full frame sits at the top and the quiet one near the bottom.
    let row_of = |level: f32| ((level * 9.0).ceil() as u16).saturating_sub(1);
    let x = 1 + super::geometry::bar_layout(38).next().expect("a bar").columns.start;
    let (newest, older) = (9 - row_of(1.0), 9 - row_of(0.2));
    assert_eq!(glyph_at(&terminal, x, newest), "▄", "the newest frame");
    assert_eq!(glyph_at(&terminal, x, older), "▄", "the older frame trails");
    assert!(
      (1..=9).all(|y| y == newest || y == older || glyph_at(&terminal, x, y) != "▄"),
      "nothing is drawn between the two frames"
    );
    let newest_color = terminal.backend().buffer()[(x, newest)].fg;
    let older_color = terminal.backend().buffer()[(x, older)].fg;
    assert_eq!(newest_color, super::bars::zone(theme, row_of(1.0), 9), "the newest keeps a zone color");
    assert_ne!(older_color, newest_color, "an older frame must be dimmer");
    assert!(!view.needs_animation(), "the trail persists until another frame arrives");
  }

  #[test]
  fn waterfall_scrolls_per_frame_freezes_without_them_and_survives_generations() {
    let mut view = styled(SpectrumStyle::Waterfall);
    let mut terminal = backend(40, 12);
    render(&mut view, &mut terminal, true);
    assert!(!view.needs_animation(), "an empty waterfall is idle");
    view.accept(active([1.0; BANDS]));
    assert!(view.needs_animation(), "a frame requests one draw");
    render(&mut view, &mut terminal, true);
    assert!(!view.needs_animation(), "nothing moves until the next frame");
    let buffer = terminal.backend().buffer();
    assert!((1..=38).all(|x| buffer[(x, 9)].symbol() == "█"));
    assert_eq!(buffer[(4, 8)].symbol(), " ", "one frame is one row");
    view.accept(active([0.3; BANDS]));
    view.accept(active([0.0; BANDS]));
    render(&mut view, &mut terminal, true);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(4, 9)].symbol(), " ", "silence is a blank row");
    assert_ne!(buffer[(4, 7)].symbol(), " ", "the older loud frame is still there");
    // A new generation keeps the past; a new track starts over.
    view.accept(SpectrumFrame { generation: 1, ..SpectrumFrame::default() });
    assert_eq!(view.history.len(), 3, "a new generation keeps the past");
    view.accept(SpectrumFrame { generation: 1, current_id: Some("next".into()), ..SpectrumFrame::default() });
    assert!(view.history.is_empty(), "a new track starts a fresh history");
    for _ in 0..(HISTORY + 10) {
      view.accept(active([0.2; BANDS]));
    }
    assert_eq!(view.history.len(), HISTORY, "the history is bounded");
    // Narrow panes merge bands instead of clipping them.
    let mut terminal = backend(20, 12);
    render(&mut view, &mut terminal, true);
    assert!((1..=18).all(|x| glyph_at(&terminal, x, 9) == "░"));
  }

  #[test]
  fn ridge_draws_lines_and_keeps_history_like_the_waterfall() {
    let mut view = styled(SpectrumStyle::Ridge);
    let mut terminal = backend(40, 12);
    render(&mut view, &mut terminal, true);
    assert!(!view.needs_animation(), "an empty ridge is idle");
    view.accept(active([0.6; BANDS]));
    assert!(view.needs_animation(), "a frame requests one draw");
    render(&mut view, &mut terminal, true);
    assert!(braille_glyphs(terminal.backend().buffer()) > 0, "ridge lines are braille");
    assert!(!view.needs_animation(), "nothing moves until the next frame");
    let drawn = terminal.backend().buffer().clone();
    for _ in 0..3 {
      age(&mut view);
      render(&mut view, &mut terminal, false);
    }
    assert_eq!(terminal.backend().buffer(), &drawn, "pause freezes the lines");
    view.accept(SpectrumFrame { generation: 1, ..SpectrumFrame::default() });
    assert_eq!(view.history.len(), 1, "a new generation keeps the past");
    assert_eq!(view.pushed, 1);
    view.accept(SpectrumFrame { generation: 1, current_id: Some("next".into()), ..SpectrumFrame::default() });
    assert!(view.history.is_empty(), "a new track starts over");
    assert_eq!(view.pushed, 0);
  }

  #[test]
  fn radial_settles_to_its_resting_ring_and_reacts_to_onsets() {
    let theme = THEMES[0];
    let mut view = styled(SpectrumStyle::Radial);
    let mut terminal = backend(60, 18);
    render(&mut view, &mut terminal, false);
    assert!(!view.needs_animation(), "an idle ring is not animating");
    let resting = terminal.backend().buffer().clone();
    view.accept(scaled([0.2; BANDS]));
    render(&mut view, &mut terminal, true);
    assert!(!view.radial.is_active(), "one frame has nothing to compare");
    view.accept(scaled([0.9; BANDS]));
    assert!(view.needs_animation(), "a jump between frames is an onset");
    render(&mut view, &mut terminal, true);
    assert!(view.radial.is_active());
    assert_ne!(terminal.backend().buffer(), &resting, "an onset must change the figure");
    settle(&mut view, &mut terminal);
    assert!(!view.radial.is_active(), "the pulse decays");
    assert!(!view.needs_animation());
    // Only the figure is compared: the axis row differs between the two
    // snapshots because the first was taken before any frame had arrived.
    // Only the body is compared. The title and the axis legitimately differ:
    // the first snapshot was taken before any frame arrived, so its axis shows
    // the LOW/HIGH fallback rather than decade marks.
    let body = |buffer: &Buffer| {
      (1..buffer.area.height.saturating_sub(2))
        .map(|y| {
          (0..buffer.area.width)
            .map(|x| (buffer[(x, y)].symbol().to_string(), buffer[(x, y)].fg, buffer[(x, y)].bg))
            .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
    };
    assert_eq!(
      body(terminal.backend().buffer()),
      body(&resting),
      "the pulse and its waves must decay back to the resting ring"
    );
    // The idle ring is drawn in the border role alone.
    assert!((1..=15).all(|y| (1..=58).all(|x| {
      let cell = &terminal.backend().buffer()[(x, y)];
      (cell.symbol() == " " || cell.fg == theme.border) && cell.bg == theme.panel_bg
    })));
    // A seek, pause, or resume is not an onset.
    view.accept(SpectrumFrame { generation: 1, ..scaled([0.2; BANDS]) });
    view.accept(SpectrumFrame { generation: 1, ..scaled([0.9; BANDS]) });
    age(&mut view);
    render(&mut view, &mut terminal, true);
    assert!(view.radial.is_active());
    view.accept(SpectrumFrame { generation: 2, ..SpectrumFrame::default() });
    assert!(!view.radial.is_active(), "a new generation stops the pulse at once");
  }

  #[test]
  fn fire_burns_while_heat_remains_and_resets_with_the_stream() {
    let theme = THEMES[0];
    let mut view = styled(SpectrumStyle::Fire);
    let mut terminal = backend(40, 12);
    render(&mut view, &mut terminal, false);
    assert!(!view.needs_animation(), "a cold fire is idle");
    view.accept(SpectrumFrame { generation: 1, ..active(bands_at(0..8, 1.0)) });
    for _ in 0..4 {
      age(&mut view);
      render(&mut view, &mut terminal, true);
    }
    assert!(view.fire.is_hot());
    let buffer = terminal.backend().buffer();
    assert!(buffer.content().iter().any(|cell| cell.symbol() == "▀"), "fire burns on half blocks");
    assert!((1..=9).all(|y| (30..=38).all(|x| buffer[(x, y)].symbol() == " ")), "the treble columns stay cold");
    settle(&mut view, &mut terminal);
    assert!(!view.fire.is_hot(), "the fire burns out once the levels are gone");
    assert!(!view.needs_animation());
    assert!(
      (1..=9).all(|y| (1..=38).all(|x| {
        let cell = &terminal.backend().buffer()[(x, y)];
        cell.symbol() == " " && cell.fg == theme.panel_bg && cell.bg == theme.panel_bg
      })),
      "a burnt-out fire leaves nothing behind"
    );
    view.accept(SpectrumFrame { generation: 2, ..active([1.0; BANDS]) });
    age(&mut view);
    render(&mut view, &mut terminal, true);
    assert!(view.fire.is_hot());
    view.accept(SpectrumFrame { generation: 3, ..SpectrumFrame::default() });
    assert!(!view.fire.is_hot(), "a new generation puts the fire out at once");
  }

  #[test]
  fn sparks_follow_rises_between_consecutive_frames_only() {
    let mut view = styled(SpectrumStyle::Sparks);
    let mut bars = styled(SpectrumStyle::Bars);
    let (mut terminal, mut plain) = (backend(64, 14), backend(64, 14));
    let quiet = bands_at(0..BANDS, 0.3);
    let mut loud = quiet;
    loud[20] = 0.8;
    // The first frame after a reset has nothing to compare with.
    for target in [&mut view, &mut bars] {
      target.accept(active(loud));
    }
    render(&mut view, &mut terminal, true);
    assert!(!view.sparks.is_active(), "one frame throws nothing");
    for levels in [quiet, loud] {
      for target in [&mut view, &mut bars] {
        target.accept(active(levels));
      }
    }
    // Freeze time so both views draw identical bars to compare against.
    let now = std::time::Instant::now();
    for target in [&mut view, &mut bars] {
      target.updated = now;
    }
    render(&mut view, &mut terminal, true);
    bars.updated = view.updated;
    render(&mut bars, &mut plain, true);
    assert!(view.sparks.is_active(), "a rise between frames throws sparks");
    let (sparked, plain) = (terminal.backend().buffer(), plain.backend().buffer());
    let mut sparks = 0;
    // Body rows y = 1..=11, between the title and the axis.
    for (x, y) in (1..=11).flat_map(|y| (1..=62).map(move |x| (x, y))) {
      if sparked[(x, y)].symbol().chars().all(|c| ('\u{2801}'..='\u{28FF}').contains(&c)) {
        sparks += 1;
        assert_eq!(plain[(x, y)].symbol(), " ", "sparks only use cells the bars leave blank");
      } else {
        assert_eq!(sparked[(x, y)], plain[(x, y)], "cell {x},{y}");
      }
    }
    assert!(sparks > 0, "expected sparks above the bars");
    view.accept(SpectrumFrame { generation: 1, ..active(quiet) });
    assert!(!view.sparks.is_active(), "a rise across a generation change compares nothing");
    view.accept(SpectrumFrame { generation: 1, ..active(loud) });
    age(&mut view);
    render(&mut view, &mut terminal, true);
    assert!(view.sparks.is_active(), "consecutive frames of the new stream count");
    settle(&mut view, &mut terminal);
    assert!(!view.sparks.is_active(), "sparks cool and fade");
    assert!(!view.needs_animation());
  }

  #[test]
  fn switching_styles_keeps_levels_and_history_but_clears_particles() {
    let mut view = styled(SpectrumStyle::Bars);
    let mut terminal = backend(40, 12);
    view.accept(active([0.8; BANDS]));
    render(&mut view, &mut terminal, true);
    assert!(view.levels.iter().all(|value| *value > 0.0));
    view.set_style(SpectrumStyle::Waterfall);
    assert_eq!(view.style, SpectrumStyle::Waterfall);
    assert!(view.needs_animation(), "a style change redraws once");
    assert_eq!(view.history.len(), 1, "history was collected while in bars");
    assert!(view.levels.iter().all(|value| *value > 0.0));
    // Fire heat and sparks are drawing state, not audio: a switch starts them over.
    view.set_style(SpectrumStyle::Fire);
    age(&mut view);
    render(&mut view, &mut terminal, true);
    assert!(view.fire.is_hot());
    view.set_style(SpectrumStyle::Sparks);
    assert!(!view.fire.is_hot(), "leaving fire puts it out");
    assert!(view.levels.iter().all(|value| *value > 0.0), "the audio levels carry over");
    assert_eq!(view.history.len(), 1);
  }

  #[test]
  fn clearing_the_stream_puts_out_the_particles() {
    let mut view = styled(SpectrumStyle::Fire);
    let mut terminal = backend(40, 12);
    view.accept(SpectrumFrame { generation: 1, ..active(bands_at(0..8, 1.0)) });
    for _ in 0..4 {
      age(&mut view);
      render(&mut view, &mut terminal, true);
    }
    assert!(view.fire.is_hot());
    view.clear();
    assert!(!view.fire.is_hot(), "stopping playback must not leave a burning pane");
    assert!(view.history.is_empty());
  }

  #[test]
  fn the_axis_names_real_frequencies_and_falls_back_to_ends_when_narrow() {
    let mut view = styled(SpectrumStyle::Bars);
    let mut frame = active([0.5; BANDS]);
    frame.low_hz = 40.0;
    frame.high_hz = 16_000.0;
    view.accept(frame);
    let mut terminal = backend(40, 12);
    render(&mut view, &mut terminal, true);
    // Body rows y = 1..=9, so the axis is row 10.
    let axis = row_symbols(&terminal, 10);
    assert!(axis.contains("100"), "{axis:?}");
    assert!(axis.contains("1k"), "{axis:?}");
    assert!(!axis.contains("LOW"), "{axis:?}");
    // Too narrow for decade labels: the plain ends keep the row meaningful.
    let mut narrow = backend(10, 12);
    render(&mut view, &mut narrow, true);
    let axis = row_symbols(&narrow, 10);
    assert!(axis.contains("LOW"), "{axis:?}");
    assert!(!axis.contains("1k"), "{axis:?}");
    // Without any frame there is no scale to name yet.
    view.clear();
    render(&mut view, &mut terminal, true);
    assert!(row_symbols(&terminal, 10).contains("LOW"));
  }

  #[test]
  fn every_style_draws_every_size_and_cell_size_and_then_settles() {
    let ramp: [f32; BANDS] = std::array::from_fn(|band| band as f32 / (BANDS - 1) as f32);
    let sizes = [(1, 1), (40, 3), (40, 4), (40, 6), (40, 12), (61, 12), (120, 40)];
    // Terminals disagree about the cell size, and some report none at all.
    let cells = [CELL, (0, 0), (7, 15)];
    for style in SpectrumStyle::ALL {
      for (width, height) in sizes {
        for cell in cells {
          let mut view = styled(style);
          let mut terminal = backend(width, height);
          let mut draw = |view: &mut SpectrumView, playing: bool| {
            terminal.draw(|f| view.draw(f, f.area(), &THEMES[0], playing, cell)).expect("draw");
          };
          for step in 0..6 {
            view.accept(active(if step % 2 == 0 { ramp } else { [0.2; BANDS] }));
            age(&mut view);
            draw(&mut view, true);
          }
          // Pausing must bring every style to a halt, or yp keeps drawing at
          // full rate over a stopped track.
          settle(&mut view, &mut terminal);
          assert!(!view.needs_animation(), "{} at {width}x{height} cell {cell:?} must settle", style.id());
        }
      }
    }
  }

  #[test]
  fn every_style_draws_every_theme() {
    // A theme with an odd palette must not break blending or glyph choice.
    for theme in THEMES {
      for style in SpectrumStyle::ALL {
        let mut view = styled(style);
        view.accept(active([0.7; BANDS]));
        for (width, height) in [(61, 12), (6, 5), (3, 20), (80, 30)] {
          let mut terminal = backend(width, height);
          terminal.draw(|f| view.draw(f, f.area(), theme, true, CELL)).expect("draw");
        }
      }
    }
  }
}
