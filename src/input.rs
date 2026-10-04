use anyhow::{Context, Result};
use ratatui::crossterm::{
  self as crossterm,
  event::{self, KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
};

use crate::app::{App, AppMode};
use crate::window;

// --- Helpers ---

/// Seconds moved by a single left/right press while playing.
const SEEK_STEP_SECS: f64 = 10.0;

/// Convert a char index to a byte offset within the string.
pub fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
  s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

// --- Event Handling ---

#[allow(clippy::too_many_lines)]
pub async fn handle_key_event(app: &mut App, key: event::KeyEvent) -> Result<()> {
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
    app.should_quit = true;
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
    app.next_theme();
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f') {
    app.next_frame_mode();
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('v') {
    app.cycle_spectrum_style();
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
    if app.player.is_playing() {
      app.player.stop().await.context("Failed to stop playback")?;
      app.cancel_transcription();
      app.utterances.clear();
      app.clear_frame_state();
      app.gfx.last_sent = None;
      app.gfx.resized_thumb = None;
      app.dragging = false;
    }
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
    if let Some(ref details) = app.player.current_details {
      let url = details.url.clone();
      // Use platform-appropriate command to open URL in default browser.
      #[cfg(target_os = "macos")]
      let cmd = "open";
      #[cfg(not(target_os = "macos"))]
      let cmd = "xdg-open";
      match std::process::Command::new(cmd)
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
      {
        Ok(mut child) => {
          // Reap the child in a background thread to avoid zombie processes.
          std::thread::spawn(move || {
            let _ = child.wait();
          });
        }
        Err(e) => {
          app.set_error(format!("Failed to open browser: {e}"));
        }
      }
    }
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('a') {
    app.transcript_toggle();
    return Ok(());
  }

  // Ctrl+W — toggle wiki pane. Only offered for the channel the wiki covers;
  // `wiki_toggle` reports the refusal if the shortcut is pressed anyway.
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('w') {
    if app.player.is_playing() {
      app.wiki_toggle();
    }
    return Ok(());
  }

  // Wiki controls when wiki pane is visible
  if app.wiki_visible {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('j') {
      app.wiki_scroll = app.wiki_scroll.saturating_add(1);
      return Ok(());
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('k') {
      app.wiki_scroll = app.wiki_scroll.saturating_sub(1);
      return Ok(());
    }
    // Tab — switch between Detail and Raw wiki views
    if key.code == KeyCode::Tab && key.modifiers.is_empty() {
      use crate::app::WikiTab;
      app.wiki_tab = match app.wiki_tab {
        WikiTab::Detail => WikiTab::Raw,
        WikiTab::Raw => WikiTab::Detail,
      };
      app.wiki_scroll = 0;
      // Trigger raw transcript fetch on first switch to Raw tab
      if app.wiki_tab == WikiTab::Raw && app.wiki_raw.is_none() {
        app.trigger_wiki_raw_fetch();
      }
      return Ok(());
    }
    // Number keys 1-5: load related video from wiki (Detail tab only)
    if app.wiki_tab == crate::app::WikiTab::Detail
      && key.modifiers.is_empty()
      && let KeyCode::Char(c @ '1'..='5') = key.code
    {
      let idx = (c as usize) - ('1' as usize);
      if let Some(ref detail) = app.wiki_detail
        && let Some(rel) = detail.related.get(idx)
      {
        let video_id = rel.id.clone();
        app.trigger_load_by_id(video_id);
      }
      return Ok(());
    }
  }

  // Ctrl+M — toggle PiP (picture-in-picture) mode (only on supported terminals)
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('m') && window::pip_supported() {
    app.toggle_pip().await;
    return Ok(());
  }

  // Playback keys are handled before mode dispatch, so a focused search box
  // cannot swallow Space or the arrows while a track is playing.
  if let Some(action) = playback_key(key, app.player.is_playing()) {
    apply_playback_key(app, action).await;
    return Ok(());
  }

  match app.mode {
    AppMode::Input => handle_input_key(app, key),
    AppMode::Results => handle_results_key(app, key).await.context("Failed to handle results key event")?,
    AppMode::Filter => handle_filter_key(app, key).context("Failed to handle filter key event")?,
  }
  Ok(())
}

/// What a key requests of the player while a track is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackKey {
  TogglePause,
  SeekForward,
  SeekBackward,
}

/// Decides whether a key acts on playback rather than on the search box.
///
/// Playback wins over text editing, otherwise a search that is still focused
/// swallows Space and the arrows, leaving a playing track with no keyboard
/// control. Typing a query still works: only these three keys are claimed.
#[must_use]
pub fn playback_key(key: event::KeyEvent, playing: bool) -> Option<PlaybackKey> {
  if !playing {
    return None;
  }
  match key.code {
    KeyCode::Char(' ') => Some(PlaybackKey::TogglePause),
    KeyCode::Right if key.modifiers.is_empty() => Some(PlaybackKey::SeekForward),
    KeyCode::Left if key.modifiers.is_empty() => Some(PlaybackKey::SeekBackward),
    _ => None,
  }
}

/// Runs a playback action, reporting failures through the status line.
pub async fn apply_playback_key(app: &mut App, action: PlaybackKey) {
  match action {
    PlaybackKey::TogglePause => {
      if let Err(e) = app.player.toggle_pause().await {
        app.set_error(format!("Pause error: {e}"));
      }
    }
    PlaybackKey::SeekForward => app.seek_relative(SEEK_STEP_SECS).await,
    PlaybackKey::SeekBackward => app.seek_relative(-SEEK_STEP_SECS).await,
  }
}

fn handle_input_key(app: &mut App, key: event::KeyEvent) {
  app.clear_error();
  match key.code {
    KeyCode::Enter => {
      app.trigger_search();
    }
    KeyCode::Char(c) => {
      let byte_idx = char_to_byte_index(&app.input, app.cursor_position);
      app.input.insert(byte_idx, c);
      app.cursor_position += 1;
    }
    KeyCode::Backspace if app.cursor_position > 0 => {
      app.cursor_position -= 1;
      let byte_idx = char_to_byte_index(&app.input, app.cursor_position);
      app.input.remove(byte_idx);
    }
    KeyCode::Delete if app.cursor_position < app.input.chars().count() => {
      let byte_idx = char_to_byte_index(&app.input, app.cursor_position);
      app.input.remove(byte_idx);
    }
    KeyCode::Left => {
      app.cursor_position = app.cursor_position.saturating_sub(1);
    }
    KeyCode::Right if app.cursor_position < app.input.chars().count() => {
      app.cursor_position += 1;
    }
    KeyCode::Home => {
      app.cursor_position = 0;
    }
    KeyCode::End => {
      app.cursor_position = app.input.chars().count();
    }
    KeyCode::Esc => {
      if !app.input.is_empty() {
        app.input.clear();
        app.cursor_position = 0;
        app.input_scroll = 0;
      } else if !app.search_results.is_empty() {
        app.mode = AppMode::Results;
      } else {
        app.should_quit = true;
      }
    }
    KeyCode::Down if !app.search_results.is_empty() => {
      app.mode = AppMode::Results;
    }
    _ => {}
  }
}

async fn handle_results_key(app: &mut App, key: event::KeyEvent) -> Result<()> {
  if let Some(action) = playback_key(key, app.player.is_playing()) {
    apply_playback_key(app, action).await;
    return Ok(());
  }
  match key.code {
    KeyCode::Enter => {
      app.trigger_load();
    }
    KeyCode::Char('/') => {
      app.mode = AppMode::Filter;
    }
    KeyCode::Down | KeyCode::Char('j') => {
      let count = app.filtered_indices.len();
      if count > 0 {
        let i = app.list_state.selected().map_or(0, |i| (i + 1) % count);
        app.list_state.select(Some(i));
        // Trigger background load when within 5 items of the bottom (use actual index)
        if let Some(&actual_idx) = app.filtered_indices.get(i)
          && actual_idx >= app.search_results.len().saturating_sub(5)
        {
          app.trigger_load_more();
        }
      }
    }
    KeyCode::Up | KeyCode::Char('k') => {
      let count = app.filtered_indices.len();
      if count > 0 {
        let i =
          app.list_state.selected().map_or(0, |i| if i == 0 { count.saturating_sub(1) } else { i.saturating_sub(1) });
        app.list_state.select(Some(i));
      }
    }
    KeyCode::Esc => {
      app.mode = AppMode::Input;
    }
    _ => {}
  }
  Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn handle_filter_key(app: &mut App, key: event::KeyEvent) -> Result<()> {
  match key.code {
    KeyCode::Char(c) => {
      let byte_idx = char_to_byte_index(&app.filter, app.filter_cursor);
      app.filter.insert(byte_idx, c);
      app.filter_cursor += 1;
      app.recompute_filter();
    }
    KeyCode::Backspace if app.filter_cursor > 0 => {
      app.filter_cursor -= 1;
      let byte_idx = char_to_byte_index(&app.filter, app.filter_cursor);
      app.filter.remove(byte_idx);
      app.recompute_filter();
    }
    KeyCode::Delete if app.filter_cursor < app.filter.chars().count() => {
      let byte_idx = char_to_byte_index(&app.filter, app.filter_cursor);
      app.filter.remove(byte_idx);
      app.recompute_filter();
    }
    KeyCode::Left => {
      app.filter_cursor = app.filter_cursor.saturating_sub(1);
    }
    KeyCode::Right if app.filter_cursor < app.filter.chars().count() => {
      app.filter_cursor += 1;
    }
    KeyCode::Home => {
      app.filter_cursor = 0;
    }
    KeyCode::End => {
      app.filter_cursor = app.filter.chars().count();
    }
    KeyCode::Down => {
      // Navigate filtered results while typing
      let count = app.filtered_indices.len();
      if count > 0 {
        let i = app.list_state.selected().map_or(0, |i| (i + 1) % count);
        app.list_state.select(Some(i));
        // Trigger pagination if near bottom of actual results
        if let Some(&actual_idx) = app.filtered_indices.get(i)
          && actual_idx >= app.search_results.len().saturating_sub(5)
        {
          app.trigger_load_more();
        }
      }
    }
    KeyCode::Up => {
      let count = app.filtered_indices.len();
      if count > 0 {
        let i =
          app.list_state.selected().map_or(0, |i| if i == 0 { count.saturating_sub(1) } else { i.saturating_sub(1) });
        app.list_state.select(Some(i));
      }
    }
    KeyCode::Enter => {
      // Apply filter and return to Results mode
      app.mode = AppMode::Results;
    }
    KeyCode::Esc => {
      // Clear filter and return to Results mode
      app.filter.clear();
      app.filter_cursor = 0;
      app.filter_scroll = 0;
      app.recompute_filter();
      app.mode = AppMode::Results;
    }
    _ => {}
  }
  Ok(())
}

/// Check if the mouse position is within the wiki/info pane.
fn mouse_in_info_pane(app: &App, col: u16, row: u16) -> bool {
  app.wiki_visible
    && app.info_pane_area.is_some_and(|a| {
      col >= a.x && col < a.x.saturating_add(a.width) && row >= a.y && row < a.y.saturating_add(a.height)
    })
}

/// Handle mouse events: scroll in results/wiki, drag to resize split.
pub fn handle_mouse_event(app: &mut App, m: MouseEvent) {
  match m.kind {
    MouseEventKind::Down(MouseButton::Left) if app.player.is_playing() && app.info_pane_area.is_some() => {
      app.dragging = true;
    }
    MouseEventKind::Drag(MouseButton::Left) if app.dragging => {
      let col = f64::from(m.column);
      let width = f64::from(crossterm::terminal::size().map_or(80, |(w, _)| w).max(1));
      app.split = (col / width).clamp(0.2, 0.8);
    }
    MouseEventKind::Up(MouseButton::Left) => {
      app.dragging = false;
    }
    MouseEventKind::ScrollDown => {
      if mouse_in_info_pane(app, m.column, m.row) {
        app.wiki_scroll = app.wiki_scroll.saturating_add(2);
        return;
      }
      if matches!(app.mode, AppMode::Results | AppMode::Filter) {
        let count = app.filtered_indices.len();
        if count > 0 {
          let i = app.list_state.selected().map_or(0, |i| (i + 1).min(count.saturating_sub(1)));
          app.list_state.select(Some(i));
          if let Some(&actual_idx) = app.filtered_indices.get(i)
            && actual_idx >= app.search_results.len().saturating_sub(5)
          {
            app.trigger_load_more();
          }
        }
      }
    }
    MouseEventKind::ScrollUp => {
      if mouse_in_info_pane(app, m.column, m.row) {
        app.wiki_scroll = app.wiki_scroll.saturating_sub(2);
        return;
      }
      if matches!(app.mode, AppMode::Results | AppMode::Filter) {
        let count = app.filtered_indices.len();
        if count > 0 {
          let i = app.list_state.selected().map_or(0, |i| i.saturating_sub(1));
          app.list_state.select(Some(i));
        }
      }
    }
    _ => {}
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // --- playback_key ---

  fn key(code: KeyCode) -> event::KeyEvent {
    event::KeyEvent::new(code, KeyModifiers::NONE)
  }

  #[test]
  fn playback_keys_work_while_a_track_plays() {
    // The regression: a focused search box used to swallow these, leaving a
    // playing track with no keyboard control.
    assert_eq!(playback_key(key(KeyCode::Char(' ')), true), Some(PlaybackKey::TogglePause));
    assert_eq!(playback_key(key(KeyCode::Right), true), Some(PlaybackKey::SeekForward));
    assert_eq!(playback_key(key(KeyCode::Left), true), Some(PlaybackKey::SeekBackward));
  }

  #[test]
  fn nothing_is_claimed_when_nothing_is_playing() {
    // Otherwise Space would be unusable as a space in a search query.
    assert_eq!(playback_key(key(KeyCode::Char(' ')), false), None);
    assert_eq!(playback_key(key(KeyCode::Right), false), None);
    assert_eq!(playback_key(key(KeyCode::Left), false), None);
  }

  #[test]
  fn typing_and_search_keys_are_never_claimed() {
    for code in
      [KeyCode::Char('a'), KeyCode::Enter, KeyCode::Backspace, KeyCode::Up, KeyCode::Down, KeyCode::Esc, KeyCode::Tab]
    {
      assert_eq!(playback_key(key(code), true), None, "{code:?} must stay with text entry");
    }
  }

  #[test]
  fn modified_arrows_are_left_to_text_entry() {
    // Ctrl+Left and friends are not seeks.
    let modified = event::KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL);
    assert_eq!(playback_key(modified, true), None);
  }

  // --- char_to_byte_index ---

  #[test]
  fn char_to_byte_ascii() {
    assert_eq!(char_to_byte_index("hello", 0), 0);
    assert_eq!(char_to_byte_index("hello", 3), 3);
    assert_eq!(char_to_byte_index("hello", 5), 5); // past end
  }

  #[test]
  fn char_to_byte_multibyte() {
    let s = "aé日"; // a=1 byte, é=2 bytes, 日=3 bytes
    assert_eq!(char_to_byte_index(s, 0), 0); // 'a'
    assert_eq!(char_to_byte_index(s, 1), 1); // 'é' starts at byte 1
    assert_eq!(char_to_byte_index(s, 2), 3); // '日' starts at byte 3
    assert_eq!(char_to_byte_index(s, 3), 6); // past end
  }

  #[test]
  fn char_to_byte_empty() {
    assert_eq!(char_to_byte_index("", 0), 0);
    assert_eq!(char_to_byte_index("", 5), 0);
  }
}
