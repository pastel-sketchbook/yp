use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use clap::{Parser, ValueEnum};
use color_quant::NeuQuant;
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use ratatui::{
  DefaultTerminal, Frame,
  buffer::Buffer,
  crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
  layout::{Alignment, Constraint, Layout, Rect},
  style::{Color, Modifier, Style, Stylize},
  text::{Line, Span},
  widgets::{Block, List, ListItem, ListState, Padding, Paragraph, Widget},
};
use reqwest::Client;
use std::{
  io::{Cursor, Write},
  process::Stdio,
  sync::{Arc, Mutex},
  time::Duration,
};
use tokio::{
  io::AsyncBufReadExt,
  io::BufReader as TokioBufReader,
  process::{Child as TokioChild, Command},
  sync::{mpsc, oneshot},
  task::JoinHandle,
};

// --- CLI ---

#[derive(Parser, Debug)]
#[command(author, version = env!("CARGO_PKG_VERSION"), about, long_about = None)]
struct Args {
  /// Display mode: 'auto', 'kitty', 'sixel', 'direct', or 'ascii' (default: auto-detect)
  #[arg(short, long, default_value = "auto")]
  display_mode: CliDisplayMode,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliDisplayMode {
  Auto,
  Kitty,
  Sixel,
  Direct,
  Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayMode {
  Ascii,
  Direct,
  Sixel,
  Kitty,
}

impl DisplayMode {
  fn label(self) -> &'static str {
    match self {
      DisplayMode::Ascii => "ASCII",
      DisplayMode::Direct => "Half-block",
      DisplayMode::Sixel => "Sixel",
      DisplayMode::Kitty => "Kitty",
    }
  }
}

/// Detect the best display mode the terminal supports.
///
/// Probe order: Kitty graphics > Sixel > true-color half-block > ASCII
///
/// - Kitty: `TERM=xterm-kitty`, or `TERM_PROGRAM` is kitty/WezTerm/ghostty
/// - Sixel: `TERM_PROGRAM` is foot/mlterm, or `TERM` contains "sixel"
/// - Direct: `COLORTERM` is `truecolor` or `24bit`
/// - Ascii: fallback
fn detect_display_mode() -> DisplayMode {
  let term = std::env::var("TERM").unwrap_or_default();
  let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default().to_lowercase();

  // Terminals known to support the Kitty graphics protocol
  if term == "xterm-kitty" || matches!(term_program.as_str(), "kitty" | "wezterm" | "ghostty") {
    return DisplayMode::Kitty;
  }

  // Terminals known to support Sixel graphics
  if matches!(term_program.as_str(), "foot" | "mlterm" | "contour") || term.contains("sixel") {
    return DisplayMode::Sixel;
  }

  let colorterm = std::env::var("COLORTERM").unwrap_or_default().to_lowercase();
  if colorterm == "truecolor" || colorterm == "24bit" {
    return DisplayMode::Direct;
  }

  DisplayMode::Ascii
}

fn resolve_display_mode(cli: CliDisplayMode) -> DisplayMode {
  match cli {
    CliDisplayMode::Auto => detect_display_mode(),
    CliDisplayMode::Kitty => DisplayMode::Kitty,
    CliDisplayMode::Sixel => DisplayMode::Sixel,
    CliDisplayMode::Direct => DisplayMode::Direct,
    CliDisplayMode::Ascii => DisplayMode::Ascii,
  }
}

type SearchResult = Vec<(String, String)>;
type LoadResult = (VideoDetails, Option<DynamicImage>);

// --- Theme ---

#[derive(Debug, Clone, Copy)]
struct Theme {
  name: &'static str,
  bg: Color,
  fg: Color,
  accent: Color,
  muted: Color,
  border: Color,
  error: Color,
  status: Color,
  highlight_bg: Color,
  highlight_fg: Color,
  stripe_bg: Color,
  key_bg: Color,
  key_fg: Color,
}

const THEMES: &[Theme] = &[
  // Default — dark, cyan accent
  Theme {
    name: "Default",
    bg: Color::Reset,
    fg: Color::White,
    accent: Color::Rgb(0, 217, 255),
    muted: Color::DarkGray,
    border: Color::DarkGray,
    error: Color::Rgb(255, 80, 80),
    status: Color::Rgb(0, 217, 255),

    highlight_bg: Color::Rgb(40, 40, 60),
    highlight_fg: Color::Rgb(255, 220, 100),
    stripe_bg: Color::Rgb(28, 28, 34),
    key_bg: Color::DarkGray,
    key_fg: Color::Black,
  },
  // Gruvbox Dark
  Theme {
    name: "Gruvbox",
    bg: Color::Rgb(29, 32, 33),
    fg: Color::Rgb(235, 219, 178),
    accent: Color::Rgb(215, 153, 33),
    muted: Color::Rgb(146, 131, 116),
    border: Color::Rgb(62, 57, 54),
    error: Color::Rgb(251, 73, 52),
    status: Color::Rgb(184, 187, 38),

    highlight_bg: Color::Rgb(50, 48, 47),
    highlight_fg: Color::Rgb(250, 189, 47),
    stripe_bg: Color::Rgb(40, 40, 40),
    key_bg: Color::Rgb(80, 73, 69),
    key_fg: Color::Rgb(235, 219, 178),
  },
  // Solarized Dark
  Theme {
    name: "Solarized",
    bg: Color::Rgb(0, 43, 54),
    fg: Color::Rgb(253, 246, 227),
    accent: Color::Rgb(42, 161, 152),
    muted: Color::Rgb(131, 148, 150),
    border: Color::Rgb(16, 58, 68),
    error: Color::Rgb(220, 50, 47),
    status: Color::Rgb(181, 137, 0),
    highlight_bg: Color::Rgb(7, 54, 66),
    highlight_fg: Color::Rgb(253, 246, 227),
    stripe_bg: Color::Rgb(3, 48, 58),
    key_bg: Color::Rgb(88, 110, 117),
    key_fg: Color::Rgb(253, 246, 227),
  },
  // Flexoki Dark
  Theme {
    name: "Flexoki",
    bg: Color::Rgb(16, 15, 15),
    fg: Color::Rgb(206, 205, 195),
    accent: Color::Rgb(36, 131, 123),
    muted: Color::Rgb(135, 133, 128),
    border: Color::Rgb(40, 39, 38),
    error: Color::Rgb(209, 77, 65),
    status: Color::Rgb(208, 162, 21),
    highlight_bg: Color::Rgb(28, 27, 26),
    highlight_fg: Color::Rgb(208, 162, 21),
    stripe_bg: Color::Rgb(22, 21, 20),
    key_bg: Color::Rgb(52, 51, 49),
    key_fg: Color::Rgb(206, 205, 195),
  },
  // Ayu Dark
  Theme {
    name: "Ayu",
    bg: Color::Rgb(10, 14, 20),
    fg: Color::Rgb(191, 191, 191),
    accent: Color::Rgb(255, 153, 64),
    muted: Color::Rgb(92, 103, 115),
    border: Color::Rgb(40, 44, 52),
    error: Color::Rgb(240, 113, 113),
    status: Color::Rgb(85, 180, 211),
    highlight_bg: Color::Rgb(20, 24, 32),
    highlight_fg: Color::Rgb(255, 180, 84),
    stripe_bg: Color::Rgb(15, 19, 26),
    key_bg: Color::Rgb(60, 66, 76),
    key_fg: Color::Rgb(191, 191, 191),
  },
  // Zoegi Dark
  Theme {
    name: "Zoegi",
    bg: Color::Rgb(20, 20, 20),
    fg: Color::Rgb(204, 204, 204),
    accent: Color::Rgb(64, 128, 104),
    muted: Color::Rgb(89, 89, 89),
    border: Color::Rgb(48, 48, 48),
    error: Color::Rgb(204, 92, 92),
    status: Color::Rgb(86, 139, 153),
    highlight_bg: Color::Rgb(34, 34, 34),
    highlight_fg: Color::Rgb(128, 200, 160),
    stripe_bg: Color::Rgb(27, 27, 27),
    key_bg: Color::Rgb(64, 64, 64),
    key_fg: Color::Rgb(204, 204, 204),
  },
  // --- Light themes ---
  // Gruvbox Light
  Theme {
    name: "Gruvbox Light",
    bg: Color::Rgb(251, 241, 199),
    fg: Color::Rgb(60, 56, 54),
    accent: Color::Rgb(215, 153, 33),
    muted: Color::Rgb(146, 131, 116),
    border: Color::Rgb(213, 196, 161),
    error: Color::Rgb(204, 36, 29),
    status: Color::Rgb(121, 116, 14),
    highlight_bg: Color::Rgb(235, 219, 178),
    highlight_fg: Color::Rgb(60, 56, 54),
    stripe_bg: Color::Rgb(249, 236, 186),
    key_bg: Color::Rgb(213, 196, 161),
    key_fg: Color::Rgb(60, 56, 54),
  },
  // Solarized Light
  Theme {
    name: "Solarized Light",
    bg: Color::Rgb(253, 246, 227),
    fg: Color::Rgb(88, 110, 117),
    accent: Color::Rgb(42, 161, 152),
    muted: Color::Rgb(147, 161, 161),
    border: Color::Rgb(220, 212, 188),
    error: Color::Rgb(220, 50, 47),
    status: Color::Rgb(133, 153, 0),
    highlight_bg: Color::Rgb(238, 232, 213),
    highlight_fg: Color::Rgb(7, 54, 66),
    stripe_bg: Color::Rgb(245, 239, 218),
    key_bg: Color::Rgb(220, 212, 188),
    key_fg: Color::Rgb(88, 110, 117),
  },
  // Flexoki Light
  Theme {
    name: "Flexoki Light",
    bg: Color::Rgb(255, 252, 240),
    fg: Color::Rgb(16, 15, 15),
    accent: Color::Rgb(36, 131, 123),
    muted: Color::Rgb(111, 110, 105),
    border: Color::Rgb(230, 228, 217),
    error: Color::Rgb(209, 77, 65),
    status: Color::Rgb(102, 128, 11),
    highlight_bg: Color::Rgb(242, 240, 229),
    highlight_fg: Color::Rgb(16, 15, 15),
    stripe_bg: Color::Rgb(247, 245, 234),
    key_bg: Color::Rgb(230, 228, 217),
    key_fg: Color::Rgb(16, 15, 15),
  },
  // Ayu Light
  Theme {
    name: "Ayu Light",
    bg: Color::Rgb(252, 252, 252),
    fg: Color::Rgb(92, 97, 102),
    accent: Color::Rgb(255, 153, 64),
    muted: Color::Rgb(153, 160, 166),
    border: Color::Rgb(207, 209, 210),
    error: Color::Rgb(240, 113, 113),
    status: Color::Rgb(133, 179, 4),
    highlight_bg: Color::Rgb(230, 230, 230),
    highlight_fg: Color::Rgb(92, 97, 102),
    stripe_bg: Color::Rgb(243, 244, 245),
    key_bg: Color::Rgb(207, 209, 210),
    key_fg: Color::Rgb(92, 97, 102),
  },
  // Zoegi Light
  Theme {
    name: "Zoegi Light",
    bg: Color::Rgb(255, 255, 255),
    fg: Color::Rgb(51, 51, 51),
    accent: Color::Rgb(55, 121, 97),
    muted: Color::Rgb(89, 89, 89),
    border: Color::Rgb(230, 230, 230),
    error: Color::Rgb(204, 92, 92),
    status: Color::Rgb(55, 121, 97),
    highlight_bg: Color::Rgb(235, 235, 235),
    highlight_fg: Color::Rgb(51, 51, 51),
    stripe_bg: Color::Rgb(247, 247, 247),
    key_bg: Color::Rgb(230, 230, 230),
    key_fg: Color::Rgb(51, 51, 51),
  },
];

// --- Data ---

#[derive(Debug, Clone)]
struct VideoDetails {
  url: String,
  title: String,
  uploader: Option<String>,
  duration: Option<String>,
}

// --- App State ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMode {
  Input,
  Results,
}

struct App {
  input: String,
  cursor_position: usize,
  mode: AppMode,
  theme_index: usize,
  search_results: Vec<(String, String)>,
  list_state: ListState,
  player: MusicPlayer,
  last_error: Option<String>,
  status_message: Option<String>,
  should_quit: bool,
  search_rx: Option<oneshot::Receiver<Result<SearchResult>>>,
  load_rx: Option<oneshot::Receiver<Result<LoadResult>>>,
  /// Set during rendering so the main loop can draw the Kitty/Sixel image after ratatui's draw pass.
  graphics_thumb_area: Option<Rect>,
  /// Track last Kitty/Sixel image sent: (video_id, area) — skip re-send when unchanged.
  graphics_last_sent: Option<(String, Rect)>,
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
    }
  }

  fn theme(&self) -> &'static Theme {
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
            Ok((details, thumbnail)) => {
              let video_id = details.url.split('=').next_back().unwrap_or("").to_string();
              if let Err(e) = self.player.play(details).await {
                self.last_error = Some(format!("Playback error: {}", e));
                let _ = self.player.stop().await;
              } else if let Some(thumb) = thumbnail {
                // Set thumbnail AFTER play() — play() calls stop() which clears cached_thumbnail.
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
          let _ = tx.send(Ok((d, thumb)));
        }
        Err(e) => {
          let _ = tx.send(Err(e));
        }
      }
    });
    self.load_rx = Some(rx);
  }
}

// --- Music Player ---

struct MusicPlayer {
  http_client: Client,
  current_process: Option<TokioChild>,
  display_mode: DisplayMode,
  current_details: Option<VideoDetails>,
  cached_thumbnail: Option<(String, DynamicImage)>,
  mpv_monitor_handle: Option<JoinHandle<()>>,
  mpv_status_rx: Option<mpsc::Receiver<String>>,
  last_mpv_status: Arc<Mutex<Option<String>>>,
  ipc_socket_path: Option<String>,
  paused: bool,
}

impl MusicPlayer {
  fn new(display_mode: DisplayMode) -> Self {
    Self {
      http_client: Client::new(),
      current_process: None,
      display_mode,
      current_details: None,
      cached_thumbnail: None,
      mpv_monitor_handle: None,
      mpv_status_rx: None,
      last_mpv_status: Arc::new(Mutex::new(None)),
      ipc_socket_path: None,
      paused: false,
    }
  }

  fn is_playing(&self) -> bool {
    self.current_process.is_some()
  }

  fn check_mpv_status(&mut self) {
    if let Some(rx) = &mut self.mpv_status_rx {
      while let Ok(status) = rx.try_recv() {
        // safety: mutex is only locked briefly and we never panic while holding it
        let mut last_status = self.last_mpv_status.lock().expect("mpv status mutex poisoned");
        *last_status = Some(status);
      }
    }
  }

  fn get_last_mpv_status(&self) -> Option<String> {
    // safety: mutex is only locked briefly and we never panic while holding it
    self.last_mpv_status.lock().expect("mpv status mutex poisoned").clone()
  }

  async fn play(&mut self, details: VideoDetails) -> Result<()> {
    self.stop().await.context("Failed to stop previous playback")?;
    self.current_details = Some(details.clone());
    self.paused = false;

    let socket_path = format!("/tmp/yp-mpv-{}.sock", std::process::id());
    // Remove stale socket if it exists from a previous crash.
    let _ = std::fs::remove_file(&socket_path);

    let mut cmd = Command::new("mpv");
    cmd.args([
      "--no-video",
      "--term-status-msg=Time: ${time-pos/full} / ${duration/full} | Title: ${media-title} | ${pause} ${percent-pos}%",
      &format!("--input-ipc-server={}", socket_path),
      &details.url,
    ]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("mpv not found. Install it with: brew install mpv (macOS) or apt install mpv (Linux)")
      } else {
        anyhow!(e).context("Failed to spawn mpv process")
      }
    })?;

    let stdout = child.stdout.take().context("Failed to get mpv stdout")?;
    let (tx, rx) = mpsc::channel::<String>(10);
    self.mpv_status_rx = Some(rx);

    let monitor_handle = tokio::spawn(async move {
      let reader = TokioBufReader::new(stdout);
      let mut lines = reader.lines();
      while let Ok(Some(line)) = lines.next_line().await {
        if tx.send(line).await.is_err() {
          break;
        }
      }
    });

    self.current_process = Some(child);
    self.mpv_monitor_handle = Some(monitor_handle);
    self.ipc_socket_path = Some(socket_path);
    Ok(())
  }

  async fn toggle_pause(&mut self) -> Result<()> {
    let Some(ref socket_path) = self.ipc_socket_path else {
      return Ok(());
    };
    let stream = tokio::net::UnixStream::connect(socket_path).await.context("Failed to connect to mpv IPC socket")?;
    stream.writable().await.context("mpv IPC socket not writable")?;
    stream.try_write(b"{\"command\":[\"cycle\",\"pause\"]}\n").context("Failed to send pause command to mpv")?;
    self.paused = !self.paused;
    Ok(())
  }

  async fn stop(&mut self) -> Result<()> {
    if let Some(handle) = self.mpv_monitor_handle.take() {
      handle.abort();
      let _ = handle.await;
    }
    self.mpv_status_rx = None;
    // safety: mutex is only locked briefly and we never panic while holding it
    *self.last_mpv_status.lock().expect("mpv status mutex poisoned") = None;

    if let Some(mut child) = self.current_process.take() {
      child.kill().await.context("Failed to kill mpv process")?;
      let _ = child.wait().await;
    }

    self.current_details = None;
    self.cached_thumbnail = None;
    self.paused = false;

    if let Some(path) = self.ipc_socket_path.take() {
      let _ = std::fs::remove_file(&path);
    }
    Ok(())
  }
}

// --- YouTube / yt-dlp helpers ---

async fn search_youtube(query: &str) -> Result<Vec<(String, String)>> {
  let output = Command::new("yt-dlp")
    .args([
      "--get-title",
      "--get-id",
      "--default-search",
      "ytsearch5:",
      "--no-playlist",
      "--skip-download",
      "--ignore-errors",
      "--no-warnings",
      "--",
      query,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .await
    .map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("yt-dlp not found. Install it with: brew install yt-dlp (macOS) or pip install yt-dlp")
      } else {
        anyhow!(e).context("Failed to execute yt-dlp search command")
      }
    })?;

  if !output.status.success() {
    return Err(anyhow!("yt-dlp search failed: {}", String::from_utf8_lossy(&output.stderr)));
  }

  let stdout_str = String::from_utf8(output.stdout).context("yt-dlp output non-UTF8")?;
  let lines: Vec<&str> = stdout_str.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
  Ok(lines.chunks_exact(2).map(|c| (c[0].to_string(), c[1].to_string())).collect())
}

async fn get_video_info(video_id: &str) -> Result<VideoDetails> {
  let url = format!("https://youtube.com/watch?v={}", video_id);
  let output = Command::new("yt-dlp")
    .args([
      "--print",
      "%(title)s",
      "--print",
      "%(uploader)s",
      "--print",
      "%(duration_string)s",
      "--no-warnings",
      "--",
      &url,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .await
    .map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("yt-dlp not found. Install it with: brew install yt-dlp (macOS) or pip install yt-dlp")
      } else {
        anyhow!(e).context("Failed to execute yt-dlp to get video info")
      }
    })?;

  if output.status.success() {
    let info_str = String::from_utf8(output.stdout).context("Failed to parse yt-dlp info output as UTF-8")?;
    let mut lines = info_str.lines();
    let title = lines.next().map(|s| s.trim().to_string()).ok_or_else(|| anyhow!("Missing title in yt-dlp output"))?;
    let uploader = lines.next().map(|s| s.trim().to_string()).filter(|s| s != "NA");
    let duration = lines.next().map(|s| s.trim().to_string()).filter(|s| s != "NA");
    Ok(VideoDetails { url, title, uploader, duration })
  } else {
    Ok(VideoDetails { url, title: video_id.to_string(), uploader: None, duration: None })
  }
}

async fn fetch_thumbnail(client: &Client, video_id: &str) -> Result<DynamicImage> {
  let thumbnail_urls = [
    format!("https://img.youtube.com/vi/{}/maxresdefault.jpg", video_id),
    format!("https://img.youtube.com/vi/{}/sddefault.jpg", video_id),
    format!("https://img.youtube.com/vi/{}/hqdefault.jpg", video_id),
    format!("https://img.youtube.com/vi/{}/0.jpg", video_id),
  ];

  for url in &thumbnail_urls {
    if let Ok(response) = client.get(url).send().await
      && response.status().is_success()
    {
      let image_bytes = response.bytes().await.with_context(|| format!("Failed to read image bytes from {}", url))?;
      let image = image::load_from_memory(&image_bytes)
        .with_context(|| format!("Failed to decode image from memory (URL: {})", url))?;
      return Ok(image);
    }
  }
  Err(anyhow!("Failed to fetch any thumbnail for video ID: {}", video_id))
}

// --- Helpers ---

/// Convert a char index to a byte offset within the string.
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
  s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

/// Compute the display width of the first `n` chars (accounting for double-width CJK).
fn display_width(s: &str, n: usize) -> usize {
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

// --- Thumbnail Widget ---

struct ThumbnailWidget<'a> {
  image: &'a DynamicImage,
  display_mode: DisplayMode,
}

const ASCII_CHARS: [&str; 10] = [" ", ".", ":", "-", "=", "+", "*", "#", "%", "@"];

impl Widget for ThumbnailWidget<'_> {
  fn render(self, area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
      return;
    }
    match self.display_mode {
      DisplayMode::Direct => render_direct(self.image, area, buf),
      DisplayMode::Ascii => render_ascii(self.image, area, buf),
      // Kitty and Sixel: image is sent via escape sequences after the draw pass.
      DisplayMode::Kitty | DisplayMode::Sixel => {}
    }
  }
}

fn render_direct(image: &DynamicImage, area: Rect, buf: &mut Buffer) {
  // Preserve aspect ratio: resize to fit, then center within the area.
  let pixel_h = (area.height * 2) as u32;
  let resized = image.resize(area.width as u32, pixel_h, FilterType::Lanczos3).into_rgb8();
  let img_w = resized.width().min(area.width as u32);
  let img_h = resized.height();
  let cell_h = img_h.div_ceil(2); // pixel rows → cell rows (half-block)
  let offset_x = (area.width as u32).saturating_sub(img_w) / 2;
  let offset_y = (area.height as u32).saturating_sub(cell_h) / 2;

  for y in 0..cell_h.min(area.height as u32) {
    for x in 0..img_w {
      let upper = resized.get_pixel(x, y * 2);
      let lower_y = y * 2 + 1;
      let fg = Color::Rgb(upper[0], upper[1], upper[2]);
      let bg = if lower_y < img_h {
        let lower = resized.get_pixel(x, lower_y);
        Color::Rgb(lower[0], lower[1], lower[2])
      } else {
        Color::Reset
      };
      buf.set_string(
        area.x + offset_x as u16 + x as u16,
        area.y + offset_y as u16 + y as u16,
        "▀",
        Style::default().fg(fg).bg(bg),
      );
    }
  }
}

fn render_ascii(image: &DynamicImage, area: Rect, buf: &mut Buffer) {
  // Preserve aspect ratio: resize to fit, then center within the area.
  // ASCII cells are roughly square, so use area dimensions directly.
  let resized = image.resize(area.width as u32, area.height as u32, FilterType::Lanczos3).to_luma8();
  let img_w = resized.width().min(area.width as u32);
  let img_h = resized.height().min(area.height as u32);
  let offset_x = (area.width as u32).saturating_sub(img_w) / 2;
  let offset_y = (area.height as u32).saturating_sub(img_h) / 2;

  for y in 0..img_h {
    for x in 0..img_w {
      let pixel = resized.get_pixel(x, y)[0];
      let idx = ((pixel as f32 / 255.0) * (ASCII_CHARS.len() - 1) as f32).round() as usize;
      let idx = idx.min(ASCII_CHARS.len() - 1);
      buf.set_string(
        area.x + offset_x as u16 + x as u16,
        area.y + offset_y as u16 + y as u16,
        ASCII_CHARS[idx],
        Style::default(),
      );
    }
  }
}

// --- Kitty Graphics Protocol ---
//
// Sends an image to the terminal using the Kitty graphics protocol (OSC APC).
//
//   Transmit:  \x1B_G a=T,f=100,t=d,c=<cols>,r=<rows>,q=2,m=1;<base64 chunk>\x1B\\
//   Continue:  \x1B_G m=1;<base64 chunk>\x1B\\
//   Last:      \x1B_G m=0;<base64 chunk>\x1B\\
//   Delete:    \x1B_G a=d,d=a,q=2\x1B\\
//
// The image is encoded as PNG, base64'd, and sent in <=4096-byte chunks.
// `c` and `r` tell the terminal how many columns/rows to scale the image over.

const KITTY_CHUNK_SIZE: usize = 4096;

/// Delete all Kitty images currently displayed.
fn kitty_delete_all() -> Result<()> {
  let mut stdout = std::io::stdout();
  write!(stdout, "\x1B_Ga=d,d=a,q=2\x1B\\")?;
  stdout.flush().context("Failed to flush kitty delete")?;
  Ok(())
}

/// Render an image at `area` using the Kitty graphics protocol.
fn kitty_render_image(image: &DynamicImage, area: Rect) -> Result<()> {
  if area.is_empty() {
    return Ok(());
  }

  // Resize to fit the cell area. Each cell row is ~2 pixel rows for aspect ratio,
  // so use area.height * 2 for the pixel height, matching the direct renderer.
  let resized = image.resize(area.width as u32, (area.height * 2) as u32, FilterType::Lanczos3);

  let mut png_buf = Vec::new();
  resized
    .write_to(&mut Cursor::new(&mut png_buf), ImageFormat::Png)
    .context("Failed to encode thumbnail as PNG for kitty")?;

  let b64 = BASE64.encode(&png_buf);
  let chunks: Vec<&[u8]> = b64.as_bytes().chunks(KITTY_CHUNK_SIZE).collect();
  let last = chunks.len().saturating_sub(1);

  let mut stdout = std::io::stdout();

  // Position cursor at the target cell
  // Terminal rows/cols are 1-indexed
  write!(stdout, "\x1B[{};{}H", area.y + 1, area.x + 1)?;

  for (i, chunk) in chunks.iter().enumerate() {
    let data = std::str::from_utf8(chunk).context("base64 chunk was not valid UTF-8")?;
    let more = if i < last { 1 } else { 0 };

    if i == 0 {
      write!(stdout, "\x1B_Ga=T,f=100,t=d,c={},r={},q=2,m={};{}\x1B\\", area.width, area.height, more, data)?;
    } else {
      write!(stdout, "\x1B_Gm={};{}\x1B\\", more, data)?;
    }
  }

  stdout.flush().context("Failed to flush kitty image")?;
  Ok(())
}

// --- Sixel Graphics Protocol ---
//
// Sixel encodes images at pixel resolution directly in the terminal stream.
// Each sixel "row" represents 6 vertical pixels. Colors are defined via
// registers and then pixels are emitted as characters in the range 0x3F–0x7E.
//
// Sequence:
//   DCS q <data> ST
//   DCS = \x1BP,  ST = \x1B\\
//
// Color register:  #<n>;2;<r%>;<g%>;<b%>
// Sixel data char: offset 0x3F (63) + 6-bit bitmap of which of 6 rows are "on"
// $  = carriage return (rewind to start of current sixel row)
// -  = newline (advance to next sixel row)
//
// Color quantization uses NeuQuant (neural-network-based, via `color_quant` crate).

const SIXEL_MAX_COLORS: usize = 256;

/// Render an image at `area` using the Sixel graphics protocol.
fn sixel_render_image(image: &DynamicImage, area: Rect) -> Result<()> {
  if area.is_empty() {
    return Ok(());
  }

  // Estimate pixel dimensions from cell area.
  // Typical cell is ~8px wide, ~16px tall.
  let pixel_w = area.width as u32 * 8;
  let pixel_h = area.height as u32 * 16;
  let resized = image.resize(pixel_w, pixel_h, FilterType::Lanczos3).into_rgb8();
  let (w, h) = (resized.width() as usize, resized.height() as usize);

  // Quantize colors using NeuQuant (sample_factor=1 = best quality, 10 = fast)
  let rgba_pixels: Vec<u8> = resized.pixels().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
  let nq = NeuQuant::new(3, SIXEL_MAX_COLORS, &rgba_pixels);
  let palette: Vec<[u8; 3]> =
    (0..SIXEL_MAX_COLORS).map(|i| nq.color_map_rgb()[i * 3..i * 3 + 3].try_into().unwrap_or([0, 0, 0])).collect();

  // Map each pixel to nearest palette index
  let indices: Vec<u8> = resized.pixels().map(|p| nq.index_of(&[p[0], p[1], p[2], 255]) as u8).collect();

  // --- Build sixel stream ---
  let mut out = String::with_capacity(w * h);

  // DCS q - enter sixel mode
  out.push_str("\x1BPq");

  // Set raster attributes: "aspect;aspect;width;height
  out.push_str(&format!("\"1;1;{};{}", w, h));

  // Define color registers
  for (i, c) in palette.iter().enumerate() {
    let r_pct = (c[0] as u32 * 100) / 255;
    let g_pct = (c[1] as u32 * 100) / 255;
    let b_pct = (c[2] as u32 * 100) / 255;
    out.push_str(&format!("#{};2;{};{};{}", i, r_pct, g_pct, b_pct));
  }

  // Emit sixel rows (6 pixel-rows each)
  let sixel_rows = h.div_ceil(6);
  for sr in 0..sixel_rows {
    let y_base = sr * 6;

    for (color_idx, _) in palette.iter().enumerate() {
      let color_idx_u8 = color_idx as u8;
      let mut has_pixels = false;
      let mut row_data = Vec::with_capacity(w);

      for x in 0..w {
        let mut sixel_val: u8 = 0;
        for bit in 0..6 {
          let y = y_base + bit;
          if y < h && indices[y * w + x] == color_idx_u8 {
            sixel_val |= 1 << bit;
            has_pixels = true;
          }
        }
        row_data.push(sixel_val);
      }

      if !has_pixels {
        continue;
      }

      out.push_str(&format!("#{}", color_idx));

      // RLE-encode the sixel data
      let mut i = 0;
      while i < row_data.len() {
        let val = row_data[i];
        let ch = (val + 0x3F) as char;
        let mut run = 1usize;
        while i + run < row_data.len() && row_data[i + run] == val {
          run += 1;
        }
        if run > 3 {
          out.push_str(&format!("!{}{}", run, ch));
        } else {
          for _ in 0..run {
            out.push(ch);
          }
        }
        i += run;
      }
      out.push('$'); // carriage return within sixel row
    }
    out.push('-'); // newline to next sixel row
  }

  // ST - leave sixel mode
  out.push_str("\x1B\\");

  // Position cursor at target cell, then emit the sixel data
  let mut stdout = std::io::stdout();
  write!(stdout, "\x1B[{};{}H{}", area.y + 1, area.x + 1, out)?;
  stdout.flush().context("Failed to flush sixel image")?;
  Ok(())
}

// --- UI Rendering ---

fn ui(frame: &mut Frame, app: &mut App) {
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
  let [thumb_area, info_area] =
    Layout::horizontal([Constraint::Percentage(75), Constraint::Percentage(25)]).areas(area);

  // Add 1-line vertical padding to the thumbnail area for breathing room.
  let thumb_area = Rect { y: thumb_area.y + 1, height: thumb_area.height.saturating_sub(2), ..thumb_area };

  if let Some((_, ref image)) = app.player.cached_thumbnail {
    let widget = ThumbnailWidget { image, display_mode: app.player.display_mode };
    frame.render_widget(widget, thumb_area);

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
    .border_style(Style::default().fg(theme.border));

  if let Some(details) = &app.player.current_details {
    // Available width inside the bordered block (1-cell border on each side).
    let inner_w = info_area.width.saturating_sub(2) as usize;

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
  let items: Vec<ListItem> = app
    .search_results
    .iter()
    .enumerate()
    .map(|(i, (title, _))| {
      let is_selected = Some(i) == app.list_state.selected();
      let fg = if is_selected { theme.highlight_fg } else { theme.fg };
      let bg = if is_selected {
        theme.highlight_bg
      } else if i % 2 == 1 {
        theme.stripe_bg
      } else {
        theme.bg
      };
      ListItem::new(Line::from(Span::styled(title, Style::default().fg(fg)))).bg(bg)
    })
    .collect();

  let list = List::new(items)
    .block(
      Block::bordered()
        .title(" Results ")
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

fn render_input(frame: &mut Frame, app: &App, area: Rect) {
  let theme = app.theme();
  let border_color = if app.mode == AppMode::Input { theme.accent } else { theme.border };
  let input_block = Block::bordered()
    .title(" Search YouTube ")
    .title_style(Style::default().fg(border_color))
    .border_type(ratatui::widgets::BorderType::Rounded)
    .border_style(Style::default().fg(border_color))
    .padding(Padding::horizontal(1));

  let paragraph = Paragraph::new(app.input.as_str()).style(Style::default().fg(theme.fg)).block(input_block);
  frame.render_widget(paragraph, area);

  if app.mode == AppMode::Input {
    frame.set_cursor_position((area.x + display_width(&app.input, app.cursor_position) as u16 + 2, area.y + 1));
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
      let mut k = vec![("Enter", "Play"), ("↑↓", "Navigate")];
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

// --- Event Handling ---

async fn handle_key_event(app: &mut App, key: event::KeyEvent) -> Result<()> {
  // Ctrl+C always quits
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
    app.should_quit = true;
    return Ok(());
  }

  // Theme cycling — Ctrl+t works in all modes
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
    app.next_theme();
    return Ok(());
  }

  // Stop playback — Ctrl+s works in all modes when playing
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
    if app.player.is_playing() {
      app.player.stop().await.context("Failed to stop playback")?;
      app.graphics_last_sent = None;
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
  let mut terminal = ratatui::init();
  let result = run(&mut terminal, args).await;
  ratatui::restore();
  result
}

async fn run(terminal: &mut DefaultTerminal, args: Args) -> Result<()> {
  let display_mode = resolve_display_mode(args.display_mode);
  let mut app = App::new(display_mode);
  let uses_graphics_protocol = matches!(display_mode, DisplayMode::Kitty | DisplayMode::Sixel);

  loop {
    app.check_pending().await?;
    app.player.check_mpv_status();

    terminal.draw(|frame| ui(frame, &mut app))?;

    // Kitty/Sixel graphics: send image after ratatui's draw pass so it isn't overwritten.
    // Only re-send when the image (video_id) or target area changes.
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
        // No thumbnail visible this frame — clear any lingering images.
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
