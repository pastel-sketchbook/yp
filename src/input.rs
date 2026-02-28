use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, KeyCode, KeyModifiers};

use crate::app::{App, AppMode};
use crate::window;

// --- Helpers ---

/// Convert a char index to a byte offset within the string.
pub fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
  s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

// --- Event Handling ---

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

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
    if app.player.is_playing() {
      app.player.stop().await.context("Failed to stop playback")?;
      app.cancel_transcription();
      app.utterances.clear();
      app.clear_frame_state();
      app.gfx.last_sent = None;
      app.gfx.resized_thumb = None;
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
          app.set_error(format!("Failed to open browser: {}", e));
        }
      }
    }
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('a') {
    app.transcript_toggle();
    return Ok(());
  }

  // Ctrl+M — toggle PiP (picture-in-picture) mode (only on supported terminals)
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('m') && window::pip_supported() {
    app.toggle_pip().await;
    return Ok(());
  }

  match app.mode {
    AppMode::Input => handle_input_key(app, key),
    AppMode::Results => handle_results_key(app, key).await.context("Failed to handle results key event")?,
    AppMode::Filter => handle_filter_key(app, key).await.context("Failed to handle filter key event")?,
  }
  Ok(())
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
    KeyCode::Backspace => {
      if app.cursor_position > 0 {
        app.cursor_position -= 1;
        let byte_idx = char_to_byte_index(&app.input, app.cursor_position);
        app.input.remove(byte_idx);
      }
    }
    KeyCode::Delete => {
      if app.cursor_position < app.input.chars().count() {
        let byte_idx = char_to_byte_index(&app.input, app.cursor_position);
        app.input.remove(byte_idx);
      }
    }
    KeyCode::Left => {
      app.cursor_position = app.cursor_position.saturating_sub(1);
    }
    KeyCode::Right => {
      if app.cursor_position < app.input.chars().count() {
        app.cursor_position += 1;
      }
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
    KeyCode::Down => {
      if !app.search_results.is_empty() {
        app.mode = AppMode::Results;
      }
    }
    _ => {}
  }
}

async fn handle_results_key(app: &mut App, key: event::KeyEvent) -> Result<()> {
  match key.code {
    KeyCode::Enter => {
      app.trigger_load();
    }
    KeyCode::Char(' ') => {
      if app.player.is_playing()
        && let Err(e) = app.player.toggle_pause().await
      {
        app.set_error(format!("Pause error: {}", e));
      }
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

async fn handle_filter_key(app: &mut App, key: event::KeyEvent) -> Result<()> {
  match key.code {
    KeyCode::Char(c) => {
      let byte_idx = char_to_byte_index(&app.filter, app.filter_cursor);
      app.filter.insert(byte_idx, c);
      app.filter_cursor += 1;
      app.recompute_filter();
    }
    KeyCode::Backspace => {
      if app.filter_cursor > 0 {
        app.filter_cursor -= 1;
        let byte_idx = char_to_byte_index(&app.filter, app.filter_cursor);
        app.filter.remove(byte_idx);
        app.recompute_filter();
      }
    }
    KeyCode::Delete => {
      if app.filter_cursor < app.filter.chars().count() {
        let byte_idx = char_to_byte_index(&app.filter, app.filter_cursor);
        app.filter.remove(byte_idx);
        app.recompute_filter();
      }
    }
    KeyCode::Left => {
      app.filter_cursor = app.filter_cursor.saturating_sub(1);
    }
    KeyCode::Right => {
      if app.filter_cursor < app.filter.chars().count() {
        app.filter_cursor += 1;
      }
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

#[cfg(test)]
mod tests {
  use super::*;

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
