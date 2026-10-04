use image::DynamicImage;
use image::imageops::FilterType;
use ratatui::{
  Frame,
  layout::{Alignment, Constraint, Layout, Rect},
  style::{Color, Modifier, Style, Stylize},
  text::{Line, Span},
  widgets::{Block, Clear, Gauge, List, ListItem, Padding, Paragraph, Wrap},
};

use crate::app::{App, AppMode};
use crate::constants::constants;
use crate::display::DisplayMode;
use crate::graphics::ThumbnailWidget;
use crate::theme::Theme;
use crate::transcript::TranscriptState;

// --- Helpers ---

/// Dim an RGB color by the given factor (0.0 = black, 1.0 = unchanged).
/// Non-RGB colors are returned as-is.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn dim_color(color: Color, factor: f32) -> Color {
  match color {
    Color::Rgb(r, g, b) => Color::Rgb(
      (f32::from(r) * factor).clamp(0.0, 255.0) as u8,
      (f32::from(g) * factor).clamp(0.0, 255.0) as u8,
      (f32::from(b) * factor).clamp(0.0, 255.0) as u8,
    ),
    other => other,
  }
}

/// Compute the display width of the first `n` chars (accounting for double-width CJK).
pub fn display_width(s: &str, n: usize) -> usize {
  use unicode_width::UnicodeWidthChar;
  s.chars().take(n).map(|c| c.width().unwrap_or(0)).sum()
}

/// Truncate a string to `max_width` characters, appending "…" if truncated.
fn truncate_str(s: &str, max_width: usize) -> String {
  if s.chars().count() <= max_width {
    s.to_string()
  } else {
    let truncated: String = s.chars().take(max_width.saturating_sub(1)).collect();
    format!("{truncated}…")
  }
}

/// Split `text` into spans with case-insensitive highlighting of all `needle` occurrences.
///
/// Uses char-based position mapping for Unicode safety. If `to_lowercase()` changes
/// the char count (e.g. Turkish İ → i + combining dot), falls back to no highlighting
/// since byte/char positions would be unreliable.
///
/// Returns owned `Span<'static>` so the result can outlive the input string.
pub fn highlight_text(text: &str, needle: &str, normal_style: Style, match_style: Style) -> Vec<Span<'static>> {
  if needle.is_empty() {
    return vec![Span::styled(text.to_string(), normal_style)];
  }

  let text_lower = text.to_lowercase();
  let needle_lower = needle.to_lowercase();

  // Safety: if lowercasing changed char counts, positions won't map correctly.
  if text.chars().count() != text_lower.chars().count() {
    return vec![Span::styled(text.to_string(), normal_style)];
  }

  // Find all match positions (char indices) in the lowercased text
  let needle_char_len = needle_lower.chars().count();
  let text_lower_chars: Vec<char> = text_lower.chars().collect();
  let needle_chars: Vec<char> = needle_lower.chars().collect();
  let mut matches: Vec<(usize, usize)> = Vec::new(); // (start_char, end_char)

  if needle_char_len > text_lower_chars.len() {
    return vec![Span::styled(text.to_string(), normal_style)];
  }

  for i in 0..=text_lower_chars.len() - needle_char_len {
    if text_lower_chars[i..i + needle_char_len] == needle_chars[..] {
      matches.push((i, i + needle_char_len));
    }
  }

  if matches.is_empty() {
    return vec![Span::styled(text.to_string(), normal_style)];
  }

  // Build spans by splitting the original text at match boundaries
  let text_chars: Vec<char> = text.chars().collect();
  let mut spans = Vec::new();
  let mut pos = 0;

  for (start, end) in &matches {
    if pos < *start {
      let segment: String = text_chars[pos..*start].iter().collect();
      spans.push(Span::styled(segment, normal_style));
    }
    let segment: String = text_chars[*start..*end].iter().collect();
    spans.push(Span::styled(segment, match_style));
    pos = *end;
  }

  if pos < text_chars.len() {
    let segment: String = text_chars[pos..].iter().collect();
    spans.push(Span::styled(segment, normal_style));
  }

  spans
}

// --- Size guard ---

pub const MIN_TERM_WIDTH: u16 = 60;
pub const MIN_TERM_HEIGHT: u16 = 12;

/// Render a "terminal too small" message and return `true` if the terminal is too small.
fn render_size_guard(frame: &mut Frame, theme: &Theme) -> bool {
  let area = frame.area();
  if area.width >= MIN_TERM_WIDTH && area.height >= MIN_TERM_HEIGHT {
    return false;
  }
  frame.render_widget(Clear, area);
  let msg = format!(
    "Terminal too small ({}\u{00d7}{}). Need at least {MIN_TERM_WIDTH}\u{00d7}{MIN_TERM_HEIGHT}.",
    area.width, area.height,
  );
  let p = Paragraph::new(msg).style(Style::default().fg(theme.error)).block(
    Block::bordered()
      .border_type(ratatui::widgets::BorderType::Rounded)
      .border_style(Style::default().fg(theme.border)),
  );
  frame.render_widget(p, area);
  true
}

// --- UI Rendering ---

pub fn ui(frame: &mut Frame, app: &mut App) {
  let theme = app.theme();
  app.gfx.thumb_area = None;
  app.info_pane_area = None;

  if render_size_guard(frame, theme) {
    return;
  }

  frame.render_widget(Clear, frame.area());
  frame.render_widget(Block::default().style(Style::default().bg(theme.bg)), frame.area());

  // PiP mode: compact layout showing only Now Playing + status
  if app.pip_mode {
    render_pip(frame, app);
    return;
  }

  let [header_area, main_area, status_area, input_area, footer_area] = Layout::vertical([
    Constraint::Length(1),
    Constraint::Min(3),
    Constraint::Length(1),
    Constraint::Length(3),
    Constraint::Length(1),
  ])
  .areas(frame.area());

  render_header(frame, theme, header_area);
  render_main(frame, app, main_area);
  render_status(frame, app, status_area);
  render_input(frame, app, input_area);
  render_footer(frame, app, footer_area);
}

#[allow(clippy::cast_possible_truncation)]
fn render_header(frame: &mut Frame, theme: &Theme, area: Rect) {
  let left = Line::from(Span::styled(" ▶ yp ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
  frame.render_widget(left, area);

  let version = format!("v{} ", env!("CARGO_PKG_VERSION"));
  let version_w = (version.len().min(u16::MAX as usize)) as u16;
  let right = Line::from(Span::styled(&version, Style::default().fg(theme.muted)));
  let right_area = Rect { x: area.x.saturating_add(area.width.saturating_sub(version_w)), width: version_w, ..area };
  frame.render_widget(right, right_area);
}

/// `PiP` mode layout: thumbnail filling the window + status bar.
/// Designed for a small (~550x350px) terminal window showing album art.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
fn render_pip(frame: &mut Frame, app: &mut App) {
  let theme = app.theme();

  let [status_area, main_area, hint_area] = Layout::vertical([
    Constraint::Length(1), // mpv status bar
    Constraint::Min(3),    // thumbnail
    Constraint::Length(1), // PiP hint
  ])
  .areas(frame.area());

  // Status bar
  render_status(frame, app, status_area);

  // Shared with the split layout so both letterbox identically.
  render_thumbnail_pane(frame, app, main_area, theme);

  // Bottom hint
  let hint = Line::from(Span::styled(" [Ctrl+M] exit PiP", Style::default().fg(theme.muted)));
  frame.render_widget(hint, hint_area);
}

/// Draws the thumbnail inside a bordered pane.
///
/// Shared by the split Now Playing layout and by PiP. These two used to carry
/// separate copies of the aspect arithmetic, which is how PiP kept the squashing
/// bug after it was fixed in the main layout.
fn render_thumbnail_pane(frame: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
  let glow_color = dim_color(theme.accent, 0.35);
  let glow_block =
    Block::bordered().border_type(ratatui::widgets::BorderType::Rounded).border_style(Style::default().fg(glow_color));
  frame.render_widget(glow_block, area);

  // Inside the glow border: 1 cell inset on all sides.
  let mut thumb_area = Rect {
    x: area.x.saturating_add(1),
    y: area.y.saturating_add(1),
    width: area.width.saturating_sub(2),
    height: area.height.saturating_sub(2),
  };

  let Some((ref video_id, ref image)) = app.player.cached_thumbnail else {
    return;
  };

  // Kitty and Sixel take the whole pane: the terminal scales the image into the
  // given cells using its own pixel metrics and letterboxes the result.
  // Deriving a cell height instead is guesswork about cell aspect, and getting
  // it wrong pushes the image outside its frame.
  //
  // Buffer modes blit pixels themselves, so they must letterbox by hand.
  let protocol_mode = matches!(app.player.display_mode, DisplayMode::Kitty | DisplayMode::Sixel);
  if !protocol_mode {
    let ideal_h = ideal_thumb_height(app.player.display_mode, image, thumb_area.width);
    if ideal_h < thumb_area.height {
      let diff = thumb_area.height.saturating_sub(ideal_h);
      thumb_area.y = thumb_area.y.saturating_add(diff / 2);
      thumb_area.height = ideal_h;
    }
  }

  if protocol_mode {
    // Kitty/Sixel: rendering is handled outside ratatui, in the run loop.
    // Record the pane and skip the resize and widget render that only the
    // buffer modes use.
    app.gfx.thumb_area = Some(thumb_area);
    return;
  }

  let needs_resize = match &app.gfx.resized_thumb {
    Some((id, w, h, _)) => id != video_id || *w != thumb_area.width || *h != thumb_area.height,
    None => true,
  };
  if needs_resize {
    let target_w = u32::from(thumb_area.width);
    // Half-block glyphs render two pixel rows per cell, so one cell of height
    // is two pixel rows. Matching the placement rect keeps the blitted pixels
    // the same shape as the area they land in.
    let target_h = u32::from(thumb_area.height) * 2;
    let resized = image.resize_to_fill(target_w, target_h.max(1), FilterType::Lanczos3);
    app.gfx.resized_thumb = Some((video_id.clone(), thumb_area.width, thumb_area.height, resized));
  }
  if let Some((_, _, _, ref resized)) = app.gfx.resized_thumb {
    let widget = ThumbnailWidget { image: resized, display_mode: app.player.display_mode };
    frame.render_widget(widget, thumb_area);
  }
}

fn render_main(frame: &mut Frame, app: &mut App, area: Rect) {
  if matches!(app.mode, AppMode::Results | AppMode::Filter) && !app.search_results.is_empty() {
    render_results(frame, app, area);
  } else if app.player.is_playing() {
    render_player(frame, app, area);
  } else {
    render_welcome(frame, app.theme(), area);
  }
}

fn render_welcome(frame: &mut Frame, theme: &Theme, area: Rect) {
  let text = vec![
    Line::from(""),
    Line::from(Span::styled("▶  Welcome to yp", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
    Line::from(""),
    Line::from(Span::styled(
      "YouTube player with thumbnails, transcription, and channel browsing.",
      Style::default().fg(theme.fg),
    )),
    Line::from(""),
    Line::from(Span::styled("Type a search query or @channel below.", Style::default().fg(theme.muted))),
  ];
  let paragraph = Paragraph::new(text).alignment(Alignment::Center).block(
    Block::bordered()
      .border_type(ratatui::widgets::BorderType::Rounded)
      .border_style(Style::default().fg(theme.border)),
  );
  frame.render_widget(paragraph, area);
}

#[allow(clippy::too_many_lines, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
/// Rows the metadata needs to stay legible: border, title, uploader, and URL.
const MIN_METADATA_HEIGHT: u16 = 8;
/// Smallest spectrum pane worth drawing: border, axis labels, and bar rows.
const MIN_SPECTRUM_HEIGHT: u16 = 7;
/// The spectrum never takes more than this share, so it cannot crowd out the
/// text that identifies the track.
const MAX_SPECTRUM_SHARE_PERCENT: u16 = 45;

/// Height in cells that renders an image without distorting it.
///
/// Only meaningful for the buffer modes, which blit pixels into cells. Kitty
/// and Sixel let the terminal do the scaling, so they use the whole pane
/// instead.
///
/// Terminal cells are roughly twice as tall as they are wide, so a cell count
/// is about half the visual height: an image with a 16:9 *visual* aspect needs
/// a rect about 32:9 in *cells*. Direct mode uses half-block glyphs, which map
/// one pixel row per half row, so it needs the same compensation.
fn ideal_thumb_height(display_mode: DisplayMode, image: &DynamicImage, width: u16) -> u16 {
  let image_width = image.width();
  if image_width == 0 {
    return u16::MAX;
  }
  let scale = width as f32 * image.height() as f32 / image_width as f32 / CELL_TALLNESS;
  // Ignore the mode beyond documentation: both buffer modes share the same
  // cell geometry. Taking it as a parameter keeps the intent explicit.
  let _ = display_mode;
  scale.round().max(1.0) as u16
}

/// A terminal cell is about twice as tall as it is wide.
const CELL_TALLNESS: f32 = 2.0;

/// Splits the Now Playing pane into metadata and spectrum.
///
/// The spectrum is reserved first, with a floor, because metadata grows with
/// tags: giving the spectrum only the leftover rows meant it was never drawn in
/// a normal terminal. Metadata takes the remainder and clips, which is the
/// graceful direction to lose rows.
fn split_now_playing(area: Rect, want_spectrum: bool) -> (Rect, Option<Rect>) {
  if !want_spectrum || area.height < MIN_METADATA_HEIGHT + MIN_SPECTRUM_HEIGHT {
    return (area, None);
  }
  let share = area.height * MAX_SPECTRUM_SHARE_PERCENT / 100;
  let spectrum_height = share.max(MIN_SPECTRUM_HEIGHT).min(area.height - MIN_METADATA_HEIGHT);
  let [metadata, spectrum] = Layout::vertical([Constraint::Min(0), Constraint::Length(spectrum_height)]).areas(area);
  (metadata, Some(spectrum))
}

fn render_player(frame: &mut Frame, app: &mut App, area: Rect) {
  let theme = app.theme();
  let left_pct = (app.split * 100.0).round().clamp(20.0, 80.0) as u16;
  let right_pct = 100 - left_pct;
  let [thumb_layout_area, info_area] =
    Layout::horizontal([Constraint::Percentage(left_pct), Constraint::Percentage(right_pct)]).areas(area);
  app.info_pane_area = Some(info_area);

  render_thumbnail_pane(frame, app, thumb_layout_area, theme);

  let show_transcript =
    app.transcript_visible && (!app.utterances.is_empty() || !matches!(app.transcript_state, TranscriptState::Idle));

  let (np_area, transcript_area) = if show_transcript && !app.wiki_visible {
    let [top, bottom] = Layout::vertical([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(info_area);
    (top, Some(bottom))
  } else {
    (info_area, None)
  };

  // Wiki pane replaces the info pane when visible
  if app.wiki_visible {
    render_wiki(frame, app, np_area);
    return;
  }

  let info_title = Line::from(vec![
    Span::styled(" Now Playing ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
    Span::styled(format!("[{}] ", app.player.display_mode.label().to_lowercase()), Style::default().fg(theme.muted)),
  ]);
  let info_block = Block::bordered()
    .title(info_title)
    .border_type(ratatui::widgets::BorderType::Rounded)
    .border_style(Style::default().fg(theme.border))
    .padding(Padding::horizontal(1))
    .style(Style::default().bg(theme.panel_bg));

  if let Some(details) = &app.player.current_details {
    let inner_w = np_area.width.saturating_sub(4) as usize;

    let mut lines = vec![
      Line::from(""),
      Line::from(Span::styled(
        truncate_str(&details.title, inner_w),
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
      )),
      Line::from(""),
    ];
    if let Some(uploader) = &details.uploader {
      let label = "Uploader  ";
      let value_w = inner_w.saturating_sub(label.len());
      lines.push(Line::from(vec![
        Span::styled(label, Style::default().fg(theme.muted)),
        Span::styled(truncate_str(uploader, value_w), Style::default().fg(theme.fg)),
      ]));
    }
    if let Some(duration) = &details.duration {
      lines.push(Line::from(vec![
        Span::styled("Duration  ", Style::default().fg(theme.muted)),
        Span::styled(duration.as_str(), Style::default().fg(theme.fg)),
      ]));
    }
    if let Some(date) = &details.upload_date {
      lines.push(Line::from(vec![
        Span::styled("Published ", Style::default().fg(theme.muted)),
        Span::styled(date.as_str(), Style::default().fg(theme.fg)),
      ]));
    }
    if let Some(views) = &details.view_count {
      lines.push(Line::from(vec![
        Span::styled("Views     ", Style::default().fg(theme.muted)),
        Span::styled(views.as_str(), Style::default().fg(theme.fg)),
      ]));
    }
    lines.push(Line::from(""));
    let url_display = truncate_str(&details.url, inner_w);
    lines.push(Line::from(Span::styled(
      url_display,
      Style::default().fg(theme.accent).add_modifier(Modifier::UNDERLINED),
    )));
    if !details.tags.is_empty() {
      lines.push(Line::from(""));
      lines.push(Line::from(Span::styled("Tags", Style::default().fg(theme.muted))));
      for tag in details.tags.iter().take(constants().max_display_tags) {
        lines.push(Line::from(Span::styled(
          format!("  {}", truncate_str(tag, inner_w.saturating_sub(2))),
          Style::default().fg(theme.tag),
        )));
      }
    }

    let paragraph = Paragraph::new(lines).block(info_block);
    let (metadata_area, spectrum_area) = split_now_playing(np_area, app.spectrum_visible());
    frame.render_widget(paragraph, metadata_area);
    if let Some(spectrum_area) = spectrum_area {
      app.draw_spectrum(frame, spectrum_area);
    }
  } else {
    frame.render_widget(info_block, np_area);
  }

  // --- Transcript area (bottom 35% of info pane) ---
  if let Some(t_area) = transcript_area {
    render_transcript(frame, app, t_area);
  }
}

#[allow(
  clippy::too_many_lines,
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  clippy::cast_precision_loss,
  clippy::cast_possible_wrap
)]
fn render_transcript(frame: &mut Frame, app: &App, area: Rect) {
  let theme = app.theme();

  let mut block = Block::bordered()
    .border_type(ratatui::widgets::BorderType::Rounded)
    .border_style(Style::default().fg(theme.border))
    .padding(Padding::horizontal(1))
    .style(Style::default().bg(theme.panel_bg));

  // Show model download progress bar
  if let Some((downloaded, total)) = app.download_progress
    && total > 0
  {
    let ratio = (downloaded as f64 / total as f64).min(1.0);
    let downloaded_mb = downloaded as f64 / (1024.0 * 1024.0);
    let total_mb = total as f64 / (1024.0 * 1024.0);
    let label = format!("{downloaded_mb:.0} / {total_mb:.0} MB");

    let gauge = Gauge::default()
      .block(block)
      .gauge_style(Style::default().fg(theme.accent).bg(theme.border))
      .ratio(ratio)
      .label(Span::styled(label, Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)));
    frame.render_widget(gauge, area);
    return;
  }

  if app.utterances.is_empty() {
    // Show animated progress indicator when transcript pipeline is active
    let is_busy = !matches!(app.transcript_state, TranscriptState::Idle);
    if is_busy {
      let inner = block.inner(area);
      frame.render_widget(block, area);

      // Build pulsing ..:..:..:..:.. pattern
      let pattern = "..:..:..:..:..";
      let pattern_len = pattern.len();
      let elapsed_ms = app.started_at.elapsed().as_millis() as usize;
      // Shift the "lit" position every 150ms
      let phase = (elapsed_ms / 150) % pattern_len;

      let accent = theme.accent;
      let muted = dim_color(theme.border, 0.5);

      let spans: Vec<Span> = pattern
        .chars()
        .enumerate()
        .map(|(i, c)| {
          // Light up 3 adjacent chars in a sliding window
          let dist = ((i as isize) - (phase as isize)).unsigned_abs();
          let dist = dist.min(pattern_len.abs_diff(dist)); // wrap around
          let color = if dist == 0 {
            accent
          } else if dist <= 2 {
            dim_color(accent, 0.6)
          } else {
            muted
          };
          Span::styled(c.to_string(), Style::default().fg(color))
        })
        .collect();

      // Center vertically and horizontally
      let mid_y = inner.y.saturating_add(inner.height / 2);
      let text_w = pattern_len.min(u16::MAX as usize) as u16;
      let mid_x = inner.x.saturating_add(inner.width.saturating_sub(text_w) / 2);
      let indicator_area = Rect { x: mid_x, y: mid_y, width: text_w.min(inner.width), height: 1 };
      frame.render_widget(Line::from(spans), indicator_area);
    } else {
      frame.render_widget(block, area);
    }
    return;
  }

  let title = Line::from(Span::styled(" Transcript ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
  block = block.title(title);

  // Determine current playback time for highlighting
  let current_time_cs: Option<i64> = app.player.position_secs().map(|secs| (secs * 100.0) as i64); // Convert seconds to centiseconds

  // Find the active utterance index
  let active_idx: Option<usize> =
    current_time_cs.and_then(|t| app.utterances.iter().position(|u| t >= u.start && t < u.stop));

  let mut lines: Vec<Line> = Vec::new();
  // Track which line index in `lines` corresponds to the active utterance
  let mut active_line_idx: Option<usize> = None;
  for (i, utterance) in app.utterances.iter().enumerate() {
    let text = utterance.text.trim();
    if text.is_empty() {
      continue;
    }

    let is_active = active_idx == Some(i);

    let style = if is_active {
      Style::default().fg(theme.highlight_fg).bg(theme.highlight_bg).add_modifier(Modifier::BOLD)
    } else {
      Style::default().fg(theme.muted)
    };

    if is_active {
      active_line_idx = Some(lines.len());
    }

    lines.push(Line::from(Span::styled(text.to_string(), style)));

    // Add blank line between utterances (except after last)
    if i < app.utterances.len().saturating_sub(1) {
      lines.push(Line::from(""));
    }
  }

  // Auto-scroll: compute total visual lines after word-wrap,
  // then scroll so the currently active line is centered vertically.
  let inner_height = area.height.saturating_sub(2) as usize;
  let inner_width = area.width.saturating_sub(4).max(1) as usize;

  // Find the visual line offset of the active utterance for smart scrolling.
  // Use unicode display width (not byte length) to match ratatui's Wrap behavior.
  let mut total_visual_lines: usize = 0;
  let mut active_visual_line: usize = 0;
  let mut active_visual_rows: usize = 0;
  for (line_i, line) in lines.iter().enumerate() {
    let line_width: usize = line.spans.iter().map(|s| display_width(&s.content, s.content.chars().count())).sum();
    let visual_rows = if line_width == 0 { 1 } else { (line_width.saturating_sub(1) / inner_width).saturating_add(1) };

    if active_line_idx == Some(line_i) {
      active_visual_line = total_visual_lines;
      active_visual_rows = visual_rows;
    }
    total_visual_lines += visual_rows;
  }

  // Scroll to keep active line centered, falling back to bottom-scroll
  let scroll = if active_line_idx.is_some() {
    // Place the middle of the active utterance at the middle of the pane
    let active_center = active_visual_line.saturating_add(active_visual_rows / 2);
    active_center.saturating_sub(inner_height / 2).min(u16::MAX as usize) as u16
  } else {
    // No active line — scroll to bottom
    total_visual_lines.saturating_sub(inner_height).min(u16::MAX as usize) as u16
  };

  let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false }).scroll((scroll, 0));
  frame.render_widget(paragraph, area);
}

#[allow(clippy::too_many_lines)]
fn render_wiki(frame: &mut Frame, app: &mut App, area: Rect) {
  let theme = app.theme();
  let inner_w = area.width.saturating_sub(4) as usize;

  // Tab indicator in title
  let (detail_style, raw_style) = match app.wiki_tab {
    crate::app::WikiTab::Detail => {
      (Style::default().fg(theme.accent).add_modifier(Modifier::BOLD), Style::default().fg(theme.muted))
    }
    crate::app::WikiTab::Raw => {
      (Style::default().fg(theme.muted), Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))
    }
  };
  let title = Line::from(vec![
    Span::styled(" Detail ", detail_style),
    Span::styled("│", Style::default().fg(theme.border)),
    Span::styled(" Raw ", raw_style),
  ]);
  let block = Block::bordered()
    .title(title)
    .border_type(ratatui::widgets::BorderType::Rounded)
    .border_style(Style::default().fg(theme.accent))
    .padding(Padding::horizontal(1))
    .style(Style::default().bg(theme.panel_bg));

  // Raw tab: raw transcript content (independent of wiki detail)
  if app.wiki_tab == crate::app::WikiTab::Raw {
    if app.tasks.wiki_raw_rx.is_some() {
      let lines =
        vec![Line::from(""), Line::from(Span::styled("Loading transcript…", Style::default().fg(theme.muted)))];
      let paragraph = Paragraph::new(lines).block(block).alignment(Alignment::Center);
      frame.render_widget(paragraph, area);
      return;
    }
    let Some(ref raw) = app.wiki_raw else {
      let lines = vec![
        Line::from(""),
        Line::from(Span::styled("No raw transcript for this video.", Style::default().fg(theme.muted))),
      ];
      let paragraph = Paragraph::new(lines).block(block).alignment(Alignment::Center);
      frame.render_widget(paragraph, area);
      return;
    };
    let lines: Vec<Line> =
      raw.lines().map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(theme.fg)))).collect();
    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false }).scroll((app.wiki_scroll, 0));
    frame.render_widget(paragraph, area);
    return;
  }

  if app.tasks.wiki_rx.is_some() {
    // Loading state
    let lines = vec![Line::from(""), Line::from(Span::styled("Loading wiki…", Style::default().fg(theme.muted)))];
    let paragraph = Paragraph::new(lines).block(block).alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
    return;
  }

  let Some(ref detail) = app.wiki_detail else {
    let lines =
      vec![Line::from(""), Line::from(Span::styled("No wiki entry for this video.", Style::default().fg(theme.muted)))];
    let paragraph = Paragraph::new(lines).block(block).alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
    return;
  };

  let mut lines: Vec<Line> = vec![
    Line::from(""),
    Line::from(Span::styled("Summary", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
    Line::from(""),
    Line::from(Span::styled(detail.summary.clone(), Style::default().fg(theme.fg))),
  ];

  // Takeaways
  if !detail.takeaways.is_empty() {
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Takeaways", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(""));
    for takeaway in &detail.takeaways {
      lines.push(Line::from(Span::styled(
        format!("• {}", truncate_str(takeaway, inner_w.saturating_sub(2).max(1) * 4)),
        Style::default().fg(theme.fg),
      )));
      lines.push(Line::from(""));
    }
  }

  // Topics
  if !detail.topics.is_empty() {
    lines.push(Line::from(Span::styled("Topics", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(""));
    let topics_str = detail.topics.join(", ");
    lines.push(Line::from(Span::styled(truncate_str(&topics_str, inner_w * 3), Style::default().fg(theme.tag))));
  }

  // Related
  if !detail.related.is_empty() {
    lines.push(Line::from(""));
    lines
      .push(Line::from(Span::styled("Related [1-5]", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(""));
    for (i, rel) in detail.related.iter().take(5).enumerate() {
      let num = format!("{} ", i + 1);
      let title_text = rel.title.as_deref().unwrap_or(&rel.id);
      let topics =
        if rel.shared_topics.is_empty() { String::new() } else { format!("  ({})", rel.shared_topics.join(", ")) };
      lines.push(Line::from(vec![
        Span::styled(num, Style::default().fg(theme.key_bg)),
        Span::styled(truncate_str(title_text, inner_w.saturating_sub(3)), Style::default().fg(theme.status)),
      ]));
      if !topics.is_empty() {
        lines.push(Line::from(Span::styled(
          truncate_str(&format!("  {topics}"), inner_w),
          Style::default().fg(theme.muted),
        )));
      }
    }
  }

  let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false }).scroll((app.wiki_scroll, 0));
  frame.render_widget(paragraph, area);
}

#[allow(clippy::too_many_lines)]
fn render_results(frame: &mut Frame, app: &mut App, area: Rect) {
  let theme = app.theme();

  let is_channel = app.channel_source.is_some();
  let loading_more = app.channel_source.as_ref().is_some_and(|s| s.loading_more);
  let is_filtering = !app.filter.is_empty();
  let filter_needle = &app.filter;

  // Inner width: area minus 2 borders minus 2 chars for highlight symbol ("▶ ")
  let inner_w = area.width.saturating_sub(4) as usize;

  // Style for highlighted keyword matches
  let match_style = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);

  let items: Vec<ListItem> = app
    .filtered_indices
    .iter()
    .enumerate()
    .filter_map(|(display_idx, &actual_idx)| {
      let entry = app.search_results.get(actual_idx)?;
      let is_selected = Some(display_idx) == app.list_state.selected();
      let fg = if is_selected { theme.highlight_fg } else { theme.fg };
      let bg = if is_selected {
        theme.highlight_bg
      } else if display_idx % 2 == 1 {
        theme.stripe_bg
      } else {
        theme.bg
      };
      let normal_style = Style::default().fg(fg);

      // Build right-side metadata: "tags  date" or just "date" or just "tags"
      let date_str = entry.upload_date.as_deref().unwrap_or("");
      let tags_limited: String;
      let tags_str = match entry.tags.as_deref() {
        Some(raw) if !raw.is_empty() => {
          let half_w = inner_w / 2;
          let parts: Vec<&str> = raw.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
          // Take up to max_display_tags, then shrink further if the joined string exceeds half the line width.
          let max = constants().max_display_tags.min(parts.len());
          let mut count = max;
          loop {
            let candidate = parts[..count].join(", ");
            if candidate.len() <= half_w || count <= 1 {
              tags_limited = candidate;
              break;
            }
            count = count.saturating_sub(2).max(1);
          }
          tags_limited.as_str()
        }
        _ => "",
      };
      let right = match (!tags_str.is_empty(), !date_str.is_empty()) {
        (true, true) => format!("{tags_str}  {date_str}"),
        (true, false) => tags_str.to_string(),
        (false, true) => date_str.to_string(),
        (false, false) => String::new(),
      };

      let line = if right.is_empty() {
        let title = truncate_str(&entry.title, inner_w);
        if is_filtering {
          Line::from(highlight_text(&title, filter_needle, normal_style, match_style))
        } else {
          Line::from(Span::styled(title, normal_style))
        }
      } else {
        // Reserve space for right side + 2-char gap
        let right_w = right.chars().count();
        let title_max = inner_w.saturating_sub(right_w).saturating_sub(2);
        let title = truncate_str(&entry.title, title_max);
        let title_w = title.chars().count();
        let gap = inner_w.saturating_sub(title_w).saturating_sub(right_w);

        let padding: String = " ".repeat(gap);

        let mut spans = if is_filtering {
          highlight_text(&title, filter_needle, normal_style, match_style)
        } else {
          vec![Span::styled(title, normal_style)]
        };

        spans.push(Span::raw(padding));

        // Split right into tags and date parts for separate styling, with highlighting
        let muted_style = Style::default().fg(theme.muted);
        let muted_match_style = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
        if !tags_str.is_empty() && !date_str.is_empty() {
          if is_filtering {
            spans.extend(highlight_text(tags_str, filter_needle, muted_style, muted_match_style));
          } else {
            spans.push(Span::styled(tags_str.to_string(), muted_style));
          }
          spans.push(Span::raw("  "));
          spans.push(Span::styled(date_str.to_string(), muted_style));
        } else if !tags_str.is_empty() {
          if is_filtering {
            spans.extend(highlight_text(tags_str, filter_needle, muted_style, muted_match_style));
          } else {
            spans.push(Span::styled(tags_str.to_string(), muted_style));
          }
        } else {
          spans.push(Span::styled(date_str.to_string(), muted_style));
        }
        Line::from(spans)
      };

      Some(ListItem::new(line).bg(bg))
    })
    .collect();

  let title = if is_filtering {
    let filtered = app.filtered_indices.len();
    let total = app.search_results.len();
    if is_channel {
      let suffix = if loading_more { " (loading more…)" } else { "" };
      format!(" Channel — /{} ({}/{} videos){} ", app.filter, filtered, total, suffix)
    } else {
      format!(" Filter: '{}' ({}/{}) ", app.filter, filtered, total)
    }
  } else if is_channel {
    let count = app.search_results.len();
    let suffix = if loading_more { " (loading more…)" } else { "" };
    format!(" Channel — {count} videos{suffix} ")
  } else {
    " Results ".to_string()
  };

  let list = List::new(items)
    .block(
      Block::bordered()
        .title(title)
        .title_style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(if app.mode == AppMode::Filter { theme.accent } else { theme.border })),
    )
    .highlight_symbol("▶ ")
    .highlight_style(Style::default().fg(theme.highlight_fg).bg(theme.highlight_bg).add_modifier(Modifier::BOLD));

  frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
  let theme = app.theme();

  let (text, style) = if let Some(msg) = &app.status_message {
    (format!(" ⏳ {msg}"), Style::default().fg(theme.status))
  } else if let Some(err) = &app.last_error {
    (format!(" ⚠  {err}"), Style::default().fg(theme.error))
  } else if let Some(msg) = &app.info_message {
    (format!(" ℹ  {msg}"), Style::default().fg(theme.muted))
  } else {
    // The clock comes from the device, not mpv, so the position shown is the
    // audio actually heard.
    match app.player.position_secs() {
      Some(secs) if app.player.is_playing() => {
        let icon = if app.player.paused { "⏸" } else { "▶" };
        let elapsed = crate::format_time(secs);
        let status = match app.player.current_details.as_ref().and_then(|d| d.duration.as_deref()) {
          Some(duration) => format!("{elapsed} / {duration}"),
          None => elapsed,
        };
        (format!(" ♪ {status} {icon}"), Style::default().fg(theme.status))
      }
      _ if app.player.is_playing() => (" ♪ Buffering...".to_string(), Style::default().fg(theme.muted)),
      _ => (" Ready".to_string(), Style::default().fg(theme.muted)),
    }
  };
  frame.render_widget(Paragraph::new(text).style(style), area);
}

#[allow(clippy::cast_possible_truncation)]
fn render_input(frame: &mut Frame, app: &mut App, area: Rect) {
  let theme = app.theme();
  let is_filter = app.mode == AppMode::Filter;

  // In filter mode, use filter fields; otherwise use input fields
  let (text, cursor_pos, scroll, title_text, is_active) = if is_filter {
    (&app.filter, app.filter_cursor, &mut app.filter_scroll, " Filter (title/tags) ", true)
  } else {
    (&app.input, app.cursor_position, &mut app.input_scroll, " Search YouTube ", app.mode == AppMode::Input)
  };

  let border_color = if is_active { theme.accent } else { theme.border };
  let input_block = Block::bordered()
    .title(title_text)
    .title_style(Style::default().fg(border_color))
    .border_type(ratatui::widgets::BorderType::Rounded)
    .border_style(Style::default().fg(border_color))
    .padding(Padding::horizontal(1));

  let inner_w = area.width.saturating_sub(4) as usize;
  let cursor_col = display_width(text, cursor_pos);

  if cursor_col < *scroll {
    *scroll = cursor_col;
  } else if cursor_col >= *scroll + inner_w {
    *scroll = cursor_col.saturating_sub(inner_w).saturating_add(1);
  }

  let scroll_val = *scroll;
  let visible: String = text
    .chars()
    .scan(0usize, |col, c| {
      let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
      let start = *col;
      *col += w;
      Some((start, *col, c))
    })
    .skip_while(|(_, end, _)| *end <= scroll_val)
    .take_while(|(start, _, _)| *start < scroll_val + inner_w)
    .map(|(_, _, c)| c)
    .collect();

  let paragraph = Paragraph::new(visible).style(Style::default().fg(theme.fg)).block(input_block);
  frame.render_widget(paragraph, area);

  if is_active {
    let cursor_offset = cursor_col.saturating_sub(scroll_val).min(u16::MAX as usize) as u16;
    let cursor_x = area.x.saturating_add(2).saturating_add(cursor_offset);
    frame.set_cursor_position((cursor_x, area.y.saturating_add(1)));
  }
}

#[allow(clippy::cast_possible_truncation)]
fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
  let theme = app.theme();
  let has_results = !app.search_results.is_empty();
  let is_playing = app.player.is_playing();
  let transcript_busy =
    matches!(app.transcript_state, TranscriptState::ExtractingAudio { .. } | TranscriptState::Transcribing { .. });
  let has_transcript = !app.utterances.is_empty() || transcript_busy;
  let transcript_hint = if transcript_busy {
    ("^a", "Cancel")
  } else if has_transcript {
    if app.transcript_visible { ("^a", "Hide") } else { ("^a", "Show") }
  } else {
    ("^a", "Transcript")
  };
  // The wiki bundle covers one channel only, so the shortcut is offered only
  // when the playing video can actually use it.
  let wiki_hint: Option<(&str, &str)> = if app.wiki_visible {
    Some(("^w", "Hide Wiki"))
  } else if app.wiki_available() {
    Some(("^w", "Wiki"))
  } else {
    None
  };
  let keys: Vec<(&str, &str)> = match app.mode {
    AppMode::Input => {
      let mut k = vec![("Enter", "Search"), ("^t", "Theme"), ("^f", "Frame")];
      if is_playing {
        // Playback keys work from here even though the search box has focus.
        let pause_label = if app.player.paused { "Resume" } else { "Pause" };
        k.push(("Space", pause_label));
        k.push(("←/→", "Seek"));
        k.push(("^v", "Spectrum"));
        k.push(transcript_hint);
        k.extend(wiki_hint);
        if crate::window::pip_supported() {
          k.push(("^m", "PiP"));
        }
        k.push(("^s", "Stop"));
        k.push(("^o", "Open"));
      }
      if has_results {
        k.push(("↓", "Results"));
        k.push(("Esc", "Results"));
      } else {
        k.push(("Esc", "Quit"));
      }
      k
    }
    AppMode::Results => {
      let mut k = vec![("Enter", "Play"), ("j/k", "Navigate"), ("/", "Filter")];
      if is_playing {
        k.push(transcript_hint);
        k.extend(wiki_hint);
        let pause_label = if app.player.paused { "Resume" } else { "Pause" };
        k.push(("Space", pause_label));
        k.push(("←/→", "Seek"));
        if crate::window::pip_supported() {
          k.push(("^m", "PiP"));
        }
        k.push(("^s", "Stop"));
        k.push(("^o", "Open"));
      }
      k.push(("^t", "Theme"));
      k.push(("^f", "Frame"));
      k.push(("^v", "Spectrum"));
      k.push(("Esc", "Back"));
      k
    }
    AppMode::Filter => {
      let mut k = vec![("Enter", "Apply"), ("Esc", "Clear"), ("↑↓", "Navigate")];
      if is_playing {
        k.push(transcript_hint);
        k.extend(wiki_hint);
        let pause_label = if app.player.paused { "Resume" } else { "Pause" };
        k.push(("Space", pause_label));
        k.push(("←/→", "Seek"));
        k.push(("^v", "Spectrum"));
        if crate::window::pip_supported() {
          k.push(("^m", "PiP"));
        }
        k.push(("^s", "Stop"));
        k.push(("^o", "Open"));
      }
      k
    }
  };

  let spans: Vec<Span> = keys
    .iter()
    .enumerate()
    .flat_map(|(i, (key, action))| {
      let mut s = vec![
        Span::styled(format!(" {key} "), Style::default().fg(theme.key_fg).bg(theme.key_bg)),
        Span::styled(format!(" {action}"), Style::default().fg(theme.muted)),
      ];
      if i < keys.len().saturating_sub(1) {
        s.push(Span::raw(" "));
      }
      s
    })
    .collect();

  frame.render_widget(Line::from(spans), area);

  let right_label = format!("{} | {} ", app.frame_mode.label(), theme.name);
  let right_w = (right_label.len().min(u16::MAX as usize)) as u16;
  let right = Line::from(Span::styled(&right_label, Style::default().fg(theme.muted)));
  let right_area = Rect { x: area.x.saturating_add(area.width.saturating_sub(right_w)), width: right_w, ..area };
  frame.render_widget(right, right_area);
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{app::App, display::DisplayMode, player::VideoDetails};
  use image::DynamicImage;
  use ratatui::{Terminal, backend::TestBackend, style::Color};

  /// Renders just the Now Playing pane and returns its rows of text.
  ///
  /// `render_player` is called directly because `render_main` gates on
  /// `player.is_playing()`, which needs a live mpv decoder.
  fn render_now_playing(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    let area = Rect { x: 0, y: 0, width, height };
    terminal.draw(|f| render_player(f, app, area)).expect("draw");
    let buffer = terminal.backend().buffer();
    (0..height).map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
  }

  /// A Now Playing pane with a full set of metadata, as a real track produces.
  fn playing_app(tags: usize) -> App {
    playing_app_from("Play&Enjoy", tags)
  }

  fn playing_app_from(uploader: &str, tags: usize) -> App {
    let mut app = App::new(DisplayMode::Ascii);
    app.player.current_details = Some(VideoDetails {
      url: "https://youtube.com/watch?v=abc".into(),
      title: "Relaxing Rock Ambient Guitar".into(),
      uploader: Some(uploader.into()),
      duration: Some("1:57:50".into()),
      upload_date: Some("20261003".into()),
      view_count: Some("1.2M".into()),
      tags: (0..tags).map(|i| format!("tag{i}")).collect(),
      categories: Vec::new(),
    });
    app.poll_spectrum();
    app
  }

  /// Renders just the footer row and returns its text.
  fn render_footer_row(app: &App, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 4)).expect("test backend");
    terminal.draw(|f| render_footer(f, app, Rect { x: 0, y: 0, width, height: 1 })).expect("draw");
    let buffer = terminal.backend().buffer();
    (0..width).map(|x| buffer[(x, 0)].symbol()).collect()
  }

  #[test]
  fn the_wiki_shortcut_appears_only_for_the_owning_channel() {
    let expected = crate::constants::constants().pastel_sketchbook_uploader.clone();

    let mut channel = playing_app_from(&expected, 0);
    channel.player.set_playing_for_test(true);
    assert!(channel.wiki_available());
    assert!(render_footer_row(&channel, 200).contains("Wiki"), "channel videos must offer the wiki");

    let mut other = playing_app_from("Some Other Channel", 0);
    other.player.set_playing_for_test(true);
    assert!(!other.wiki_available());
    let row = render_footer_row(&other, 200);
    assert!(!row.contains("Wiki"), "other channels must not offer the wiki: {row}");
  }

  #[test]
  fn the_wiki_shortcut_matches_the_uploader_case_insensitively() {
    let expected = crate::constants::constants().pastel_sketchbook_uploader.clone();
    let mut app = playing_app_from(&expected.to_lowercase(), 0);
    app.player.set_playing_for_test(true);
    assert!(app.wiki_available(), "channel matching must tolerate case");
  }

  #[test]
  fn an_unknown_uploader_still_offers_the_wiki() {
    // A missing metadata field is not evidence of another channel, and hiding
    // the shortcut would break the feature for anyone rate limited.
    let mut app = playing_app_from("Play&Enjoy", 0);
    app.player.current_details.as_mut().expect("details").uploader = None;
    app.player.set_playing_for_test(true);
    assert!(app.wiki_available());
    assert!(render_footer_row(&app, 200).contains("Wiki"));
  }

  #[test]
  fn no_wiki_without_a_loaded_video() {
    let mut app = App::new(DisplayMode::Ascii);
    assert!(!app.wiki_available(), "nothing is loaded");

    app.player.current_details = Some(VideoDetails {
      url: "https://youtube.com/watch?v=abc".into(),
      title: "t".into(),
      uploader: Some("Some Other Channel".into()),
      duration: None,
      upload_date: None,
      view_count: None,
      tags: Vec::new(),
      categories: Vec::new(),
    });
    assert!(!app.wiki_available(), "another channel's video has no wiki entry");
  }

  #[test]
  fn toggling_the_wiki_refuses_outside_the_channel() {
    // Only the refusal is exercised here: the accepting path fetches the
    // bundle over the network, which does not belong in a unit test.
    let mut app = playing_app_from("Some Other Channel", 0);
    app.wiki_toggle();
    assert!(!app.wiki_visible, "the pane must not open for another channel");
    assert!(app.last_error.is_some(), "the refusal must be explained");
  }

  #[test]
  fn hiding_the_wiki_stays_available() {
    // Once open, the hint must still offer a way back out.
    let expected = crate::constants::constants().pastel_sketchbook_uploader.clone();
    let mut app = playing_app_from(&expected, 0);
    app.player.set_playing_for_test(true);
    app.wiki_visible = true;
    assert!(render_footer_row(&app, 200).contains("Hide Wiki"));
  }

  #[test]
  fn the_spectrum_is_reserved_whenever_the_pane_is_tall_enough() {
    let area = Rect { x: 0, y: 0, width: 40, height: 18 };
    let (metadata, spectrum) = split_now_playing(area, true);
    let spectrum = spectrum.expect("spectrum reserved");
    assert_eq!(metadata.height + spectrum.height, area.height, "the split must consume the pane");
    assert!(spectrum.height >= MIN_SPECTRUM_HEIGHT, "got {}", spectrum.height);
    assert!(metadata.height >= MIN_METADATA_HEIGHT, "metadata got {}", metadata.height);
  }

  #[test]
  fn the_spectrum_never_takes_more_than_its_share() {
    // A very tall pane must not hand most of its rows to the display.
    let area = Rect { x: 0, y: 0, width: 40, height: 100 };
    let (_, spectrum) = split_now_playing(area, true);
    let spectrum = spectrum.expect("spectrum reserved");
    assert!(spectrum.height <= 45, "spectrum took {} of 100 rows", spectrum.height);
  }

  #[test]
  fn the_spectrum_yields_when_the_pane_is_too_short() {
    let area = Rect { x: 0, y: 0, width: 40, height: MIN_METADATA_HEIGHT + MIN_SPECTRUM_HEIGHT - 1 };
    let (metadata, spectrum) = split_now_playing(area, true);
    assert!(spectrum.is_none(), "must not split a pane this short");
    assert_eq!(metadata.height, area.height);
  }

  #[test]
  fn no_spectrum_is_reserved_when_none_is_requested() {
    let area = Rect { x: 0, y: 0, width: 40, height: 40 };
    let (metadata, spectrum) = split_now_playing(area, false);
    assert!(spectrum.is_none());
    assert_eq!(metadata.height, area.height);
  }

  #[test]
  fn the_spectrum_is_drawn_even_with_a_full_set_of_metadata() {
    // The regression: metadata with tags used to consume the entire pane,
    // leaving the spectrum zero rows at any realistic terminal size.
    for (width, height) in [(100, 24), (80, 24), (120, 40)] {
      let text = render_now_playing(&mut playing_app(7), width, height);
      assert!(text.contains("SPECTRUM"), "no spectrum at {width}x{height}:\n{text}");
    }
  }

  #[test]
  fn the_spectrum_is_drawn_without_tags() {
    let text = render_now_playing(&mut playing_app(0), 100, 24);
    assert!(text.contains("SPECTRUM"), "no spectrum:\n{text}");
  }

  #[test]
  fn the_spectrum_is_dropped_rather_than_crowding_out_the_metadata() {
    // One row shorter than both panes need: the text must survive.
    let text = render_now_playing(&mut playing_app(0), 80, 14);
    assert!(text.contains("Now Playing"), "metadata must remain:\n{text}");
    assert!(!text.contains("SPECTRUM"), "the spectrum must yield when space is short:\n{text}");
  }

  #[test]
  fn the_footer_advertises_the_spectrum_shortcut_while_playing() {
    // The shortcut is only discoverable if the footer names it in the mode the
    // user is actually in, which is Input when a channel track is playing.
    let mut app = playing_app(0);
    app.player.set_playing_for_test(true);
    app.player.paused = false;
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("test backend");
    terminal.draw(|f| render_footer(f, &app, Rect { x: 0, y: 0, width: 120, height: 1 })).expect("draw");
    let buffer = terminal.backend().buffer();
    let row: String = (0..120).map(|x| buffer[(x, 0)].symbol()).collect();
    assert!(row.contains("Spectrum"), "footer must offer the spectrum shortcut: {row}");
    assert!(row.contains("Seek"), "footer must offer seeking: {row}");
    assert!(row.contains("Space"), "footer must offer pause: {row}");
  }

  #[test]
  fn the_footer_omits_playback_keys_when_idle() {
    let mut app = playing_app(0);
    app.player.set_playing_for_test(false);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("test backend");
    terminal.draw(|f| render_footer(f, &app, Rect { x: 0, y: 0, width: 120, height: 1 })).expect("draw");
    let buffer = terminal.backend().buffer();
    let row: String = (0..120).map(|x| buffer[(x, 0)].symbol()).collect();
    assert!(!row.contains("Spectrum"), "an idle app has no spectrum to configure: {row}");
  }

  #[test]
  fn no_spectrum_without_a_loaded_track() {
    let mut app = App::new(DisplayMode::Ascii);
    app.poll_spectrum();
    assert!(!app.spectrum_visible(), "an idle app must not reserve spectrum rows");
    let text = render_now_playing(&mut app, 100, 24);
    assert!(!text.contains("SPECTRUM"), "an idle app must not draw a spectrum:\n{text}");
  }

  #[test]
  fn buffer_modes_halve_the_rows_because_cells_are_twice_as_tall() {
    let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1920, 1080, image::Rgb([0, 0, 0])));
    // A 16:9 image in a 32-column pane is 9 rows of cells: 32 * 9/32.
    assert_eq!(ideal_thumb_height(DisplayMode::Direct, &image, 32), 9);
    assert_eq!(ideal_thumb_height(DisplayMode::Ascii, &image, 32), 9);
  }

  #[test]
  fn a_non_wide_image_gets_a_taller_rect() {
    let square = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(500, 500, image::Rgb([0, 0, 0])));
    assert_eq!(ideal_thumb_height(DisplayMode::Direct, &square, 20), 10, "a square looks square on screen");
    let portrait = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(400, 800, image::Rgb([0, 0, 0])));
    assert_eq!(ideal_thumb_height(DisplayMode::Direct, &portrait, 20), 20, "a 1:2 image is twice as tall");
  }

  #[test]
  fn a_degenerate_image_never_panics() {
    let empty = DynamicImage::ImageRgb8(image::RgbImage::new(0, 0));
    let height = ideal_thumb_height(DisplayMode::Direct, &empty, 16);
    assert!(height >= 1, "must yield a usable height, got {height}");
  }

  #[test]
  fn the_protocol_modes_use_the_whole_pane() {
    // Kitty and Sixel let the terminal letterbox, so no cell arithmetic and
    // therefore no vertical jitter as the pane resizes.
    let mut app = playing_app(0);
    app.player.set_playing_for_test(true);
    app.player.display_mode = DisplayMode::Kitty;
    app.player.cached_thumbnail = Some((
      "abc".to_string(),
      DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1920, 1080, image::Rgb([0, 0, 0]))),
    ));
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test backend");
    let area = Rect { x: 0, y: 0, width: 120, height: 40 };
    terminal.draw(|f| render_player(f, &mut app, area)).expect("draw");
    let placed = app.gfx.thumb_area.expect("kitty records the pane");
    // The full inner pane, top-aligned at the inset: no aspect-derived height,
    // so the placement cannot drift as the pane or image changes shape.
    assert_eq!(placed.y, 1, "inset by the border");
    assert_eq!(placed.height, 38, "the whole pane height, not an aspect slice");
    assert_eq!(placed.height, area.height - 2);
  }

  fn normal() -> Style {
    Style::default().fg(Color::White)
  }

  fn matched() -> Style {
    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
  }

  #[test]
  fn highlight_text_empty_needle() {
    let spans = highlight_text("hello world", "", normal(), matched());
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "hello world");
  }

  #[test]
  fn highlight_text_no_match() {
    let spans = highlight_text("hello world", "xyz", normal(), matched());
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "hello world");
  }

  #[test]
  fn highlight_text_basic_match() {
    let spans = highlight_text("hello world", "world", normal(), matched());
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].content, "hello ");
    assert_eq!(spans[0].style, normal());
    assert_eq!(spans[1].content, "world");
    assert_eq!(spans[1].style, matched());
  }

  #[test]
  fn highlight_text_case_insensitive() {
    let spans = highlight_text("Hello World", "hello", normal(), matched());
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].content, "Hello");
    assert_eq!(spans[0].style, matched());
    assert_eq!(spans[1].content, " World");
    assert_eq!(spans[1].style, normal());
  }

  #[test]
  fn highlight_text_multiple_matches() {
    let spans = highlight_text("rock and rock music", "rock", normal(), matched());
    assert_eq!(spans.len(), 4);
    assert_eq!(spans[0].content, "rock");
    assert_eq!(spans[0].style, matched());
    assert_eq!(spans[1].content, " and ");
    assert_eq!(spans[1].style, normal());
    assert_eq!(spans[2].content, "rock");
    assert_eq!(spans[2].style, matched());
    assert_eq!(spans[3].content, " music");
    assert_eq!(spans[3].style, normal());
  }

  #[test]
  fn highlight_text_match_at_start() {
    let spans = highlight_text("abc def", "abc", normal(), matched());
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].content, "abc");
    assert_eq!(spans[0].style, matched());
    assert_eq!(spans[1].content, " def");
  }

  #[test]
  fn highlight_text_match_at_end() {
    let spans = highlight_text("abc def", "def", normal(), matched());
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].content, "abc ");
    assert_eq!(spans[1].content, "def");
    assert_eq!(spans[1].style, matched());
  }

  #[test]
  fn highlight_text_entire_string_match() {
    let spans = highlight_text("rock", "rock", normal(), matched());
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "rock");
    assert_eq!(spans[0].style, matched());
  }

  #[test]
  fn highlight_text_unicode() {
    let spans = highlight_text("café music", "café", normal(), matched());
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].content, "café");
    assert_eq!(spans[0].style, matched());
    assert_eq!(spans[1].content, " music");
  }

  #[test]
  fn highlight_text_needle_longer_than_text() {
    let spans = highlight_text("hi", "hello world", normal(), matched());
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "hi");
    assert_eq!(spans[0].style, normal());
  }
}
