use anyhow::{Context, Result, anyhow};
use image::DynamicImage;
use reqwest::Client;
use std::process::Stdio;
use tokio::process::Command;

use crate::player::VideoDetails;

pub async fn search_youtube(query: &str) -> Result<Vec<(String, String)>> {
  let output = Command::new("yt-dlp")
    .args([
      "--print",
      "%(title)s\t%(id)s",
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
  Ok(
    stdout_str
      .lines()
      .map(str::trim)
      .filter(|l| !l.is_empty())
      .filter_map(|l| l.split_once('\t').map(|(t, id)| (t.to_string(), id.to_string())))
      .collect(),
  )
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
