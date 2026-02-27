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
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

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

// --- Voice Input ---

/// Maximum recording duration in seconds (safety guard).
const VOICE_MAX_DURATION_SECS: u64 = 15;

/// HuggingFace URL for the whisper.cpp small model (~460 MB).
const WHISPER_MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin";

/// Progress events from the model download task.
pub enum DownloadEvent {
  /// Intermediate progress: (bytes downloaded, total bytes).
  Progress(u64, u64),
  /// Download finished (Ok) or failed (Err).
  Complete(Result<()>),
}

/// Return the local path where the whisper model should live.
fn whisper_model_path() -> std::path::PathBuf {
  let data_dir = directories::BaseDirs::new()
    .map(|d| d.data_dir().to_path_buf())
    .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
  data_dir.join("whisper-cpp/models/ggml-small.bin")
}

/// Voice input state machine.
#[derive(Default)]
pub enum VoiceState {
  /// No voice activity.
  #[default]
  Idle,
  /// sox `rec` is recording audio to a temp WAV file.
  Recording { child: tokio::process::Child, wav_path: std::path::PathBuf, started: Instant },
  /// Downloading the whisper model from HuggingFace.
  Downloading {
    rx: mpsc::UnboundedReceiver<DownloadEvent>,
    wav_path: std::path::PathBuf,
    /// Bytes downloaded so far — updated by polling `rx`.
    downloaded: u64,
    /// Total bytes (from Content-Length) — updated by polling `rx`.
    total: u64,
  },
  /// whisper-cli is transcribing the recorded WAV file.
  Transcribing { rx: oneshot::Receiver<Result<String>>, wav_path: std::path::PathBuf },
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
  /// Informational message — shown with ℹ icon, lower priority than status/error.
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
  frames: FrameState,
  tasks: AsyncTasks,
  pub voice: VoiceState,
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
      voice: VoiceState::default(),
    }
  }

  pub fn theme(&self) -> &'static theme::Theme {
    // Safety: theme_index is always bounded by modular arithmetic in next_theme()
    // and clamped to THEMES.len() - 1 on initialization.
    &THEMES[self.theme_index]
  }

  fn save_config(&self) {
    let config =
      Config { theme_name: Some(self.theme().name.to_string()), frame_mode: Some(self.frame_mode.label().to_string()) };
    config.save();
  }

  /// Check if a search entry matches the given filter string.
  /// Matches case-insensitively against both title and tags.
  fn matches_filter(entry: &SearchEntry, filter: &str) -> bool {
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
  fn recompute_filter(&mut self) {
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

  fn next_theme(&mut self) {
    self.theme_index = (self.theme_index + 1) % THEMES.len();
    self.save_config();
  }

  fn next_frame_mode(&mut self) {
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

  // --- Voice Input ---

  /// Start recording audio via sox `rec` with auto-silence detection.
  fn voice_start_recording(&mut self) {
    let wav_path = std::env::temp_dir().join(format!("yp-voice-{}.wav", std::process::id()));
    info!(path = %wav_path.display(), "voice: start recording");
    // Remove stale file from previous recording
    let _ = std::fs::remove_file(&wav_path);

    // sox `rec` args:
    //   rate 16000       — 16kHz sample rate (what Whisper expects)
    //   channels 1       — mono
    //
    // No silence effect — it trims audio below the threshold, producing
    // empty files when mic levels are low. Instead we record continuously
    // and rely on Ctrl+A (manual stop) or the max duration safety guard.
    let child = tokio::process::Command::new("rec")
      .args([wav_path.to_str().unwrap_or("/tmp/yp-voice.wav"), "rate", "16000", "channels", "1"])
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .spawn();

    match child {
      Ok(child) => {
        debug!(pid = child.id(), "voice: sox spawned");
        self.voice = VoiceState::Recording { child, wav_path, started: Instant::now() };
        self.status_message = Some("Recording…".to_string());
        self.last_error = None;
        self.info_message = None;
      }
      Err(e) => {
        error!(err = %e, "voice: failed to spawn sox");
        if e.kind() == std::io::ErrorKind::NotFound {
          self.last_error = Some("sox not found. Install with: brew install sox".to_string());
        } else {
          self.last_error = Some(format!("Failed to start recording: {}", e));
        }
      }
    }
  }

  /// Stop recording and proceed to transcription (downloading model first if needed).
  /// Sends SIGTERM so sox can finalize the WAV header; falls back to SIGKILL after timeout.
  async fn voice_stop_recording(&mut self) {
    let state = std::mem::replace(&mut self.voice, VoiceState::Idle);
    if let VoiceState::Recording { mut child, wav_path, started } = state {
      let elapsed = started.elapsed();
      info!(elapsed_ms = elapsed.as_millis() as u64, path = %wav_path.display(), "voice: stop recording");
      // Send SIGTERM to let sox finalize the WAV file header.
      // SIGKILL would corrupt the header, causing whisper to read garbage.
      if let Some(pid) = child.id() {
        debug!(pid, "voice: sending SIGTERM to sox");
        let _ = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status();
      }
      // Wait up to 2 seconds for graceful exit, then force kill.
      match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(status)) => {
          debug!(code = ?status.code(), "voice: sox exited gracefully");
        }
        Ok(Err(e)) => {
          error!(err = %e, "voice: sox wait error");
        }
        Err(_) => {
          error!("voice: sox did not exit in 2s, sending SIGKILL");
          let _ = child.kill().await;
          let _ = child.wait().await;
        }
      }
      // Log WAV file size for debugging
      if let Ok(meta) = std::fs::metadata(&wav_path) {
        debug!(size_bytes = meta.len(), "voice: WAV file written");
      }
      self.voice_ensure_model_then_transcribe(wav_path);
    }
  }

  /// Check if whisper model exists; if not, start downloading it; otherwise start transcription.
  /// Skips transcription entirely if the WAV file is empty (header-only, no audio data).
  fn voice_ensure_model_then_transcribe(&mut self, wav_path: std::path::PathBuf) {
    // A WAV header with no audio data is ~44-80 bytes. Skip transcription for empty recordings.
    const MIN_WAV_SIZE: u64 = 256;
    let wav_size = std::fs::metadata(&wav_path).map(|m| m.len()).unwrap_or(0);
    if wav_size < MIN_WAV_SIZE {
      info!(size_bytes = wav_size, "voice: WAV too small, skipping transcription");
      let _ = std::fs::remove_file(&wav_path);
      self.status_message = None;
      self.info_message = Some("No speech detected".to_string());
      return;
    }

    let model_path = whisper_model_path();
    if model_path.exists() {
      debug!(model = %model_path.display(), "voice: model exists, starting transcription");
      self.voice_start_transcription(wav_path);
    } else {
      info!(model = %model_path.display(), "voice: model not found, starting download");
      self.voice_download_model(wav_path);
    }
  }

  /// Start background download of the whisper model from HuggingFace.
  fn voice_download_model(&mut self, wav_path: std::path::PathBuf) {
    let (tx, rx) = mpsc::unbounded_channel();
    let model_path = whisper_model_path();
    info!(model = %model_path.display(), "voice: starting model download");

    tokio::spawn(async move {
      let result = download_whisper_model(&model_path, &tx).await;
      if let Err(ref e) = result {
        error!(err = %e, "voice: model download failed");
      } else {
        info!("voice: model download complete");
      }
      // Send completion event — if receiver is dropped (user cancelled), that's fine.
      let _ = tx.send(DownloadEvent::Complete(result));
    });

    self.status_message = None; // progress bar is rendered by the UI directly
    self.voice = VoiceState::Downloading { rx, wav_path, downloaded: 0, total: 0 };
  }

  /// Spawn whisper-cli to transcribe the WAV file.
  fn voice_start_transcription(&mut self, wav_path: std::path::PathBuf) {
    info!(path = %wav_path.display(), "voice: start transcription");
    self.status_message = Some("Transcribing…".to_string());

    let (tx, rx) = oneshot::channel();
    let wav = wav_path.clone();
    let model = whisper_model_path();
    tokio::spawn(async move {
      let model_str = model.to_string_lossy().to_string();
      debug!(model = %model_str, wav = %wav.display(), "voice: spawning whisper-cli");
      let result = tokio::process::Command::new("whisper-cli")
        .args(["-m", &model_str, "-f", wav.to_str().unwrap_or(""), "--no-timestamps", "-l", "auto"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;

      let text = match result {
        Ok(output) if output.status.success() => {
          let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
          let stderr_raw = String::from_utf8_lossy(&output.stderr).trim().to_string();
          debug!(stdout_len = raw.len(), stderr_len = stderr_raw.len(), "voice: whisper-cli succeeded");
          debug!(raw_stdout = %raw, "voice: whisper raw output");
          // whisper-cli may output bracketed markers like [BLANK_AUDIO] — filter them
          let cleaned: String =
            raw.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('[')).collect::<Vec<_>>().join(" ");
          info!(cleaned_len = cleaned.len(), cleaned = %cleaned, "voice: transcription result");
          Ok(cleaned)
        }
        Ok(output) => {
          let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
          error!(code = ?output.status.code(), stderr = %stderr, "voice: whisper-cli failed");
          if stderr.contains("failed to open") || stderr.contains("no such file") {
            Err(anyhow::anyhow!("Whisper model not found at {}", model_str))
          } else {
            Err(anyhow::anyhow!("whisper-cli failed: {}", stderr))
          }
        }
        Err(e) => {
          error!(err = %e, "voice: failed to spawn whisper-cli");
          if e.kind() == std::io::ErrorKind::NotFound {
            Err(anyhow::anyhow!("whisper-cli not found. Install with: brew install whisper-cpp"))
          } else {
            Err(anyhow::anyhow!("Failed to run whisper-cli: {}", e))
          }
        }
      };
      let _ = tx.send(text);
    });
    self.voice = VoiceState::Transcribing { rx, wav_path };
  }

  /// Handle Ctrl+A toggle: start recording or stop recording.
  async fn voice_toggle(&mut self) {
    match self.voice {
      VoiceState::Idle => {
        debug!("voice: toggle -> start recording");
        self.voice_start_recording();
      }
      VoiceState::Recording { .. } => {
        debug!("voice: toggle -> stop recording");
        self.voice_stop_recording().await;
      }
      VoiceState::Downloading { .. } | VoiceState::Transcribing { .. } => {
        debug!("voice: toggle ignored (busy)");
      }
    }
  }

  /// Insert transcribed text into the current input field (search or filter).
  fn voice_insert_text(&mut self, text: &str) {
    let (input, cursor) = match self.mode {
      AppMode::Filter => (&mut self.filter, &mut self.filter_cursor),
      _ => (&mut self.input, &mut self.cursor_position),
    };
    let byte_idx = char_to_byte_index(input, *cursor);
    input.insert_str(byte_idx, text);
    *cursor += text.chars().count();

    // Recompute filter if we're in filter mode
    if self.mode == AppMode::Filter {
      self.recompute_filter();
    }
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
              self.recompute_filter();
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
              self.recompute_filter();
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

    // --- Voice input polling ---

    // Check recording max duration safety guard
    if let VoiceState::Recording { ref started, .. } = self.voice
      && started.elapsed() >= Duration::from_secs(VOICE_MAX_DURATION_SECS)
    {
      info!(max_secs = VOICE_MAX_DURATION_SECS, "voice: max duration reached, stopping");
      self.voice_stop_recording().await;
    }

    // Check if sox recording process has exited (auto-silence triggered)
    if let VoiceState::Recording { ref mut child, .. } = self.voice
      && let Some(status) = child.try_wait().ok().flatten()
    {
      info!(code = ?status.code(), "voice: sox exited (auto-silence)");
      // sox exited — take the state and check model before transcription
      let state = std::mem::replace(&mut self.voice, VoiceState::Idle);
      if let VoiceState::Recording { wav_path, .. } = state {
        if let Ok(meta) = std::fs::metadata(&wav_path) {
          debug!(size_bytes = meta.len(), "voice: WAV file from auto-silence");
        }
        self.voice_ensure_model_then_transcribe(wav_path);
      }
    }

    // Poll model download progress
    if let VoiceState::Downloading { ref mut rx, ref mut downloaded, ref mut total, .. } = self.voice {
      // Drain all available progress messages, keeping the latest values
      while let Ok(event) = rx.try_recv() {
        match event {
          DownloadEvent::Progress(d, t) => {
            *downloaded = d;
            *total = t;
          }
          DownloadEvent::Complete(result) => {
            let state = std::mem::replace(&mut self.voice, VoiceState::Idle);
            if let VoiceState::Downloading { wav_path, .. } = state {
              match result {
                Ok(()) => {
                  self.voice_start_transcription(wav_path);
                }
                Err(e) => {
                  self.last_error = Some(format!("Model download failed: {}", e));
                  // Clean up the wav file
                  let _ = std::fs::remove_file(&wav_path);
                }
              }
            }
            break;
          }
        }
      }
    }

    // Check if whisper-cli transcription has completed
    if let VoiceState::Transcribing { ref mut rx, .. } = self.voice
      && let Ok(result) = rx.try_recv()
    {
      let state = std::mem::replace(&mut self.voice, VoiceState::Idle);
      if let VoiceState::Transcribing { wav_path, .. } = state {
        debug!(path = %wav_path.display(), "voice: cleaning up WAV file");
        let _ = std::fs::remove_file(&wav_path);
      }
      self.status_message = None;
      match result {
        Ok(text) if text.is_empty() => {
          info!("voice: no speech detected (empty transcription)");
          self.info_message = Some("No speech detected".to_string());
        }
        Ok(text) => {
          info!(len = text.len(), text = %text, "voice: inserting transcribed text");
          self.voice_insert_text(&text);
        }
        Err(e) => {
          error!(err = %e, "voice: transcription error");
          self.last_error = Some(format!("{}", e));
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
    info!(query = %query, "search triggered");
    self.tasks.search_rx = None;
    self.tasks.more_rx = None;
    self.cancel_enrich();
    self.last_error = None;
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

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('a') {
    app.voice_toggle().await;
    return Ok(());
  }

  match app.mode {
    AppMode::Input => handle_input_key(app, key),
    AppMode::Results => handle_results_key(app, key).await?,
    AppMode::Filter => handle_filter_key(app, key).await?,
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
          && actual_idx + 5 >= app.search_results.len()
        {
          app.trigger_load_more();
        }
      }
    }
    KeyCode::Up | KeyCode::Char('k') => {
      let count = app.filtered_indices.len();
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
          && actual_idx + 5 >= app.search_results.len()
        {
          app.trigger_load_more();
        }
      }
    }
    KeyCode::Up => {
      let count = app.filtered_indices.len();
      if count > 0 {
        let i = app.list_state.selected().map_or(0, |i| if i == 0 { count - 1 } else { i - 1 });
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

// --- Whisper Model Download ---

/// Download the whisper model file from HuggingFace, streaming to disk with progress updates.
async fn download_whisper_model(model_path: &std::path::Path, tx: &mpsc::UnboundedSender<DownloadEvent>) -> Result<()> {
  use futures::StreamExt;
  use tokio::io::AsyncWriteExt;

  // Create parent directory
  if let Some(parent) = model_path.parent() {
    tokio::fs::create_dir_all(parent).await.context("Failed to create whisper model directory")?;
  }

  let response = reqwest::get(WHISPER_MODEL_URL).await.context("Failed to connect to HuggingFace")?;
  if !response.status().is_success() {
    anyhow::bail!("Download failed: HTTP {}", response.status());
  }

  let total = response.content_length().unwrap_or(0);
  let _ = tx.send(DownloadEvent::Progress(0, total));

  // Write to a temp file first, rename on success to avoid partial files.
  let tmp_path = model_path.with_extension("bin.part");
  let mut file = tokio::fs::File::create(&tmp_path).await.context("Failed to create model file")?;

  let mut stream = response.bytes_stream();
  let mut downloaded: u64 = 0;

  while let Some(chunk) = stream.next().await {
    let chunk = chunk.context("Download interrupted")?;
    file.write_all(&chunk).await.context("Failed to write model data")?;
    downloaded += chunk.len() as u64;
    let _ = tx.send(DownloadEvent::Progress(downloaded, total));
  }

  file.flush().await.context("Failed to flush model file")?;
  drop(file);

  // Atomic-ish rename from .part to final path
  tokio::fs::rename(&tmp_path, model_path).await.context("Failed to rename downloaded model")?;

  Ok(())
}

// --- Main ---

#[tokio::main]
async fn main() -> Result<()> {
  // --- Daily file logging ---
  let log_dir = directories::BaseDirs::new()
    .map(|d| d.data_dir().join("yp/logs"))
    .unwrap_or_else(|| std::path::PathBuf::from("/tmp/yp/logs"));
  let file_appender = tracing_appender::rolling::daily(&log_dir, "yp.log");
  let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
  tracing_subscriber::fmt()
    .with_writer(non_blocking)
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("yp=debug".parse().unwrap()))
    .with_ansi(false)
    .with_target(false)
    .init();

  info!("yp v{} starting", env!("CARGO_PKG_VERSION"));

  // Remove old log files (keep only today's)
  let today = chrono::Local::now().format("%Y-%m-%d").to_string();
  if let Ok(entries) = std::fs::read_dir(&log_dir) {
    for entry in entries.flatten() {
      let name = entry.file_name();
      let name = name.to_string_lossy();
      if name.starts_with("yp.log.") && !name.ends_with(&today) {
        let _ = std::fs::remove_file(entry.path());
      }
    }
  }

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
  info!(display_mode = ?display_mode, "display mode resolved");
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
