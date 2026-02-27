use anyhow::{Context, Result, anyhow};
use image::DynamicImage;
use reqwest::Client;
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::player::VideoDetails;

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
fn clean_tags(raw: &str) -> String {
  raw.trim_start_matches('[').trim_end_matches(']').replace('\'', "")
}

/// Parse a single tab-separated yt-dlp output line into a SearchEntry.
/// Expected format: `title\tid[\tupload_date\ttags]`
fn parse_search_line(line: &str) -> Option<SearchEntry> {
  let parts: Vec<&str> = line.split('\t').collect();
  if parts.len() < 2 {
    return None;
  }
  let title = parts[0].trim().to_string();
  let video_id = parts[1].trim().to_string();
  if video_id.is_empty() {
    return None;
  }
  let opt = |idx: usize| -> Option<String> {
    parts.get(idx).map(|s| s.trim()).filter(|s| !s.is_empty() && *s != "NA").map(|s| s.to_string())
  };
  let upload_date = opt(2);
  let tags = opt(3).map(|s| clean_tags(&s)).filter(|s| !s.is_empty());
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
        let result = Command::new("yt-dlp")
          .args(["--skip-download", "--print", ENRICH_FORMAT, "--no-warnings", "--", &url])
          .stdin(Stdio::null())
          .stdout(Stdio::piped())
          .stderr(Stdio::null())
          .output()
          .await;

        if let Ok(output) = result
          && output.status.success()
          && let Ok(stdout) = String::from_utf8(output.stdout)
        {
          let line = stdout.trim();
          let parts: Vec<&str> = line.split('\t').collect();
          if !parts.is_empty() && !parts[0].is_empty() {
            let opt = |idx: usize| -> Option<String> {
              parts.get(idx).map(|s| s.trim()).filter(|s| !s.is_empty() && *s != "NA").map(|s| s.to_string())
            };
            let upload_date = opt(1);
            let tags = opt(2).map(|s| clean_tags(&s)).filter(|s| !s.is_empty());
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
  let end = start + count - 1;
  let playlist_range = format!("{}:{}", start, end);

  let output = Command::new("yt-dlp")
    .args([
      "--flat-playlist",
      "--print",
      "%(title)s\t%(id)s",
      "--playlist-items",
      &playlist_range,
      "--no-warnings",
      "--ignore-errors",
      "--",
      channel_url,
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
        anyhow!(e).context("Failed to execute yt-dlp channel listing")
      }
    })?;

  if !output.status.success() {
    return Err(anyhow!("yt-dlp channel listing failed: {}", String::from_utf8_lossy(&output.stderr)));
  }

  let stdout_str = String::from_utf8(output.stdout).context("yt-dlp output non-UTF8")?;
  // flat-playlist only gives title + id, so entries will have enriched=false
  Ok(parse_search_output(&stdout_str))
}

pub async fn search_youtube(query: &str) -> Result<Vec<SearchEntry>> {
  let output = Command::new("yt-dlp")
    .args([
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
  Ok(parse_search_output(&stdout_str))
}

pub async fn get_video_info(video_id: &str) -> Result<VideoDetails> {
  let url = format!("https://youtube.com/watch?v={}", video_id);
  let output = Command::new("yt-dlp")
    .args([
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

  let opt = |s: Option<&str>| -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty() && *s != "NA").map(|s| s.to_string())
  };

  if output.status.success() {
    let info_str = String::from_utf8(output.stdout).context("Failed to parse yt-dlp info output as UTF-8")?;
    let mut lines = info_str.lines();
    let title = lines.next().map(|s| s.trim().to_string()).ok_or_else(|| anyhow!("Missing title in yt-dlp output"))?;
    let uploader = opt(lines.next());
    let duration = opt(lines.next());
    let upload_date = opt(lines.next());
    let tags = opt(lines.next())
      .map(|s| clean_tags(&s))
      .filter(|s| !s.is_empty())
      .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
      .unwrap_or_default();
    Ok(VideoDetails { url, title, uploader, duration, upload_date, tags })
  } else {
    Ok(VideoDetails {
      url,
      title: video_id.to_string(),
      uploader: None,
      duration: None,
      upload_date: None,
      tags: Vec::new(),
    })
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
