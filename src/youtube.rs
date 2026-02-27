use anyhow::{Context, Result, anyhow};
use image::DynamicImage;
use reqwest::Client;
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use tokio::process::{Child as TokioChild, Command};
use tokio::sync::mpsc;

use crate::player::VideoDetails;

// --- Shared Helpers ---

/// Parse an optional yt-dlp field value: trim, filter empty/"NA" sentinel.
fn opt_field(s: Option<&str>) -> Option<String> {
  s.map(str::trim).filter(|s| !s.is_empty() && *s != "NA").map(|s| s.to_string())
}

/// Spawn a yt-dlp command with the given arguments and wait for it to finish.
/// Provides a consistent "yt-dlp not found" error message across all call sites.
async fn run_yt_dlp(args: &[&str], context: &str) -> Result<Output> {
  Command::new("yt-dlp")
    .args(args)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .await
    .map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("yt-dlp not found. Install it with: brew install yt-dlp (macOS) or pip install yt-dlp")
      } else {
        anyhow!(e).context(format!("Failed to execute yt-dlp for {}", context))
      }
    })
}

// --- Frame Sources ---

/// Frame rate for ffmpeg video frame extraction (frames per second).
const FRAME_EXTRACT_FPS: f64 = 0.5;

/// Width of extracted video frames (height is auto-scaled to preserve aspect ratio).
const FRAME_EXTRACT_WIDTH: u32 = 640;

/// A decoded storyboard: sprite sheets + metadata for frame extraction.
pub struct SpriteFrameSource {
  /// The video ID this source belongs to.
  pub video_id: String,
  /// Decoded sprite sheet images (one per fragment).
  sheets: Vec<DynamicImage>,
  /// Width of a single frame within the sprite sheet.
  frame_width: u32,
  /// Height of a single frame within the sprite sheet.
  frame_height: u32,
  /// Number of rows in each sprite sheet grid.
  rows: u32,
  /// Number of columns in each sprite sheet grid.
  cols: u32,
  /// Storyboard frame rate (frames per second, typically ~0.5).
  fps: f64,
  /// Duration each fragment covers (in seconds).
  fragment_durations: Vec<f64>,
}

impl SpriteFrameSource {
  /// Number of frames in each sprite sheet.
  fn frames_per_sheet(&self) -> u32 {
    self.rows * self.cols
  }

  /// Duration of a single frame (in seconds).
  fn frame_interval(&self) -> f64 {
    if self.fps > 0.0 { 1.0 / self.fps } else { 2.0 }
  }

  /// Compute a global frame index for the given playback time.
  pub fn frame_index_at(&self, time_secs: f64) -> usize {
    let fps = self.frames_per_sheet() as usize;
    if self.sheets.is_empty() || fps == 0 || time_secs < 0.0 {
      return 0;
    }
    let interval = self.frame_interval();
    let mut elapsed = 0.0;
    let mut global_offset = 0usize;

    for (i, &dur) in self.fragment_durations.iter().enumerate() {
      if time_secs < elapsed + dur || i == self.sheets.len() - 1 {
        let time_in_frag = (time_secs - elapsed).max(0.0);
        let local_idx = (time_in_frag / interval) as usize;
        return global_offset + local_idx.min(fps - 1);
      }
      elapsed += dur;
      global_offset += fps;
    }
    0
  }

  /// Extract the frame image at the given playback time.
  pub fn frame_at(&self, time_secs: f64) -> Option<DynamicImage> {
    let fps = self.frames_per_sheet();
    if self.sheets.is_empty() || fps == 0 || time_secs < 0.0 {
      return None;
    }
    let interval = self.frame_interval();
    let mut elapsed = 0.0;

    for (i, sheet) in self.sheets.iter().enumerate() {
      let frag_dur = self.fragment_durations.get(i).copied().unwrap_or(0.0);
      if time_secs < elapsed + frag_dur || i == self.sheets.len() - 1 {
        let time_in_frag = (time_secs - elapsed).max(0.0);
        let local_idx = (time_in_frag / interval) as u32;
        let local_idx = local_idx.min(fps - 1);
        let col = local_idx % self.cols;
        let row = local_idx / self.cols;
        let x = col * self.frame_width;
        let y = row * self.frame_height;
        return Some(sheet.crop_imm(x, y, self.frame_width, self.frame_height));
      }
      elapsed += frag_dur;
    }
    None
  }
}

/// Video frame source: ffmpeg extracts frames progressively to a temp directory.
/// Frames become available on disk as ffmpeg processes the stream.
pub struct VideoFrameSource {
  /// The video ID this source belongs to.
  pub video_id: String,
  /// Directory containing extracted frame JPEGs (frame_0001.jpg, frame_0002.jpg, ...).
  frames_dir: PathBuf,
  /// Interval between frames in seconds (1.0 / FRAME_EXTRACT_FPS).
  frame_interval: f64,
  /// Handle to the running ffmpeg process (killed on drop).
  ffmpeg_handle: Option<TokioChild>,
}

impl VideoFrameSource {
  /// Compute the 1-indexed frame number for the given playback time.
  pub fn frame_index_at(&self, time_secs: f64) -> usize {
    if time_secs < 0.0 {
      return 1;
    }
    (time_secs / self.frame_interval) as usize + 1
  }

  /// Load the frame image at the given playback time from disk.
  /// Returns `None` if the frame hasn't been extracted yet.
  pub fn frame_at(&self, time_secs: f64) -> Option<DynamicImage> {
    let idx = self.frame_index_at(time_secs);
    let path = self.frames_dir.join(format!("frame_{:04}.jpg", idx));
    image::open(&path).ok()
  }
}

impl Drop for VideoFrameSource {
  fn drop(&mut self) {
    if let Some(ref mut child) = self.ffmpeg_handle {
      // start_kill is synchronous — safe to call in Drop
      let _ = child.start_kill();
    }
    let _ = std::fs::remove_dir_all(&self.frames_dir);
  }
}

/// Unified frame source enum — delegates to either sprite sheets or video frames.
pub enum FrameSource {
  Sprites(SpriteFrameSource),
  Video(VideoFrameSource),
}

impl FrameSource {
  pub fn video_id(&self) -> &str {
    match self {
      FrameSource::Sprites(s) => &s.video_id,
      FrameSource::Video(v) => &v.video_id,
    }
  }

  pub fn frame_index_at(&self, time_secs: f64) -> usize {
    match self {
      FrameSource::Sprites(s) => s.frame_index_at(time_secs),
      FrameSource::Video(v) => v.frame_index_at(time_secs),
    }
  }

  pub fn frame_at(&self, time_secs: f64) -> Option<DynamicImage> {
    match self {
      FrameSource::Sprites(s) => s.frame_at(time_secs),
      FrameSource::Video(v) => v.frame_at(time_secs),
    }
  }
}

/// Intermediate: parsed storyboard metadata from yt-dlp JSON.
struct StoryboardMeta {
  frame_width: u32,
  frame_height: u32,
  rows: u32,
  cols: u32,
  fps: f64,
  fragments: Vec<StoryboardFragmentMeta>,
}

struct StoryboardFragmentMeta {
  url: String,
  duration: f64,
}

/// Parse storyboard metadata from yt-dlp --dump-json output.
/// Picks the highest-resolution storyboard format available.
fn parse_storyboard_meta(json: &Value) -> Result<StoryboardMeta> {
  let formats = json.get("formats").and_then(Value::as_array).context("No formats array in yt-dlp JSON")?;

  let sb_format = formats
    .iter()
    .filter(|f| f.get("format_note").and_then(Value::as_str) == Some("storyboard"))
    .max_by_key(|f| {
      let w = f.get("width").and_then(Value::as_u64).unwrap_or(0);
      let h = f.get("height").and_then(Value::as_u64).unwrap_or(0);
      w * h
    })
    .context("No storyboard format found")?;

  let frame_width = sb_format.get("width").and_then(Value::as_u64).context("Missing storyboard width")? as u32;
  let frame_height = sb_format.get("height").and_then(Value::as_u64).context("Missing storyboard height")? as u32;
  let rows = sb_format.get("rows").and_then(Value::as_u64).context("Missing storyboard rows")? as u32;
  let cols = sb_format.get("columns").and_then(Value::as_u64).context("Missing storyboard columns")? as u32;
  let fps = sb_format.get("fps").and_then(Value::as_f64).context("Missing storyboard fps")?;

  let fragments = sb_format
    .get("fragments")
    .and_then(Value::as_array)
    .context("Missing storyboard fragments")?
    .iter()
    .filter_map(|f| {
      let url = f.get("url").and_then(Value::as_str)?.to_string();
      let duration = f.get("duration").and_then(Value::as_f64)?;
      Some(StoryboardFragmentMeta { url, duration })
    })
    .collect::<Vec<_>>();

  if fragments.is_empty() {
    return Err(anyhow!("Storyboard has no fragments"));
  }

  Ok(StoryboardMeta { frame_width, frame_height, rows, cols, fps, fragments })
}

/// Fetch storyboard sprite sheets for a video.
///
/// Runs `yt-dlp --dump-json` to get storyboard metadata, then downloads
/// all sprite sheet images. This is a progressive enhancement — if it fails,
/// the static thumbnail continues to work.
pub async fn fetch_sprite_frames(client: &Client, video_id: &str) -> Result<FrameSource> {
  let url = format!("https://youtube.com/watch?v={}", video_id);
  let output = run_yt_dlp(&["--dump-json", "--no-warnings", "--", &url], "storyboard info").await?;

  if !output.status.success() {
    return Err(anyhow!("yt-dlp --dump-json failed for storyboard"));
  }

  let json: Value = serde_json::from_slice(&output.stdout).context("Failed to parse yt-dlp JSON for storyboard")?;
  let meta = parse_storyboard_meta(&json)?;

  // Download fragments sequentially to preserve order (they're small images)
  let mut sheets = Vec::with_capacity(meta.fragments.len());
  let mut durations = Vec::with_capacity(meta.fragments.len());

  for frag in &meta.fragments {
    let response = client.get(&frag.url).send().await.with_context(|| {
      let truncated: String = frag.url.chars().take(60).collect();
      format!("Failed to fetch storyboard sheet: {}", truncated)
    })?;
    let bytes = response.bytes().await.context("Failed to read storyboard sheet bytes")?;
    let image = image::load_from_memory(&bytes).context("Failed to decode storyboard sprite sheet")?;
    sheets.push(image);
    durations.push(frag.duration);
  }

  Ok(FrameSource::Sprites(SpriteFrameSource {
    video_id: video_id.to_string(),
    sheets,
    frame_width: meta.frame_width,
    frame_height: meta.frame_height,
    rows: meta.rows,
    cols: meta.cols,
    fps: meta.fps,
    fragment_durations: durations,
  }))
}

/// Fetch video frames via ffmpeg extraction.
///
/// Runs `yt-dlp --get-url` to get the video stream URL, then spawns `ffmpeg`
/// to progressively extract frames to a temp directory. Returns immediately —
/// frames appear on disk as ffmpeg processes the stream.
pub async fn fetch_video_frames(video_id: &str) -> Result<FrameSource> {
  let yt_url = format!("https://youtube.com/watch?v={}", video_id);
  let output = run_yt_dlp(
    &["--get-url", "-f", "bestvideo[height<=480]/bestvideo", "--no-warnings", "--", &yt_url],
    "video frames",
  )
  .await?;

  if !output.status.success() {
    return Err(anyhow!("yt-dlp --get-url failed for video frame extraction"));
  }

  let stream_url = String::from_utf8(output.stdout).context("yt-dlp --get-url output not UTF-8")?.trim().to_string();
  if stream_url.is_empty() {
    return Err(anyhow!("yt-dlp returned empty stream URL"));
  }

  let frames_dir = std::env::temp_dir().join(format!("yp-frames-{}-{}", std::process::id(), video_id));
  std::fs::create_dir_all(&frames_dir).context("Failed to create temp dir for video frames")?;

  let output_pattern = frames_dir.join("frame_%04d.jpg");
  let vf_arg = format!("fps={},scale={}:-2", FRAME_EXTRACT_FPS, FRAME_EXTRACT_WIDTH);

  let child = Command::new("ffmpeg")
    .args([
      "-nostdin",
      "-i",
      &stream_url,
      "-vf",
      &vf_arg,
      "-q:v",
      "2",
      "-y",
      output_pattern.to_str().context("Invalid temp dir path")?,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("ffmpeg not found. Install it with: brew install ffmpeg (macOS)")
      } else {
        anyhow!(e).context("Failed to spawn ffmpeg for video frame extraction")
      }
    })?;

  let frame_interval = 1.0 / FRAME_EXTRACT_FPS;

  Ok(FrameSource::Video(VideoFrameSource {
    video_id: video_id.to_string(),
    frames_dir,
    frame_interval,
    ffmpeg_handle: Some(child),
  }))
}

/// Number of videos to fetch on the initial channel load.
pub const CHANNEL_INITIAL_SIZE: usize = 30;

/// Page size for subsequent channel "load more" fetches.
pub const CHANNEL_PAGE_SIZE: usize = 20;

/// A single entry from a search or channel listing.
#[derive(Debug, Clone)]
pub struct SearchEntry {
  pub title: String,
  pub video_id: String,
  pub upload_date: Option<String>,
  pub tags: Option<String>,
  /// Whether this entry has been enriched with full metadata (date, tags).
  /// Entries from `--flat-playlist` start as `false`.
  pub enriched: bool,
}

/// Clean yt-dlp's Python-list repr of tags into a comma-separated string.
/// e.g. `['rock', 'music', 'guitar']` → `rock, music, guitar`
pub(crate) fn clean_tags(raw: &str) -> String {
  raw.trim_start_matches('[').trim_end_matches(']').replace('\'', "")
}

/// Parse a single tab-separated yt-dlp output line into a SearchEntry.
/// Expected format: `title\tid[\tupload_date\ttags]`
pub(crate) fn parse_search_line(line: &str) -> Option<SearchEntry> {
  let parts: Vec<&str> = line.split('\t').collect();
  if parts.len() < 2 {
    return None;
  }
  let title = parts[0].trim().to_string();
  let video_id = parts[1].trim().to_string();
  if video_id.is_empty() {
    return None;
  }
  let upload_date = opt_field(parts.get(2).copied());
  let tags = opt_field(parts.get(3).copied()).map(|s| clean_tags(&s)).filter(|s| !s.is_empty());
  let enriched = upload_date.is_some() || tags.is_some();
  Some(SearchEntry { title, video_id, upload_date, tags, enriched })
}

/// Parse yt-dlp stdout lines into SearchEntry vec.
fn parse_search_output(stdout: &str) -> Vec<SearchEntry> {
  stdout.lines().map(str::trim).filter(|l| !l.is_empty()).filter_map(parse_search_line).collect()
}

/// Detect whether user input refers to a YouTube channel.
/// Returns the canonical channel URL if detected, or None for a regular search.
pub fn detect_channel_url(input: &str) -> Option<String> {
  let trimmed = input.trim();

  // "/channel @handle" or "/channel https://..."
  let after_prefix = trimmed.strip_prefix("/channel").map(str::trim_start);

  let candidate = after_prefix.unwrap_or(trimmed);

  // Bare @handle (e.g. "@TwoSetViolin")
  if candidate.starts_with('@') && !candidate.contains(' ') && candidate.len() > 1 {
    return Some(format!("https://www.youtube.com/{}/videos", candidate));
  }

  // Full YouTube channel URL
  if (candidate.contains("youtube.com/@") || candidate.contains("youtube.com/channel/"))
    && (candidate.starts_with("http://") || candidate.starts_with("https://"))
  {
    let url = candidate.trim_end_matches('/');
    // Append /videos if not already present, so yt-dlp lists the uploads
    if url.ends_with("/videos") {
      return Some(url.to_string());
    }
    return Some(format!("{}/videos", url));
  }

  // Only trigger for the /channel prefix form, not bare text
  if after_prefix.is_some() && !candidate.is_empty() {
    // Assume it's a channel name/handle without @
    return Some(format!("https://www.youtube.com/@{}/videos", candidate));
  }

  None
}

/// The yt-dlp print template used for all listing commands.
const PRINT_FORMAT: &str = "%(title)s\t%(id)s\t%(upload_date>%Y-%m-%d)s\t%(tags)s";

/// The yt-dlp print template used for per-video metadata enrichment.
const ENRICH_FORMAT: &str = "%(id)s\t%(upload_date>%Y-%m-%d)s\t%(tags)s";

/// Maximum number of concurrent yt-dlp enrichment processes.
const ENRICH_CONCURRENCY: usize = 5;

/// Enriched metadata for a single video (returned from background enrichment).
#[derive(Debug, Clone)]
pub struct VideoMeta {
  pub video_id: String,
  pub upload_date: Option<String>,
  pub tags: Option<String>,
}

/// Enrich a list of video IDs with full metadata (upload_date, tags).
/// Spawns up to `ENRICH_CONCURRENCY` concurrent yt-dlp processes.
/// Each result is sent progressively through `tx` as it becomes available.
pub async fn enrich_video_metadata(video_ids: Vec<String>, tx: mpsc::Sender<VideoMeta>) {
  use futures::stream::{self, StreamExt};

  stream::iter(video_ids)
    .map(|video_id| {
      let tx = tx.clone();
      async move {
        let url = format!("https://youtube.com/watch?v={}", video_id);
        let result =
          run_yt_dlp(&["--skip-download", "--print", ENRICH_FORMAT, "--no-warnings", "--", &url], "enrichment").await;

        if let Ok(output) = result
          && output.status.success()
          && let Ok(stdout) = String::from_utf8(output.stdout)
        {
          let line = stdout.trim();
          let parts: Vec<&str> = line.split('\t').collect();
          if !parts.is_empty() && !parts[0].is_empty() {
            let upload_date = opt_field(parts.get(1).copied());
            let tags = opt_field(parts.get(2).copied()).map(|s| clean_tags(&s)).filter(|s| !s.is_empty());
            let _ = tx.send(VideoMeta { video_id: parts[0].trim().to_string(), upload_date, tags }).await;
          }
        }
      }
    })
    .buffer_unordered(ENRICH_CONCURRENCY)
    .collect::<()>()
    .await;
}

/// Fetch a batch of videos from a channel URL using --flat-playlist for speed.
/// Results will have titles and IDs but no date/tags (those come from enrichment).
/// `start` is 1-indexed, `count` is how many to fetch.
pub async fn list_channel_videos(channel_url: &str, start: usize, count: usize) -> Result<Vec<SearchEntry>> {
  if count == 0 {
    return Ok(Vec::new());
  }
  let end = start + count - 1;
  let playlist_range = format!("{}:{}", start, end);

  let output = run_yt_dlp(
    &[
      "--flat-playlist",
      "--print",
      "%(title)s\t%(id)s",
      "--playlist-items",
      &playlist_range,
      "--no-warnings",
      "--ignore-errors",
      "--",
      channel_url,
    ],
    "channel listing",
  )
  .await?;

  if !output.status.success() {
    return Err(anyhow!("yt-dlp channel listing failed: {}", String::from_utf8_lossy(&output.stderr)));
  }

  let stdout_str = String::from_utf8(output.stdout).context("yt-dlp output non-UTF8")?;
  // flat-playlist only gives title + id, so entries will have enriched=false
  Ok(parse_search_output(&stdout_str))
}

pub async fn search_youtube(query: &str) -> Result<Vec<SearchEntry>> {
  let output = run_yt_dlp(
    &[
      "--print",
      PRINT_FORMAT,
      "--default-search",
      "ytsearch20:",
      "--no-playlist",
      "--skip-download",
      "--ignore-errors",
      "--no-warnings",
      "--",
      query,
    ],
    "search",
  )
  .await?;

  if !output.status.success() {
    return Err(anyhow!("yt-dlp search failed: {}", String::from_utf8_lossy(&output.stderr)));
  }

  let stdout_str = String::from_utf8(output.stdout).context("yt-dlp output non-UTF8")?;
  Ok(parse_search_output(&stdout_str))
}

pub async fn get_video_info(video_id: &str) -> Result<VideoDetails> {
  let url = format!("https://youtube.com/watch?v={}", video_id);
  let output = run_yt_dlp(
    &[
      "--print",
      "%(title)s",
      "--print",
      "%(uploader)s",
      "--print",
      "%(duration_string)s",
      "--print",
      "%(upload_date>%Y-%m-%d)s",
      "--print",
      "%(tags)s",
      "--no-warnings",
      "--",
      &url,
    ],
    "video info",
  )
  .await?;

  if output.status.success() {
    let info_str = String::from_utf8(output.stdout).context("Failed to parse yt-dlp info output as UTF-8")?;
    let mut lines = info_str.lines();
    let title = lines.next().map(|s| s.trim().to_string()).ok_or_else(|| anyhow!("Missing title in yt-dlp output"))?;
    let uploader = opt_field(lines.next());
    let duration = opt_field(lines.next());
    let upload_date = opt_field(lines.next());
    let tags = opt_field(lines.next())
      .map(|s| clean_tags(&s))
      .filter(|s| !s.is_empty())
      .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
      .unwrap_or_default();
    Ok(VideoDetails { url, title, uploader, duration, upload_date, tags })
  } else {
    Err(anyhow!("yt-dlp failed to get video info: {}", String::from_utf8_lossy(&output.stderr).trim()))
  }
}

pub async fn fetch_thumbnail(client: &Client, video_id: &str) -> Result<DynamicImage> {
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

#[cfg(test)]
mod tests {
  use super::*;

  // --- clean_tags ---

  #[test]
  fn clean_tags_python_list() {
    assert_eq!(clean_tags("['rock', 'music', 'guitar']"), "rock, music, guitar");
  }

  #[test]
  fn clean_tags_empty_list() {
    assert_eq!(clean_tags("[]"), "");
  }

  #[test]
  fn clean_tags_single_item() {
    assert_eq!(clean_tags("['jazz']"), "jazz");
  }

  #[test]
  fn clean_tags_no_brackets() {
    assert_eq!(clean_tags("rock, pop"), "rock, pop");
  }

  // --- parse_search_line ---

  #[test]
  fn parse_search_line_basic() {
    let entry = parse_search_line("My Song\tabc123").unwrap();
    assert_eq!(entry.title, "My Song");
    assert_eq!(entry.video_id, "abc123");
    assert_eq!(entry.upload_date, None);
    assert_eq!(entry.tags, None);
    assert!(!entry.enriched);
  }

  #[test]
  fn parse_search_line_with_date_and_tags() {
    let entry = parse_search_line("Title\tvid1\t2024-01-15\t['rock', 'pop']").unwrap();
    assert_eq!(entry.title, "Title");
    assert_eq!(entry.video_id, "vid1");
    assert_eq!(entry.upload_date, Some("2024-01-15".to_string()));
    assert_eq!(entry.tags, Some("rock, pop".to_string()));
    assert!(entry.enriched);
  }

  #[test]
  fn parse_search_line_with_na_fields() {
    let entry = parse_search_line("Title\tvid2\tNA\tNA").unwrap();
    assert_eq!(entry.upload_date, None);
    assert_eq!(entry.tags, None);
    assert!(!entry.enriched);
  }

  #[test]
  fn parse_search_line_empty() {
    assert!(parse_search_line("").is_none());
  }

  #[test]
  fn parse_search_line_no_id() {
    assert!(parse_search_line("Title\t").is_none());
  }

  #[test]
  fn parse_search_line_single_field() {
    assert!(parse_search_line("JustATitle").is_none());
  }

  // --- detect_channel_url ---

  #[test]
  fn detect_channel_bare_handle() {
    assert_eq!(detect_channel_url("@TwoSetViolin"), Some("https://www.youtube.com/@TwoSetViolin/videos".to_string()));
  }

  #[test]
  fn detect_channel_handle_with_spaces_is_none() {
    assert_eq!(detect_channel_url("@Two Set"), None);
  }

  #[test]
  fn detect_channel_url_full() {
    assert_eq!(
      detect_channel_url("https://www.youtube.com/@TwoSetViolin"),
      Some("https://www.youtube.com/@TwoSetViolin/videos".to_string())
    );
  }

  #[test]
  fn detect_channel_url_already_has_videos() {
    assert_eq!(
      detect_channel_url("https://www.youtube.com/@TwoSetViolin/videos"),
      Some("https://www.youtube.com/@TwoSetViolin/videos".to_string())
    );
  }

  #[test]
  fn detect_channel_url_channel_id() {
    assert_eq!(
      detect_channel_url("https://www.youtube.com/channel/UC123abc"),
      Some("https://www.youtube.com/channel/UC123abc/videos".to_string())
    );
  }

  #[test]
  fn detect_channel_prefix_command() {
    assert_eq!(
      detect_channel_url("/channel TwoSetViolin"),
      Some("https://www.youtube.com/@TwoSetViolin/videos".to_string())
    );
  }

  #[test]
  fn detect_channel_prefix_with_handle() {
    assert_eq!(
      detect_channel_url("/channel @TwoSetViolin"),
      Some("https://www.youtube.com/@TwoSetViolin/videos".to_string())
    );
  }

  #[test]
  fn detect_channel_regular_search() {
    assert_eq!(detect_channel_url("beethoven moonlight sonata"), None);
  }

  #[test]
  fn detect_channel_bare_at_sign() {
    // Single @ with nothing else
    assert_eq!(detect_channel_url("@"), None);
  }
}
