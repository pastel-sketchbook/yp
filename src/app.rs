use anyhow::Result;
use image::DynamicImage;
use ratatui::{layout::Rect, widgets::ListState};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::constants::constants;
use crate::display::DisplayMode;
use crate::player::{MusicPlayer, VideoDetails};
use crate::theme::THEMES;
use crate::transcript::{self, TranscriptEvent, TranscriptState};
use crate::window;
use crate::youtube::{
  FrameSource, SearchEntry, VideoMeta, detect_channel_url, enrich_video_metadata, fetch_sprite_frames, fetch_thumbnail,
  fetch_video_frames, get_video_info, list_channel_videos, search_youtube,
};

// --- Types ---

pub type SearchResult = Vec<SearchEntry>;
pub type LoadResult = (String, VideoDetails, Option<DynamicImage>);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
  Input,
  Results,
  Filter,
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
pub(crate) struct FrameState {
  pub(crate) source: Option<FrameSource>,
  pub(crate) source_rx: Option<oneshot::Receiver<Result<FrameSource>>>,
  pub(crate) idx: Option<usize>,
  /// The original thumbnail image (before storyboard/video frames replace it).
  /// Used to restore the thumbnail when cycling frame modes.
  pub(crate) original_thumbnail: Option<(String, DynamicImage)>,
}

/// In-flight async task receivers and handles.
#[derive(Default)]
pub(crate) struct AsyncTasks {
  pub(crate) search_rx: Option<oneshot::Receiver<Result<SearchResult>>>,
  pub(crate) load_rx: Option<oneshot::Receiver<Result<LoadResult>>>,
  pub(crate) more_rx: Option<oneshot::Receiver<Result<SearchResult>>>,
  pub(crate) enrich_rx: Option<mpsc::Receiver<VideoMeta>>,
  pub(crate) enrich_handle: Option<JoinHandle<()>>,
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
  /// Informational message — shown with info icon, lower priority than status/error.
  pub info_message: Option<String>,
  pub should_quit: bool,
  pub channel_source: Option<ChannelSource>,
  pub input_scroll: usize,
  pub gfx: GraphicsCache,
  /// Filter text for narrowing search results by title/tags.
  pub filter: String,
  /// Cursor position within the filter input (char index).
  pub filter_cursor: usize,
  /// Horizontal scroll offset for the filter input.
  pub filter_scroll: usize,
  /// Indices into `search_results` that match the current filter.
  /// When filter is empty, contains all indices.
  pub filtered_indices: Vec<usize>,
  pub(crate) frames: FrameState,
  pub(crate) tasks: AsyncTasks,
  pub transcript_state: TranscriptState,
  /// Receiver for transcript pipeline events (extraction done, transcription done, errors).
  pub(crate) transcript_rx: Option<mpsc::UnboundedReceiver<TranscriptEvent>>,
  /// Completed transcript utterances with timestamps for time-synced display.
  pub utterances: Vec<whisper_cli::Utternace>,
  /// Whether the transcript pane is visible (toggled with Ctrl+A).
  pub transcript_visible: bool,
  /// Whisper model download progress (downloaded, total) for progress bar display.
  pub download_progress: Option<(u64, u64)>,
  /// Cached whisper model instance — loaded once, reused across transcriptions.
  /// The ~460MB model is expensive to load from disk; caching avoids repeated loads.
  whisper_cache: Arc<StdMutex<Option<whisper_cli::Whisper>>>,
  /// App start instant, used to drive UI animations (e.g. transcript progress indicator).
  pub started_at: Instant,
  /// PiP (picture-in-picture) mode — terminal window shrinks to show only Now Playing.
  pub pip_mode: bool,
  /// Saved window geometry before entering PiP, for restoration on toggle-off or exit.
  pip_original_geometry: Option<window::WindowGeometry>,
  /// Whether the terminal was in fullscreen before entering PiP.
  pip_was_fullscreen: bool,
  /// When the last error was set — used for auto-dismiss after 5 seconds.
  error_time: Option<Instant>,
}

impl App {
  pub fn new(display_mode: DisplayMode) -> Self {
    let config = Config::load();
    let theme_index =
      if let Some(ref name) = config.theme_name { THEMES.iter().position(|t| t.name == name).unwrap_or(0) } else { 0 };
    let frame_mode =
      if let Some(ref mode) = config.frame_mode { FrameMode::from_config(mode) } else { FrameMode::Thumbnail };

    let default_input = constants().pastel_sketchbook_channel.clone();
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
      info_message: None,
      should_quit: false,
      channel_source: None,
      input_scroll: 0,
      gfx: GraphicsCache::default(),
      filter: String::new(),
      filter_cursor: 0,
      filter_scroll: 0,
      filtered_indices: Vec::new(),
      frames: FrameState::default(),
      tasks: AsyncTasks::default(),
      transcript_state: TranscriptState::default(),
      transcript_rx: None,
      utterances: Vec::new(),
      transcript_visible: true,
      download_progress: None,
      whisper_cache: Arc::new(StdMutex::new(None)),
      started_at: Instant::now(),
      pip_mode: false,
      pip_original_geometry: None,
      pip_was_fullscreen: false,
      error_time: None,
    }
  }

  pub fn theme(&self) -> &'static crate::theme::Theme {
    // Safety: theme_index is always bounded by modular arithmetic in next_theme()
    // and clamped to THEMES.len() - 1 on initialization.
    &THEMES[self.theme_index]
  }

  /// Set an error message with auto-dismiss tracking.
  pub fn set_error(&mut self, msg: String) {
    self.last_error = Some(msg);
    self.error_time = Some(Instant::now());
  }

  /// Clear the current error message and its expiry timer.
  pub fn clear_error(&mut self) {
    self.last_error = None;
    self.error_time = None;
  }

  /// Clear stale error messages after 5 seconds.
  pub fn expire_error(&mut self) {
    if let Some(t) = self.error_time
      && t.elapsed() >= Duration::from_secs(5)
    {
      self.last_error = None;
      self.error_time = None;
    }
  }

  fn save_config(&self) {
    let config =
      Config { theme_name: Some(self.theme().name.to_string()), frame_mode: Some(self.frame_mode.label().to_string()) };
    config.save();
  }

  /// Check if a search entry matches the given filter string.
  /// Matches case-insensitively against both title and tags.
  pub fn matches_filter(entry: &SearchEntry, filter: &str) -> bool {
    if filter.is_empty() {
      return true;
    }
    let needle = filter.to_lowercase();
    if entry.title.to_lowercase().contains(&needle) {
      return true;
    }
    if let Some(ref tags) = entry.tags
      && tags.to_lowercase().contains(&needle)
    {
      return true;
    }
    false
  }

  /// Rebuild `filtered_indices` from `search_results` and the current filter.
  /// Clamps the list selection to stay within the filtered range.
  pub fn recompute_filter(&mut self) {
    if self.filter.is_empty() {
      self.filtered_indices = (0..self.search_results.len()).collect();
    } else {
      self.filtered_indices = self
        .search_results
        .iter()
        .enumerate()
        .filter(|(_, entry)| Self::matches_filter(entry, &self.filter))
        .map(|(i, _)| i)
        .collect();
    }
    // Clamp selection to new filtered range
    if self.filtered_indices.is_empty() {
      self.list_state.select(None);
    } else {
      let sel = self.list_state.selected().unwrap_or(0);
      if sel >= self.filtered_indices.len() {
        self.list_state.select(Some(self.filtered_indices.len().saturating_sub(1)));
      }
    }
  }

  pub fn next_theme(&mut self) {
    self.theme_index = (self.theme_index + 1) % THEMES.len();
    self.save_config();
  }

  pub fn next_frame_mode(&mut self) {
    // Safety: idx is bounded by position() returning 0..ALL.len()-1, and modular arithmetic
    // ensures (idx + 1) % ALL.len() is always in bounds. ALL is a non-empty const array.
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

  /// Clear all frame-related state. Used when stopping playback.
  pub fn clear_frame_state(&mut self) {
    self.frames.source = None;
    self.frames.source_rx = None;
    self.frames.idx = None;
    self.frames.original_thumbnail = None;
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

  // --- Auto-transcription ---

  /// Start the auto-transcription pipeline for the given YouTube URL.
  ///
  /// Architecture: chunked transcription for fast first results.
  /// 1. Resolve CDN stream URL (mpv IPC fast path ~0.5-4s, or yt-dlp -g fallback ~10-30s)
  /// 2. Download whisper model if needed
  /// 3. Loop: download 30s chunk via ffmpeg → transcribe → send utterances → next chunk
  ///
  /// First transcript appears in ~5-8s instead of ~50s.
  pub fn trigger_transcription(&mut self, url: &str) {
    // Cancel any in-progress transcription
    self.cancel_transcription();
    self.utterances.clear();
    self.download_progress = None;

    let (tx, rx) = mpsc::unbounded_channel();
    self.transcript_rx = Some(rx);

    let url = url.to_string();
    let whisper_cache = Arc::clone(&self.whisper_cache);
    let ipc_socket = self.player.ipc_socket_path().map(|s| s.to_string());

    info!(url = %url, "transcript: starting chunked transcription pipeline");

    let handle = transcript::spawn_transcription_pipeline(tx, url, whisper_cache, ipc_socket);

    self.transcript_state = TranscriptState::ExtractingAudio { handle };
  }

  /// Cancel any in-progress transcription pipeline.
  pub fn cancel_transcription(&mut self) {
    match std::mem::replace(&mut self.transcript_state, TranscriptState::Idle) {
      TranscriptState::ExtractingAudio { handle } => {
        info!("transcript: cancelling audio extraction");
        handle.abort();
      }
      TranscriptState::Transcribing { handle } => {
        info!("transcript: cancelling transcription");
        handle.abort();
      }
      _ => {}
    }
    self.transcript_rx = None;
    self.download_progress = None;
  }

  /// Handle Ctrl+A: toggle transcript visibility / cancel in-progress transcription.
  pub fn transcript_toggle(&mut self) {
    match self.transcript_state {
      TranscriptState::ExtractingAudio { .. } | TranscriptState::Transcribing { .. } => {
        // Cancel in-progress transcription
        debug!("transcript: toggle -> cancel");
        self.cancel_transcription();
        self.transcript_visible = false;
      }
      TranscriptState::Ready => {
        // Toggle visibility
        self.transcript_visible = !self.transcript_visible;
        debug!(visible = self.transcript_visible, "transcript: toggle visibility");
      }
      TranscriptState::Idle => {
        // Toggle visibility (show/hide even when empty)
        self.transcript_visible = !self.transcript_visible;
        debug!(visible = self.transcript_visible, "transcript: toggle visibility (idle)");
      }
    }
  }

  /// Toggle PiP (picture-in-picture) mode: shrink terminal to a small window showing
  /// only Now Playing, or restore to the original size.
  ///
  /// Handles fullscreen: if the terminal was fullscreen, exits fullscreen first,
  /// waits for the macOS animation, then resizes. On restore, re-enters fullscreen.
  pub async fn toggle_pip(&mut self) {
    if self.pip_mode {
      // Restore original window geometry
      if let Some(ref geom) = self.pip_original_geometry {
        info!("pip: restoring original window geometry");
        if let Err(e) = window::set_window_geometry(geom).await {
          warn!(err = %e, "pip: failed to restore window geometry");
          self.set_error(format!("PiP restore failed: {}", e));
        }
      }
      // Re-enter fullscreen if we were fullscreen before PiP
      if self.pip_was_fullscreen {
        info!("pip: re-entering fullscreen");
        // Brief delay for the resize to settle before fullscreen transition
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Err(e) = window::enter_fullscreen().await {
          warn!(err = %e, "pip: failed to re-enter fullscreen");
        }
      }
      self.pip_mode = false;
      self.pip_original_geometry = None;
      self.pip_was_fullscreen = false;
    } else {
      // Save current geometry and shrink to PiP
      match window::get_window_geometry().await {
        Ok(original) => {
          // Detect fullscreen by comparing window size to screen size
          let screen = window::get_screen_size().await.ok();
          let was_fullscreen = screen.is_some_and(|s| window::is_likely_fullscreen(&original, &s));

          if was_fullscreen {
            info!("pip: exiting fullscreen before PiP");
            if let Err(e) = window::exit_fullscreen().await {
              warn!(err = %e, "pip: failed to exit fullscreen");
              self.set_error(format!("PiP failed: could not exit fullscreen: {}", e));
              return;
            }
            // Wait for macOS fullscreen exit animation
            tokio::time::sleep(Duration::from_millis(750)).await;

            // Re-query geometry after exiting fullscreen (size will have changed)
            match window::get_window_geometry().await {
              Ok(post_fs) => self.pip_original_geometry = Some(post_fs),
              Err(_) => self.pip_original_geometry = Some(original),
            }
          } else {
            self.pip_original_geometry = Some(original);
          }
          self.pip_was_fullscreen = was_fullscreen;

          match window::pip_geometry().await {
            Ok(pip_geom) => {
              if let Err(e) = window::set_window_geometry(&pip_geom).await {
                warn!(err = %e, "pip: failed to set PiP geometry");
                self.set_error(format!("PiP failed: {}", e));
                self.pip_original_geometry = None;
                self.pip_was_fullscreen = false;
              } else {
                self.pip_mode = true;
                info!(was_fullscreen, "pip: entered PiP mode");
              }
            }
            Err(e) => {
              warn!(err = %e, "pip: failed to compute PiP geometry");
              self.set_error(format!("PiP failed: {}", e));
              self.pip_original_geometry = None;
              self.pip_was_fullscreen = false;
            }
          }
        }
        Err(e) => {
          warn!(err = %e, "pip: failed to get current window geometry");
          self.set_error(format!("PiP failed: {}", e));
        }
      }
    }
  }

  /// Restore window geometry if PiP is active. Called on exit to ensure the
  /// terminal returns to its original size.
  pub async fn restore_pip(&mut self) {
    if self.pip_mode {
      if let Some(ref geom) = self.pip_original_geometry {
        info!("pip: restoring window on exit");
        let _ = window::set_window_geometry(geom).await;
      }
      if self.pip_was_fullscreen {
        info!("pip: re-entering fullscreen on exit");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = window::enter_fullscreen().await;
      }
      self.pip_mode = false;
      self.pip_original_geometry = None;
      self.pip_was_fullscreen = false;
    }
  }

  pub async fn check_pending(&mut self) -> Result<()> {
    if let Some(mut rx) = self.tasks.search_rx.take() {
      match rx.try_recv() {
        Ok(result) => {
          self.status_message = None;
          match result {
            Ok(results) if results.is_empty() => {
              self.set_error("No results found.".to_string());
              self.channel_source = None;
            }
            Ok(results) => {
              let is_channel = self.channel_source.is_some();
              if let Some(ref mut source) = self.channel_source {
                source.total_fetched = results.len();
                if results.len() < constants().channel_initial_size {
                  source.has_more = false;
                }
              }
              self.search_results = results;
              self.recompute_filter();
              self.list_state.select(Some(0));
              self.mode = AppMode::Results;
              // Kick off background enrichment for channel results
              if is_channel {
                self.trigger_enrich();
              }
            }
            Err(e) => {
              self.set_error(format!("Search failed: {:#}", e));
            }
          }
        }
        Err(oneshot::error::TryRecvError::Empty) => {
          self.tasks.search_rx = Some(rx);
        }
        Err(oneshot::error::TryRecvError::Closed) => {
          self.status_message = None;
          self.set_error("Search task failed.".to_string());
        }
      }
    }

    if let Some(mut rx) = self.tasks.load_rx.take() {
      match rx.try_recv() {
        Ok(result) => {
          self.status_message = None;
          match result {
            Ok((video_id, details, thumbnail)) => {
              let play_url = details.url.clone();
              if let Err(e) = self.player.play(details).await {
                self.set_error(format!("Playback error: {}", e));
                let _ = self.player.stop().await;
              } else {
                // Auto-trigger transcription for the new track
                self.trigger_transcription(&play_url);
                if let Some(thumb) = thumbnail {
                  self.frames.original_thumbnail = Some((video_id.clone(), thumb.clone()));
                  self.player.cached_thumbnail = Some((video_id, thumb));
                }
              }
              self.mode = AppMode::Input;
            }
            Err(e) => {
              self.set_error(format!("Failed to load: {:#}", e));
            }
          }
        }
        Err(oneshot::error::TryRecvError::Empty) => {
          self.tasks.load_rx = Some(rx);
        }
        Err(oneshot::error::TryRecvError::Closed) => {
          self.status_message = None;
          self.set_error("Load task failed.".to_string());
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
              if new_results.len() < constants().channel_page_size
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
              self.recompute_filter();
            }
            Err(e) => {
              self.set_error(format!("Failed to load more: {:#}", e));
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
      let mut updated = false;
      while let Ok(meta) = rx.try_recv() {
        if let Some(entry) = self.search_results.iter_mut().find(|e| e.video_id == meta.video_id) {
          entry.upload_date = meta.upload_date;
          entry.tags = meta.tags;
          entry.enriched = true;
          updated = true;
        }
      }
      // Recompute filter when enrichment adds tags that might match/unmatch
      if updated && !self.filter.is_empty() {
        self.recompute_filter();
      }
    }

    // --- Auto-transcription polling ---

    // Poll transcript pipeline events
    if let Some(ref mut rx) = self.transcript_rx {
      while let Ok(event) = rx.try_recv() {
        match event {
          TranscriptEvent::AudioExtracted => {
            // Transition from ExtractingAudio to Transcribing.
            // The handle stays the same (single spawned task covers both stages).
            let old = std::mem::replace(&mut self.transcript_state, TranscriptState::Idle);
            if let TranscriptState::ExtractingAudio { handle } = old {
              self.transcript_state = TranscriptState::Transcribing { handle };
            }
          }
          TranscriptEvent::DownloadProgress(downloaded, total) => {
            self.download_progress = Some((downloaded, total));
          }
          TranscriptEvent::ChunkTranscribed(chunk_utterances) => {
            info!(
              segments = chunk_utterances.len(),
              total = self.utterances.len() + chunk_utterances.len(),
              "transcript: chunk arrived"
            );
            self.utterances.extend(chunk_utterances);
            self.transcript_visible = true;
            self.download_progress = None;
          }
          TranscriptEvent::Transcribed => {
            info!(total_segments = self.utterances.len(), "transcript: all chunks complete");
            self.transcript_state = TranscriptState::Ready;
            self.transcript_visible = true;
            self.download_progress = None;
            self.transcript_rx = None;
            break;
          }
          TranscriptEvent::Failed(msg) => {
            error!(err = %msg, "transcript: pipeline failed");
            self.set_error(msg);
            self.transcript_state = TranscriptState::Idle;
            self.download_progress = None;
            self.transcript_rx = None;
            break;
          }
        }
      }
    }

    Ok(())
  }

  pub fn trigger_search(&mut self) {
    let query = self.input.trim().to_string();
    if query.is_empty() {
      self.set_error("Enter a search term.".to_string());
      return;
    }
    info!(query = %query, "search triggered");
    self.tasks.search_rx = None;
    self.tasks.more_rx = None;
    self.cancel_enrich();
    self.clear_error();
    self.info_message = None;
    // Clear filter state on new search
    self.filter.clear();
    self.filter_cursor = 0;
    self.filter_scroll = 0;

    if let Some(channel_url) = detect_channel_url(&query) {
      // Channel listing mode
      self.status_message = Some("Loading channel…".to_string());
      self.channel_source =
        Some(ChannelSource { url: channel_url.clone(), total_fetched: 0, has_more: true, loading_more: false });

      let (tx, rx) = oneshot::channel();
      tokio::spawn(async move {
        let _ = tx.send(list_channel_videos(&channel_url, 1, constants().channel_initial_size).await);
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
  pub fn trigger_load_more(&mut self) {
    let Some(ref mut source) = self.channel_source else { return };
    if !source.has_more || source.loading_more {
      return;
    }
    source.loading_more = true;
    let url = source.url.clone();
    let start = source.total_fetched + 1;

    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
      let _ = tx.send(list_channel_videos(&url, start, constants().channel_page_size).await);
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

  pub fn trigger_load(&mut self) {
    let Some(selected) = self.list_state.selected() else { return };
    // Map through filtered_indices to get the actual search_results index
    let actual_idx = if self.filtered_indices.is_empty() {
      return;
    } else {
      let Some(&idx) = self.filtered_indices.get(selected) else { return };
      idx
    };
    let Some(entry) = self.search_results.get(actual_idx) else { return };

    let video_id = entry.video_id.clone();
    let upload_date = entry.upload_date.clone();
    let tags: Vec<String> = entry
      .tags
      .as_deref()
      .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
      .unwrap_or_default();
    let client = self.player.http_client.clone();
    self.clear_error();
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

  /// Access the frame source for the run loop to check frame updates.
  pub fn frame_source(&self) -> Option<&FrameSource> {
    self.frames.source.as_ref()
  }

  /// Get the current frame index.
  pub fn frame_idx(&self) -> Option<usize> {
    self.frames.idx
  }

  /// Set the current frame index.
  pub fn set_frame_idx(&mut self, idx: usize) {
    self.frames.idx = Some(idx);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

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

  // --- matches_filter ---

  fn make_entry(title: &str, tags: Option<&str>) -> SearchEntry {
    SearchEntry {
      title: title.to_string(),
      video_id: "test123".to_string(),
      upload_date: None,
      tags: tags.map(|s| s.to_string()),
      enriched: false,
    }
  }

  #[test]
  fn matches_filter_empty_filter_matches_all() {
    let entry = make_entry("Any Title", None);
    assert!(App::matches_filter(&entry, ""));
  }

  #[test]
  fn matches_filter_title_match() {
    let entry = make_entry("Rock Music Mix", None);
    assert!(App::matches_filter(&entry, "rock"));
    assert!(App::matches_filter(&entry, "MUSIC"));
    assert!(App::matches_filter(&entry, "mix"));
  }

  #[test]
  fn matches_filter_tag_match() {
    let entry = make_entry("Some Video", Some("rock, guitar, blues"));
    assert!(App::matches_filter(&entry, "guitar"));
    assert!(App::matches_filter(&entry, "BLUES"));
  }

  #[test]
  fn matches_filter_no_match() {
    let entry = make_entry("Piano Sonata", Some("classical, piano"));
    assert!(!App::matches_filter(&entry, "rock"));
    assert!(!App::matches_filter(&entry, "guitar"));
  }

  #[test]
  fn matches_filter_no_tags() {
    let entry = make_entry("Guitar Solo", None);
    assert!(App::matches_filter(&entry, "guitar"));
    assert!(!App::matches_filter(&entry, "piano"));
  }

  #[test]
  fn matches_filter_case_insensitive() {
    let entry = make_entry("ABC Def GHI", Some("Jazz, Funk"));
    assert!(App::matches_filter(&entry, "abc"));
    assert!(App::matches_filter(&entry, "DEF"));
    assert!(App::matches_filter(&entry, "jazz"));
    assert!(App::matches_filter(&entry, "FUNK"));
  }
}
