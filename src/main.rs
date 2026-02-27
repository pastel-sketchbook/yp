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
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use display::{CliDisplayMode, DisplayMode};
use graphics::{kitty_delete_all, kitty_delete_placement, kitty_render_image, sixel_render_image};
use player::{MusicPlayer, VideoDetails};
use theme::THEMES;
use youtube::{
  CHANNEL_INITIAL_SIZE, CHANNEL_PAGE_SIZE, FrameSource, SearchEntry, VideoMeta, detect_channel_url,
  enrich_video_metadata, fetch_sprite_frames, fetch_thumbnail, fetch_video_frames, get_video_info, list_channel_videos,
  search_youtube,
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

// --- CLI ---

#[derive(Parser, Debug)]
#[command(author, version = env!("CARGO_PKG_VERSION"), about, long_about = None)]
struct Args {
  /// Display mode: 'auto', 'kitty', 'sixel', 'direct', or 'ascii' (default: auto-detect)
  #[arg(short, long, default_value = "auto")]
  display_mode: CliDisplayMode,
}

// --- Types ---

type SearchResult = Vec<SearchEntry>;
type LoadResult = (String, VideoDetails, Option<DynamicImage>);

/// Video frame display mode during playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMode {
  /// Static thumbnail only (no extra work).
  Thumbnail,
  /// YouTube storyboard sprite sheets (low-res 320x180, fast, no ffmpeg).
  Storyboard,
  /// ffmpeg frame extraction (640x360, progressive, requires ffmpeg).
  Video,
}

impl FrameMode {
  pub const ALL: [FrameMode; 3] = [FrameMode::Thumbnail, FrameMode::Storyboard, FrameMode::Video];

  pub fn label(self) -> &'static str {
    match self {
      FrameMode::Thumbnail => "thumbnail",
      FrameMode::Storyboard => "storyboard",
      FrameMode::Video => "video",
    }
  }

  pub fn from_config(s: &str) -> Self {
    match s.to_lowercase().as_str() {
      "storyboard" => FrameMode::Storyboard,
      "video" => FrameMode::Video,
      _ => FrameMode::Thumbnail,
    }
  }
}

/// Tracks the state of a channel listing for on-demand pagination.
#[derive(Debug, Clone)]
pub struct ChannelSource {
  /// The canonical channel URL used with yt-dlp.
  pub url: String,
  /// How many videos have been fetched so far.
  pub total_fetched: usize,
  /// Whether there might be more videos to load.
  pub has_more: bool,
  /// Whether a background "load more" request is in flight.
  pub loading_more: bool,
}

// --- Config ---

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Config {
  pub theme_name: Option<String>,
  pub frame_mode: Option<String>,
}

impl Config {
  pub fn load() -> Self {
    if let Some(proj_dirs) = ProjectDirs::from("", "", "yp") {
      let config_file = proj_dirs.config_dir().join("prefs.toml");
      if let Ok(content) = std::fs::read_to_string(config_file)
        && let Ok(config) = toml::from_str(&content)
      {
        return config;
      }
    }
    Self::default()
  }

  pub fn save(&self) {
    if let Some(proj_dirs) = ProjectDirs::from("", "", "yp") {
      let config_dir = proj_dirs.config_dir();
      if std::fs::create_dir_all(config_dir).is_ok() {
        let config_file = config_dir.join("prefs.toml");
        if let Ok(content) = toml::to_string(self) {
          let _ = std::fs::write(config_file, content);
        }
      }
    }
  }
}

// --- App State ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
  Input,
  Results,
}

/// Terminal graphics protocol rendering state (Kitty/Sixel).
#[derive(Default)]
pub struct GraphicsCache {
  pub thumb_area: Option<Rect>,
  pub last_sent: Option<(String, Rect)>,
  pub resized_thumb: Option<(String, u16, u16, DynamicImage)>,
}

/// Frame source state for storyboard/video frame display during playback.
#[derive(Default)]
struct FrameState {
  source: Option<FrameSource>,
  source_rx: Option<oneshot::Receiver<Result<FrameSource>>>,
  idx: Option<usize>,
  /// The original thumbnail image (before storyboard/video frames replace it).
  /// Used to restore the thumbnail when cycling frame modes.
  original_thumbnail: Option<(String, DynamicImage)>,
}

/// In-flight async task receivers and handles.
#[derive(Default)]
struct AsyncTasks {
  search_rx: Option<oneshot::Receiver<Result<SearchResult>>>,
  load_rx: Option<oneshot::Receiver<Result<LoadResult>>>,
  more_rx: Option<oneshot::Receiver<Result<SearchResult>>>,
  enrich_rx: Option<mpsc::Receiver<VideoMeta>>,
  enrich_handle: Option<JoinHandle<()>>,
}

pub struct App {
  pub input: String,
  pub cursor_position: usize,
  pub mode: AppMode,
  pub theme_index: usize,
  pub frame_mode: FrameMode,
  pub search_results: Vec<SearchEntry>,
  pub list_state: ListState,
  pub player: MusicPlayer,
  pub last_error: Option<String>,
  pub status_message: Option<String>,
  pub should_quit: bool,
  pub channel_source: Option<ChannelSource>,
  pub input_scroll: usize,
  pub gfx: GraphicsCache,
  frames: FrameState,
  tasks: AsyncTasks,
}

impl App {
  fn new(display_mode: DisplayMode) -> Self {
    let config = Config::load();
    let theme_index =
      if let Some(ref name) = config.theme_name { THEMES.iter().position(|t| t.name == name).unwrap_or(0) } else { 0 };
    let frame_mode =
      if let Some(ref mode) = config.frame_mode { FrameMode::from_config(mode) } else { FrameMode::Thumbnail };

    let default_input = "@ChrisH-v4e".to_string();
    let default_cursor = default_input.chars().count();

    Self {
      input: default_input,
      cursor_position: default_cursor,
      mode: AppMode::Input,
      theme_index,
      frame_mode,
      search_results: Vec::new(),
      list_state: ListState::default(),
      player: MusicPlayer::new(display_mode),
      last_error: None,
      status_message: None,
      should_quit: false,
      channel_source: None,
      input_scroll: 0,
      gfx: GraphicsCache::default(),
      frames: FrameState::default(),
      tasks: AsyncTasks::default(),
    }
  }

  pub fn theme(&self) -> &'static theme::Theme {
    &THEMES[self.theme_index]
  }

  fn save_config(&self) {
    let config =
      Config { theme_name: Some(self.theme().name.to_string()), frame_mode: Some(self.frame_mode.label().to_string()) };
    config.save();
  }

  fn next_theme(&mut self) {
    self.theme_index = (self.theme_index + 1) % THEMES.len();
    self.save_config();
  }

  fn next_frame_mode(&mut self) {
    let idx = FrameMode::ALL.iter().position(|m| *m == self.frame_mode).unwrap_or(0);
    self.frame_mode = FrameMode::ALL[(idx + 1) % FrameMode::ALL.len()];

    // Clear current frame source state
    self.frames.source = None;
    self.frames.source_rx = None;
    self.frames.idx = None;

    // Restore original thumbnail if we have one
    if let Some((ref vid, ref img)) = self.frames.original_thumbnail {
      self.player.cached_thumbnail = Some((vid.clone(), img.clone()));
      self.gfx.resized_thumb = None;
      self.gfx.last_sent = None;
    }

    // Trigger new frame source if currently playing
    if self.player.is_playing() {
      self.trigger_frame_source();
    }

    self.save_config();
  }

  /// Spawn appropriate frame source fetch based on current `frame_mode`.
  /// Does nothing in Thumbnail mode.
  /// If `explicit_video_id` is provided, uses that instead of looking up from cached state.
  fn trigger_frame_source_for(&mut self, explicit_video_id: Option<&str>) {
    if self.frame_mode == FrameMode::Thumbnail {
      return;
    }
    let video_id = if let Some(id) = explicit_video_id {
      id.to_string()
    } else {
      // Determine video_id from original_thumbnail or cached_thumbnail
      let found =
        self.frames.original_thumbnail.as_ref().or(self.player.cached_thumbnail.as_ref()).map(|(id, _)| id.clone());
      let Some(id) = found else { return };
      id
    };

    match self.frame_mode {
      FrameMode::Storyboard => {
        let client = self.player.http_client.clone();
        let vid = video_id;
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
          let _ = tx.send(fetch_sprite_frames(&client, &vid).await);
        });
        self.frames.source_rx = Some(rx);
      }
      FrameMode::Video => {
        let vid = video_id;
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
          let _ = tx.send(fetch_video_frames(&vid).await);
        });
        self.frames.source_rx = Some(rx);
      }
      FrameMode::Thumbnail => {}
    }
  }

  /// Convenience: trigger frame source using cached video state.
  fn trigger_frame_source(&mut self) {
    self.trigger_frame_source_for(None);
  }

  async fn check_pending(&mut self) -> Result<()> {
    if let Some(mut rx) = self.tasks.search_rx.take() {
      match rx.try_recv() {
        Ok(result) => {
          self.status_message = None;
          match result {
            Ok(results) if results.is_empty() => {
              self.last_error = Some("No results found.".to_string());
              self.channel_source = None;
            }
            Ok(results) => {
              let is_channel = self.channel_source.is_some();
              if let Some(ref mut source) = self.channel_source {
                source.total_fetched = results.len();
                if results.len() < CHANNEL_INITIAL_SIZE {
                  source.has_more = false;
                }
              }
              self.search_results = results;
              self.list_state.select(Some(0));
              self.mode = AppMode::Results;
              // Kick off background enrichment for channel results
              if is_channel {
                self.trigger_enrich();
              }
            }
            Err(e) => {
              self.last_error = Some(format!("Search failed: {:#}", e));
            }
          }
        }
        Err(oneshot::error::TryRecvError::Empty) => {
          self.tasks.search_rx = Some(rx);
        }
        Err(oneshot::error::TryRecvError::Closed) => {
          self.status_message = None;
          self.last_error = Some("Search task failed.".to_string());
        }
      }
    }

    if let Some(mut rx) = self.tasks.load_rx.take() {
      match rx.try_recv() {
        Ok(result) => {
          self.status_message = None;
          match result {
            Ok((video_id, details, thumbnail)) => {
              if let Err(e) = self.player.play(details).await {
                self.last_error = Some(format!("Playback error: {}", e));
                let _ = self.player.stop().await;
              } else if let Some(thumb) = thumbnail {
                self.frames.original_thumbnail = Some((video_id.clone(), thumb.clone()));
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
          self.tasks.load_rx = Some(rx);
        }
        Err(oneshot::error::TryRecvError::Closed) => {
          self.status_message = None;
          self.last_error = Some("Load task failed.".to_string());
        }
      }
    }

    // Check for background "load more" channel results
    if let Some(mut rx) = self.tasks.more_rx.take() {
      match rx.try_recv() {
        Ok(result) => {
          if let Some(ref mut source) = self.channel_source {
            source.loading_more = false;
          }
          let mut should_enrich = false;
          match result {
            Ok(new_results) => {
              if new_results.len() < CHANNEL_PAGE_SIZE
                && let Some(ref mut source) = self.channel_source
              {
                source.has_more = false;
              }
              if let Some(ref mut source) = self.channel_source {
                source.total_fetched += new_results.len();
              }
              if !new_results.is_empty() {
                should_enrich = true;
              }
              self.search_results.extend(new_results);
            }
            Err(e) => {
              self.last_error = Some(format!("Failed to load more: {:#}", e));
            }
          }
          if should_enrich {
            self.trigger_enrich();
          }
        }
        Err(oneshot::error::TryRecvError::Empty) => {
          self.tasks.more_rx = Some(rx);
        }
        Err(oneshot::error::TryRecvError::Closed) => {
          if let Some(ref mut source) = self.channel_source {
            source.loading_more = false;
          }
        }
      }
    }

    // Check for background frame source fetch
    if let Some(mut rx) = self.frames.source_rx.take() {
      match rx.try_recv() {
        Ok(Ok(fs)) => {
          self.frames.source = Some(fs);
        }
        Ok(Err(_)) => {
          // Frame source fetch failed silently — static thumbnail continues to work
        }
        Err(oneshot::error::TryRecvError::Empty) => {
          self.frames.source_rx = Some(rx);
        }
        Err(oneshot::error::TryRecvError::Closed) => {}
      }
    }

    // Drain enrichment results and apply to matching entries
    if let Some(ref mut rx) = self.tasks.enrich_rx {
      while let Ok(meta) = rx.try_recv() {
        if let Some(entry) = self.search_results.iter_mut().find(|e| e.video_id == meta.video_id) {
          entry.upload_date = meta.upload_date;
          entry.tags = meta.tags;
          entry.enriched = true;
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
    self.tasks.search_rx = None;
    self.tasks.more_rx = None;
    self.cancel_enrich();
    self.last_error = None;

    if let Some(channel_url) = detect_channel_url(&query) {
      // Channel listing mode
      self.status_message = Some("Loading channel…".to_string());
      self.channel_source =
        Some(ChannelSource { url: channel_url.clone(), total_fetched: 0, has_more: true, loading_more: false });

      let (tx, rx) = oneshot::channel();
      tokio::spawn(async move {
        let _ = tx.send(list_channel_videos(&channel_url, 1, CHANNEL_INITIAL_SIZE).await);
      });
      self.tasks.search_rx = Some(rx);
    } else {
      // Regular search mode
      self.status_message = Some(format!("Searching '{}'…", query));
      self.channel_source = None;

      let (tx, rx) = oneshot::channel();
      tokio::spawn(async move {
        let _ = tx.send(search_youtube(&query).await);
      });
      self.tasks.search_rx = Some(rx);
    }
  }

  /// Trigger a background fetch of the next page of channel videos.
  fn trigger_load_more(&mut self) {
    let Some(ref mut source) = self.channel_source else { return };
    if !source.has_more || source.loading_more {
      return;
    }
    source.loading_more = true;
    let url = source.url.clone();
    let start = source.total_fetched + 1;

    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
      let _ = tx.send(list_channel_videos(&url, start, CHANNEL_PAGE_SIZE).await);
    });
    self.tasks.more_rx = Some(rx);
  }

  /// Cancel any in-flight enrichment task.
  fn cancel_enrich(&mut self) {
    if let Some(handle) = self.tasks.enrich_handle.take() {
      handle.abort();
    }
    self.tasks.enrich_rx = None;
  }

  /// Spawn background enrichment for all unenriched entries in `search_results`.
  /// Existing enrichment tasks are cancelled first.
  fn trigger_enrich(&mut self) {
    self.cancel_enrich();

    let ids: Vec<String> = self.search_results.iter().filter(|e| !e.enriched).map(|e| e.video_id.clone()).collect();
    if ids.is_empty() {
      return;
    }

    let (tx, rx) = mpsc::channel(64);
    let handle = tokio::spawn(async move {
      enrich_video_metadata(ids, tx).await;
    });
    self.tasks.enrich_rx = Some(rx);
    self.tasks.enrich_handle = Some(handle);
  }

  fn trigger_load(&mut self) {
    let Some(selected) = self.list_state.selected() else { return };
    let Some(entry) = self.search_results.get(selected) else { return };

    let video_id = entry.video_id.clone();
    let upload_date = entry.upload_date.clone();
    let tags: Vec<String> = entry
      .tags
      .as_deref()
      .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
      .unwrap_or_default();
    let client = self.player.http_client.clone();
    self.last_error = None;
    self.status_message = Some("Loading…".to_string());
    // Clear previous frame source state
    self.frames.source = None;
    self.frames.source_rx = None;
    self.frames.idx = None;
    self.frames.original_thumbnail = None;

    let frame_vid = video_id.clone();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
      let details = get_video_info(&video_id).await;
      match details {
        Ok(mut d) => {
          if upload_date.is_some() {
            d.upload_date = upload_date;
          }
          if !tags.is_empty() {
            d.tags = tags;
          }
          let thumb = fetch_thumbnail(&client, &video_id).await.ok();
          let _ = tx.send(Ok((video_id, d, thumb)));
        }
        Err(e) => {
          let _ = tx.send(Err(e));
        }
      }
    });
    self.tasks.load_rx = Some(rx);

    // Spawn background frame source fetch based on current mode
    self.trigger_frame_source_for(Some(&frame_vid));
  }
}

// --- Helpers ---

/// Convert a char index to a byte offset within the string.
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
  s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

/// Parse the time position (in seconds) from an mpv status string.
///
/// Expects format: `Time: MM:SS / ... ` or `Time: H:MM:SS / ...`
fn parse_mpv_time_secs(status: &str) -> Option<f64> {
  let time_part = status.strip_prefix("Time: ")?.split(" / ").next()?.trim();
  let parts: Vec<&str> = time_part.split(':').collect();
  match parts.len() {
    2 => {
      let m: f64 = parts[0].parse().ok()?;
      let s: f64 = parts[1].parse().ok()?;
      Some(m * 60.0 + s)
    }
    3 => {
      let h: f64 = parts[0].parse().ok()?;
      let m: f64 = parts[1].parse().ok()?;
      let s: f64 = parts[2].parse().ok()?;
      Some(h * 3600.0 + m * 60.0 + s)
    }
    _ => None,
  }
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

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f') {
    app.next_frame_mode();
    return Ok(());
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
    if app.player.is_playing() {
      app.player.stop().await.context("Failed to stop playback")?;
      app.frames.source = None;
      app.frames.source_rx = None;
      app.frames.idx = None;
      app.frames.original_thumbnail = None;
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
          app.last_error = Some(format!("Failed to open browser: {}", e));
        }
      }
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
        // Trigger background load when within 5 items of the bottom
        if i + 5 >= count {
          app.trigger_load_more();
        }
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

    // Update frame source image if available and time position changed
    if let Some(ref frame_source) = app.frames.source
      && let Some(status) = app.player.get_last_mpv_status()
      && let Some(time_secs) = parse_mpv_time_secs(&status)
    {
      let idx = frame_source.frame_index_at(time_secs);
      if app.frames.idx != Some(idx)
        && let Some(frame) = frame_source.frame_at(time_secs)
      {
        let vid = frame_source.video_id().to_string();
        app.player.cached_thumbnail = Some((vid, frame));
        app.gfx.resized_thumb = None;
        app.gfx.last_sent = None;
        app.frames.idx = Some(idx);
      }
    }

    terminal.draw(|frame| ui::ui(frame, &mut app)).context("Failed to draw terminal frame")?;

    if uses_graphics_protocol {
      // Wrap graphics protocol output in synchronized update markers so the
      // terminal treats the ratatui cell updates + image data as one atomic
      // frame, preventing visible gaps between cell clear and image render.
      use std::io::Write;
      let mut stdout = std::io::stdout();
      write!(stdout, "\x1B[?2026h").context("Failed to write BeginSynchronizedUpdate")?;
      stdout.flush().context("Failed to flush BeginSynchronizedUpdate")?;

      if let Some(area) = app.gfx.thumb_area {
        if let Some((ref video_id, ref image)) = app.player.cached_thumbnail {
          let key = (video_id.clone(), area);
          if app.gfx.last_sent.as_ref() != Some(&key) {
            // Image ID i=1 with placement p=1 atomically replaces the
            // previous image — no need to delete first.
            match display_mode {
              DisplayMode::Kitty => kitty_render_image(image, area)?,
              DisplayMode::Sixel => sixel_render_image(image, area)?,
              _ => {}
            }
            app.gfx.last_sent = Some(key);
          }
        }
      } else if app.gfx.last_sent.is_some() {
        if display_mode == DisplayMode::Kitty {
          kitty_delete_placement()?;
        }
        app.gfx.last_sent = None;
      }

      write!(stdout, "\x1B[?2026l").context("Failed to write EndSynchronizedUpdate")?;
      stdout.flush().context("Failed to flush EndSynchronizedUpdate")?;
    }

    if event::poll(Duration::from_millis(100)).context("Failed to poll for terminal events")? {
      match event::read().context("Failed to read terminal event")? {
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
    kitty_delete_all().context("Failed to clean up Kitty graphics on exit")?;
  }
  app.player.stop().await.context("Failed to stop player on exit")?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  // --- parse_mpv_time_secs ---

  #[test]
  fn parse_mpv_time_mm_ss() {
    let status = "Time: 01:30 / 04:00 | Title: Song | no 37%";
    assert_eq!(parse_mpv_time_secs(status), Some(90.0));
  }

  #[test]
  fn parse_mpv_time_h_mm_ss() {
    let status = "Time: 1:02:03 / 2:00:00 | Title: Song | no 51%";
    assert_eq!(parse_mpv_time_secs(status), Some(3723.0));
  }

  #[test]
  fn parse_mpv_time_zero() {
    let status = "Time: 00:00 / 03:45 | Title: Song | no 0%";
    assert_eq!(parse_mpv_time_secs(status), Some(0.0));
  }

  #[test]
  fn parse_mpv_time_no_prefix() {
    assert_eq!(parse_mpv_time_secs("Something else"), None);
  }

  #[test]
  fn parse_mpv_time_garbage() {
    assert_eq!(parse_mpv_time_secs("Time: abc / def"), None);
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

  // --- FrameMode::from_config ---

  #[test]
  fn frame_mode_from_config_thumbnail() {
    assert_eq!(FrameMode::from_config("thumbnail"), FrameMode::Thumbnail);
    assert_eq!(FrameMode::from_config("Thumbnail"), FrameMode::Thumbnail);
  }

  #[test]
  fn frame_mode_from_config_storyboard() {
    assert_eq!(FrameMode::from_config("storyboard"), FrameMode::Storyboard);
    assert_eq!(FrameMode::from_config("STORYBOARD"), FrameMode::Storyboard);
  }

  #[test]
  fn frame_mode_from_config_video() {
    assert_eq!(FrameMode::from_config("video"), FrameMode::Video);
    assert_eq!(FrameMode::from_config("Video"), FrameMode::Video);
  }

  #[test]
  fn frame_mode_from_config_unknown_defaults_to_thumbnail() {
    assert_eq!(FrameMode::from_config("invalid"), FrameMode::Thumbnail);
    assert_eq!(FrameMode::from_config(""), FrameMode::Thumbnail);
  }
}
