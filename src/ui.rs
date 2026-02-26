use image::imageops::FilterType;
use ratatui::{
  Frame,
  layout::{Alignment, Constraint, Layout, Rect},
  style::{Modifier, Style, Stylize},
  text::{Line, Span},
  widgets::{Block, List, ListItem, Padding, Paragraph},
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

// --- UI Rendering ---

pub fn ui(frame: &mut Frame, app: &mut App) {
  let theme = app.theme();
  app.graphics_thumb_area = None;

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
  let right = Line::from(Span::styled(&version, Style::default().fg(theme.muted)));
  let right_area =
    Rect { x: area.x + area.width.saturating_sub(version.len() as u16), width: version.len() as u16, ..area };
  frame.render_widget(right, right_area);
}

fn render_main(frame: &mut Frame, app: &mut App, area: Rect) {
  if app.mode == AppMode::Results && !app.search_results.is_empty() {
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
    Line::from(Span::styled("Search YouTube. Play audio. In the terminal.", Style::default().fg(theme.fg))),
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
    let needs_resize = match &app.cached_resized_thumb {
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
      app.cached_resized_thumb = Some((video_id.clone(), thumb_area.width, thumb_area.height, resized));
    }

    if let Some((_, _, _, ref resized)) = app.cached_resized_thumb {
      let widget = ThumbnailWidget { image: resized, display_mode: app.player.display_mode };
      frame.render_widget(widget, thumb_area);
    }

    if matches!(app.player.display_mode, DisplayMode::Kitty | DisplayMode::Sixel) {
      app.graphics_thumb_area = Some(thumb_area);
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
    .padding(Padding::horizontal(1));

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
    lines.push(Line::from(""));
    let url_display = truncate_str(&details.url, inner_w);
    lines.push(Line::from(Span::styled(
      url_display,
      Style::default().fg(theme.accent).add_modifier(Modifier::UNDERLINED),
    )));

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

  // Inner width: area minus 2 borders minus 2 chars for highlight symbol ("▶ ")
  let inner_w = area.width.saturating_sub(4) as usize;

  let items: Vec<ListItem> = app
    .search_results
    .iter()
    .enumerate()
    .map(|(i, entry)| {
      let is_selected = Some(i) == app.list_state.selected();
      let fg = if is_selected { theme.highlight_fg } else { theme.fg };
      let bg = if is_selected {
        theme.highlight_bg
      } else if i % 2 == 1 {
        theme.stripe_bg
      } else {
        theme.bg
      };

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
        Line::from(Span::styled(title, Style::default().fg(fg)))
      } else {
        // Reserve space for right side + 2-char gap
        let right_w = right.chars().count();
        let title_max = inner_w.saturating_sub(right_w + 2);
        let title = truncate_str(&entry.title, title_max);
        let title_w = title.chars().count();
        let gap = inner_w.saturating_sub(title_w + right_w);

        // Split right into tags and date parts for separate styling
        let padding: String = " ".repeat(gap);
        let mut spans = vec![Span::styled(title, Style::default().fg(fg)), Span::raw(padding)];
        if !tags_str.is_empty() && !date_str.is_empty() {
          spans.push(Span::styled(tags_str.to_string(), Style::default().fg(theme.muted)));
          spans.push(Span::raw("  "));
          spans.push(Span::styled(date_str.to_string(), Style::default().fg(theme.muted)));
        } else if !tags_str.is_empty() {
          spans.push(Span::styled(tags_str.to_string(), Style::default().fg(theme.muted)));
        } else {
          spans.push(Span::styled(date_str.to_string(), Style::default().fg(theme.muted)));
        }
        Line::from(spans)
      };

      ListItem::new(line).bg(bg)
    })
    .collect();

  let title = if is_channel {
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
        .border_style(Style::default().fg(theme.border)),
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
  let border_color = if app.mode == AppMode::Input { theme.accent } else { theme.border };
  let input_block = Block::bordered()
    .title(" Search YouTube ")
    .title_style(Style::default().fg(border_color))
    .border_type(ratatui::widgets::BorderType::Rounded)
    .border_style(Style::default().fg(border_color))
    .padding(Padding::horizontal(1));

  let inner_w = area.width.saturating_sub(4) as usize;
  let cursor_col = display_width(&app.input, app.cursor_position);

  if cursor_col < app.input_scroll {
    app.input_scroll = cursor_col;
  } else if cursor_col >= app.input_scroll + inner_w {
    app.input_scroll = cursor_col.saturating_sub(inner_w) + 1;
  }

  let visible: String = app
    .input
    .chars()
    .scan(0usize, |col, c| {
      let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
      let start = *col;
      *col += w;
      Some((start, *col, c))
    })
    .skip_while(|(_, end, _)| *end <= app.input_scroll)
    .take_while(|(start, _, _)| *start < app.input_scroll + inner_w)
    .map(|(_, _, c)| c)
    .collect();

  let paragraph = Paragraph::new(visible).style(Style::default().fg(theme.fg)).block(input_block);
  frame.render_widget(paragraph, area);

  if app.mode == AppMode::Input {
    let cursor_x = area.x + 2 + (cursor_col - app.input_scroll) as u16;
    frame.set_cursor_position((cursor_x, area.y + 1));
  }
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
  let theme = app.theme();
  let has_results = !app.search_results.is_empty();
  let is_playing = app.player.is_playing();
  let keys: Vec<(&str, &str)> = match app.mode {
    AppMode::Input => {
      let mut k = vec![("Enter", "Search"), ("^t", "Theme")];
      if is_playing {
        k.push(("^s", "Stop"));
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
      let mut k = vec![("Enter", "Play"), ("j/k", "Navigate")];
      if is_playing {
        let pause_label = if app.player.paused { "Resume" } else { "Pause" };
        k.push(("Space", pause_label));
        k.push(("^s", "Stop"));
      }
      k.push(("^t", "Theme"));
      k.push(("Esc", "Back"));
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
      if i < keys.len() - 1 {
        s.push(Span::raw("  "));
      }
      s
    })
    .collect();

  frame.render_widget(Line::from(spans), area);

  let theme_label = format!("{} ", theme.name);
  let right = Line::from(Span::styled(&theme_label, Style::default().fg(theme.muted)));
  let right_area =
    Rect { x: area.x + area.width.saturating_sub(theme_label.len() as u16), width: theme_label.len() as u16, ..area };
  frame.render_widget(right, right_area);
}
