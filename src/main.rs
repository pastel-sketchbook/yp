mod display;
mod graphics;
mod player;
mod theme;
mod ui;
mod youtube;

use anyhow::{Context, Result};
use clap::Parser;
use image::DynamicImage;
use ratatui::{
  DefaultTerminal,
  crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
  layout::Rect,
  widgets::ListState,
};
use std::time::Duration;
use tokio::sync::oneshot;

use display::{CliDisplayMode, DisplayMode};
use graphics::{kitty_delete_all, kitty_render_image, sixel_render_image};
use player::{MusicPlayer, VideoDetails};
use theme::THEMES;
use youtube::{fetch_thumbnail, get_video_info, search_youtube};

// --- CLI ---

#[derive(Parser, Debug)]
#[command(author, version = env!("CARGO_PKG_VERSION"), about, long_about = None)]
struct Args {
  /// Display mode: 'auto', 'kitty', 'sixel', 'direct', or 'ascii' (default: auto-detect)
  #[arg(short, long, default_value = "auto")]
  display_mode: CliDisplayMode,
}

// --- Types ---

type SearchResult = Vec<(String, String)>;
type LoadResult = (String, VideoDetails, Option<DynamicImage>);

// --- App State ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
  Input,
  Results,
}

pub struct App {
  pub input: String,
  pub cursor_position: usize,
  pub mode: AppMode,
  pub theme_index: usize,
  pub search_results: Vec<(String, String)>,
  pub list_state: ListState,
  pub player: MusicPlayer,
  pub last_error: Option<String>,
  pub status_message: Option<String>,
  pub should_quit: bool,
  search_rx: Option<oneshot::Receiver<Result<SearchResult>>>,
  load_rx: Option<oneshot::Receiver<Result<LoadResult>>>,
  pub graphics_thumb_area: Option<Rect>,
  pub graphics_last_sent: Option<(String, Rect)>,
  pub cached_resized_thumb: Option<(String, u16, u16, DynamicImage)>,
  pub input_scroll: usize,
}

impl App {
  fn new(display_mode: DisplayMode) -> Self {
    Self {
      input: String::new(),
      cursor_position: 0,
      mode: AppMode::Input,
      theme_index: 0,
      search_results: Vec::new(),
      list_state: ListState::default(),
      player: MusicPlayer::new(display_mode),
      last_error: None,
      status_message: None,
      should_quit: false,
      search_rx: None,
      load_rx: None,
      graphics_thumb_area: None,
      graphics_last_sent: None,
      cached_resized_thumb: None,
      input_scroll: 0,
    }
  }

  pub fn theme(&self) -> &'static theme::Theme {
    &THEMES[self.theme_index]
  }

  fn next_theme(&mut self) {
    self.theme_index = (self.theme_index + 1) % THEMES.len();
  }

  async fn check_pending(&mut self) -> Result<()> {
    if let Some(mut rx) = self.search_rx.take() {
      match rx.try_recv() {
        Ok(result) => {
          self.status_message = None;
          match result {
            Ok(results) if results.is_empty() => {
              self.last_error = Some("No results found.".to_string());
            }
            Ok(results) => {
              self.search_results = results;
              self.list_state.select(Some(0));
              self.mode = AppMode::Results;
            }
            Err(e) => {
              self.last_error = Some(format!("Search failed: {:#}", e));
            }
          }
        }
        Err(oneshot::error::TryRecvError::Empty) => {
          self.search_rx = Some(rx);
        }
        Err(oneshot::error::TryRecvError::Closed) => {
          self.status_message = None;
          self.last_error = Some("Search task failed.".to_string());
        }
      }
    }

    if let Some(mut rx) = self.load_rx.take() {
      match rx.try_recv() {
        Ok(result) => {
          self.status_message = None;
          match result {
            Ok((video_id, details, thumbnail)) => {
              if let Err(e) = self.player.play(details).await {
                self.last_error = Some(format!("Playback error: {}", e));
                let _ = self.player.stop().await;
              } else if let Some(thumb) = thumbnail {
                self.player.cached_thumbnail = Some((video_id, thumb));
              }
              self.mode = AppMode::Input;
            }
            Err(e) => {
              self.last_error = Some(format!("Failed to load: {:#}", e));
            }
          }
        }
        Err(oneshot::error::TryRecvError::Empty) => {
          self.load_rx = Some(rx);
        }
        Err(oneshot::error::TryRecvError::Closed) => {
          self.status_message = None;
          self.last_error = Some("Load task failed.".to_string());
        }
      }
    }

    Ok(())
  }

  fn trigger_search(&mut self) {
    let query = self.input.trim().to_string();
    if query.is_empty() {
      self.last_error = Some("Enter a search term.".to_string());
      return;
    }
    self.search_rx = None;
    self.last_error = None;
    self.status_message = Some(format!("Searching '{}'…", query));

    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
      let _ = tx.send(search_youtube(&query).await);
    });
    self.search_rx = Some(rx);
  }

  fn trigger_load(&mut self) {
    let Some(selected) = self.list_state.selected() else { return };
    let Some((_, video_id)) = self.search_results.get(selected) else { return };

    let video_id = video_id.clone();
    let client = self.player.http_client.clone();
    self.last_error = None;
    self.status_message = Some("Loading…".to_string());

    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
      let details = get_video_info(&video_id).await;
      match details {
        Ok(d) => {
          let thumb = fetch_thumbnail(&client, &video_id).await.ok();
          let _ = tx.send(Ok((video_id, d, thumb)));
        }
        Err(e) => {
          let _ = tx.send(Err(e));
        }
      }
    });
    self.load_rx = Some(rx);
  }
}

// --- Helpers ---

/// Convert a char index to a byte offset within the string.
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
  s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

// --- Event Handling ---

async fn handle_key_event(app: &mut App, key: event::KeyEvent) -> Result<()> {
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
    app.should_quit = true;
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
    app.next_theme();
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
    if app.player.is_playing() {
      app.player.stop().await.context("Failed to stop playback")?;
      app.graphics_last_sent = None;
      app.cached_resized_thumb = None;
    }
    return Ok(());
  }

  match app.mode {
    AppMode::Input => handle_input_key(app, key),
    AppMode::Results => handle_results_key(app, key).await?,
  }
  Ok(())
}

fn handle_input_key(app: &mut App, key: event::KeyEvent) {
  app.last_error = None;
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
        app.last_error = Some(format!("Pause error: {}", e));
      }
    }
    KeyCode::Down | KeyCode::Char('j') => {
      let count = app.search_results.len();
      if count > 0 {
        let i = app.list_state.selected().map_or(0, |i| (i + 1) % count);
        app.list_state.select(Some(i));
      }
    }
    KeyCode::Up | KeyCode::Char('k') => {
      let count = app.search_results.len();
      if count > 0 {
        let i = app.list_state.selected().map_or(0, |i| if i == 0 { count - 1 } else { i - 1 });
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

// --- Main ---

#[tokio::main]
async fn main() -> Result<()> {
  let args = Args::parse();

  let default_hook = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    ratatui::restore();
    default_hook(info);
  }));

  let mut terminal = ratatui::init();
  let result = run(&mut terminal, args).await;
  ratatui::restore();
  result
}

async fn run(terminal: &mut DefaultTerminal, args: Args) -> Result<()> {
  let display_mode = display::resolve_display_mode(args.display_mode);
  let mut app = App::new(display_mode);
  let uses_graphics_protocol = matches!(display_mode, DisplayMode::Kitty | DisplayMode::Sixel);

  loop {
    app.check_pending().await?;
    app.player.check_mpv_status();

    terminal.draw(|frame| ui::ui(frame, &mut app))?;

    if uses_graphics_protocol {
      if let Some(area) = app.graphics_thumb_area {
        if let Some((ref video_id, ref image)) = app.player.cached_thumbnail {
          let key = (video_id.clone(), area);
          if app.graphics_last_sent.as_ref() != Some(&key) {
            if display_mode == DisplayMode::Kitty {
              kitty_delete_all()?;
            }
            match display_mode {
              DisplayMode::Kitty => kitty_render_image(image, area)?,
              DisplayMode::Sixel => sixel_render_image(image, area)?,
              _ => {}
            }
            app.graphics_last_sent = Some(key);
          }
        }
      } else if app.graphics_last_sent.is_some() {
        if display_mode == DisplayMode::Kitty {
          kitty_delete_all()?;
        }
        app.graphics_last_sent = None;
      }
    }

    if event::poll(Duration::from_millis(100))? {
      match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
          handle_key_event(&mut app, key).await?;
        }
        _ => {}
      }
    }

    if app.should_quit {
      break;
    }
  }

  if display_mode == DisplayMode::Kitty {
    kitty_delete_all()?;
  }
  app.player.stop().await?;
  Ok(())
}
