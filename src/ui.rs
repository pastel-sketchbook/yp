use image::imageops::FilterType;
use ratatui::{
  layout::{Alignment, Constraint, Layout, Rect},
  style::{Modifier, Style, Stylize},
  text::{Line, Span},
  widgets::{Block, List, ListItem, Padding, Paragraph},
  Frame,
};

use crate::display::DisplayMode;
use crate::graphics::ThumbnailWidget;
use crate::theme::Theme;
use crate::{App, AppMode};

// --- Helpers ---

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
    format!("{}…", truncated)
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

// --- UI Rendering ---

pub fn ui(frame: &mut Frame, app: &mut App) {
  let theme = app.theme();
  app.gfx.thumb_area = None;

  frame.render_widget(Block::default().style(Style::default().bg(theme.bg)), frame.area());

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

fn render_header(frame: &mut Frame, theme: &Theme, area: Rect) {
  let left = Line::from(Span::styled(" ▶ yp ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
  frame.render_widget(left, area);

  let version = format!("v{} ", env!("CARGO_PKG_VERSION"));
  let version_w = (version.len().min(u16::MAX as usize)) as u16;
  let right = Line::from(Span::styled(&version, Style::default().fg(theme.muted)));
  let right_area = Rect { x: area.x + area.width.saturating_sub(version_w), width: version_w, ..area };
  frame.render_widget(right, right_area);
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
    Line::from(Span::styled("Search YouTube. Play audio/video. In the terminal.", Style::default().fg(theme.fg))),
    Line::from(""),
    Line::from(Span::styled("Type a query below and press Enter.", Style::default().fg(theme.muted))),
  ];
  let paragraph = Paragraph::new(text).alignment(Alignment::Center).block(
    Block::bordered()
      .border_type(ratatui::widgets::BorderType::Rounded)
      .border_style(Style::default().fg(theme.border)),
  );
  frame.render_widget(paragraph, area);
}

fn render_player(frame: &mut Frame, app: &mut App, area: Rect) {
  let theme = app.theme();
  let [mut thumb_area, info_area] =
    Layout::horizontal([Constraint::Percentage(68), Constraint::Percentage(32)]).areas(area);

  // Pad and center vertically to maintain 16:9 if possible
  thumb_area = Rect { y: thumb_area.y + 1, height: thumb_area.height.saturating_sub(2), ..thumb_area };
  let ideal_h = (thumb_area.width as f32 * 9.0 / 32.0).round() as u16;
  if ideal_h < thumb_area.height {
    let diff = thumb_area.height - ideal_h;
    thumb_area.y += diff / 2;
    thumb_area.height = ideal_h;
  }

  if let Some((ref video_id, ref image)) = app.player.cached_thumbnail {
    if matches!(app.player.display_mode, DisplayMode::Kitty | DisplayMode::Sixel) {
      // Kitty/Sixel: rendering is handled outside ratatui (in run loop).
      // Just record the area — skip the expensive resize and widget render
      // that are only used by Direct/Ascii modes.
      let _ = (video_id, image); // suppress unused warnings
      app.gfx.thumb_area = Some(thumb_area);
    } else {
      // Direct/Ascii: resize and render via ThumbnailWidget into the ratatui buffer.
      let needs_resize = match &app.gfx.resized_thumb {
        Some((id, w, h, _)) => id != video_id || *w != thumb_area.width || *h != thumb_area.height,
        None => true,
      };
      if needs_resize {
        let target_w = thumb_area.width as u32;
        let target_h = match app.player.display_mode {
          DisplayMode::Direct => (target_w as f32 * 9.0 / 16.0) as u32,
          _ => (target_w as f32 * 9.0 / 32.0) as u32,
        };
        let resized = image.resize_to_fill(target_w, target_h.max(1), FilterType::Lanczos3);
        app.gfx.resized_thumb = Some((video_id.clone(), thumb_area.width, thumb_area.height, resized));
      }

      if let Some((_, _, _, ref resized)) = app.gfx.resized_thumb {
        let widget = ThumbnailWidget { image: resized, display_mode: app.player.display_mode };
        frame.render_widget(widget, thumb_area);
      }
    }
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
    let inner_w = info_area.width.saturating_sub(4) as usize;

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
      for tag in &details.tags {
        lines.push(Line::from(Span::styled(
          format!("  {}", truncate_str(tag, inner_w.saturating_sub(2))),
          Style::default().fg(theme.tag),
        )));
      }
    }

    let paragraph = Paragraph::new(lines).block(info_block);
    frame.render_widget(paragraph, info_area);
  } else {
    frame.render_widget(info_block, info_area);
  }
}

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
      let tags_str = entry.tags.as_deref().unwrap_or("");
      let right = match (!tags_str.is_empty(), !date_str.is_empty()) {
        (true, true) => format!("{}  {}", tags_str, date_str),
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
        let title_max = inner_w.saturating_sub(right_w + 2);
        let title = truncate_str(&entry.title, title_max);
        let title_w = title.chars().count();
        let gap = inner_w.saturating_sub(title_w + right_w);

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
    format!(" Channel — {} videos{} ", count, suffix)
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
    (format!(" ⏳ {}", msg), Style::default().fg(theme.status))
  } else if let Some(err) = &app.last_error {
    (format!(" ⚠  {}", err), Style::default().fg(theme.error))
  } else {
    let mpv_status = app.player.get_last_mpv_status();
    match mpv_status {
      Some(status) => (format!(" ♪ {}", status), Style::default().fg(theme.status)),
      None => (" Ready".to_string(), Style::default().fg(theme.muted)),
    }
  };
  frame.render_widget(Paragraph::new(text).style(style), area);
}

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
    *scroll = cursor_col.saturating_sub(inner_w) + 1;
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
    frame.set_cursor_position((cursor_x, area.y + 1));
  }
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
  let theme = app.theme();
  let has_results = !app.search_results.is_empty();
  let is_playing = app.player.is_playing();
  let keys: Vec<(&str, &str)> = match app.mode {
    AppMode::Input => {
      let mut k = vec![("Enter", "Search"), ("^t", "Theme"), ("^f", "Frame")];
      if is_playing {
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
        let pause_label = if app.player.paused { "Resume" } else { "Pause" };
        k.push(("Space", pause_label));
        k.push(("^s", "Stop"));
        k.push(("^o", "Open"));
      }
      k.push(("^t", "Theme"));
      k.push(("^f", "Frame"));
      k.push(("Esc", "Back"));
      k
    }
    AppMode::Filter => {
      let mut k = vec![("Enter", "Apply"), ("Esc", "Clear"), ("↑↓", "Navigate")];
      if is_playing {
        let pause_label = if app.player.paused { "Resume" } else { "Pause" };
        k.push(("Space", pause_label));
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
        Span::styled(format!(" {} ", key), Style::default().fg(theme.key_fg).bg(theme.key_bg)),
        Span::styled(format!(" {} ", action), Style::default().fg(theme.muted)),
      ];
      if i < keys.len().saturating_sub(1) {
        s.push(Span::raw("  "));
      }
      s
    })
    .collect();

  frame.render_widget(Line::from(spans), area);

  let right_label = format!("{} | {} ", app.frame_mode.label(), theme.name);
  let right_w = (right_label.len().min(u16::MAX as usize)) as u16;
  let right = Line::from(Span::styled(&right_label, Style::default().fg(theme.muted)));
  let right_area = Rect { x: area.x + area.width.saturating_sub(right_w), width: right_w, ..area };
  frame.render_widget(right, right_area);
}

#[cfg(test)]
mod tests {
  use super::*;
  use ratatui::style::Color;

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
