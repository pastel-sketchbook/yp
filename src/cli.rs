//! CLI subcommand handlers.
//!
//! Each handler calls the core YouTube/transcription functions directly,
//! writes JSON to stdout, and progress/errors to stderr.

use anyhow::{Context, Result, anyhow};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::mpsc;

use crate::summarize;
use crate::transcript::TranscriptEvent;
use crate::youtube;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a YouTube video ID from a URL or bare ID string.
///
/// Accepts:
/// - `abc123` (bare 11-char ID)
/// - `https://youtube.com/watch?v=abc123`
/// - `https://www.youtube.com/watch?v=abc123`
/// - `https://youtu.be/abc123`
/// - `https://youtube.com/watch?v=abc123&list=...` (strips extra params)
pub fn extract_video_id(input: &str) -> String {
  let trimmed = input.trim();

  // youtu.be short URLs
  if let Some(rest) = trimmed.strip_prefix("https://youtu.be/").or_else(|| trimmed.strip_prefix("http://youtu.be/")) {
    return rest.split(['?', '&', '/']).next().unwrap_or(rest).to_string();
  }

  // youtube.com/watch?v=...
  if trimmed.contains("youtube.com/watch")
    && let Some(query) = trimmed.split('?').nth(1)
  {
    for param in query.split('&') {
      if let Some(value) = param.strip_prefix("v=") {
        return value.split('&').next().unwrap_or(value).to_string();
      }
    }
  }

  // Assume it's already a bare video ID
  trimmed.to_string()
}

/// Print a JSON error to stdout and return Ok(()) for clean CLI exit.
fn print_json_error(error_code: &str, message: &str) -> Result<()> {
  let err = serde_json::json!({
    "error": error_code,
    "message": message,
  });
  println!("{}", serde_json::to_string(&err).context("Failed to serialize error JSON")?);
  Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand: search
// ---------------------------------------------------------------------------

/// Search YouTube and output results as a JSON array.
pub async fn cmd_search(query: &str, limit: usize) -> Result<()> {
  eprintln!("Searching YouTube for: {}", query);
  let mut results = youtube::search_youtube(query).await.context("YouTube search failed")?;
  results.truncate(limit);

  let json = serde_json::to_string_pretty(&results).context("Failed to serialize search results")?;
  println!("{}", json);
  Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand: channel
// ---------------------------------------------------------------------------

/// List videos from a YouTube channel, output as JSONL.
pub async fn cmd_channel(channel: &str, limit: usize, enrich: bool) -> Result<()> {
  let channel_url =
    youtube::detect_channel_url(channel).ok_or_else(|| anyhow!("Could not detect channel URL from: {}", channel))?;

  eprintln!("Listing videos from: {}", channel_url);
  let entries = youtube::list_channel_videos(&channel_url, 1, limit).await.context("Failed to list channel videos")?;

  if enrich && !entries.is_empty() {
    eprintln!("Enriching {} videos with metadata...", entries.len());
    let video_ids: Vec<String> = entries.iter().map(|e| e.video_id.clone()).collect();
    let (tx, mut rx) = mpsc::channel(video_ids.len());

    // Spawn enrichment in background
    let enrich_handle = tokio::spawn(youtube::enrich_video_metadata(video_ids, tx));

    // Collect enriched metadata
    let mut enriched: std::collections::HashMap<String, youtube::VideoMeta> = std::collections::HashMap::new();
    while let Some(meta) = rx.recv().await {
      enriched.insert(meta.video_id.clone(), meta);
    }
    enrich_handle.await.context("Enrichment task failed")?;

    // Output entries with enriched data merged
    for entry in &entries {
      let mut obj = serde_json::json!({
        "video_id": entry.video_id,
        "title": entry.title,
        "url": format!("https://youtube.com/watch?v={}", entry.video_id),
      });
      if let Some(meta) = enriched.get(&entry.video_id) {
        if let Some(date) = &meta.upload_date {
          obj["upload_date"] = serde_json::Value::String(date.clone());
        }
        if let Some(tags) = &meta.tags {
          obj["tags"] = serde_json::Value::String(tags.clone());
        }
        if let Some(dur) = &meta.duration {
          obj["duration"] = serde_json::Value::String(dur.clone());
        }
        if let Some(vc) = &meta.view_count {
          obj["view_count"] = serde_json::Value::String(vc.clone());
        }
        if let Some(up) = &meta.uploader {
          obj["uploader"] = serde_json::Value::String(up.clone());
        }
      }
      println!("{}", serde_json::to_string(&obj).context("Failed to serialize enriched entry")?);
    }
  } else {
    // Output as JSONL without enrichment (fast mode)
    for entry in &entries {
      let obj = serde_json::json!({
        "video_id": entry.video_id,
        "title": entry.title,
        "url": format!("https://youtube.com/watch?v={}", entry.video_id),
      });
      let json = serde_json::to_string(&obj).context("Failed to serialize entry")?;
      println!("{}", json);
    }
  }

  Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand: info
// ---------------------------------------------------------------------------

/// Fetch metadata for a specific video, output as JSON.
pub async fn cmd_info(video: &str) -> Result<()> {
  let video_id = extract_video_id(video);
  eprintln!("Fetching info for video: {}", video_id);

  let details = youtube::get_video_info(&video_id).await.context("Failed to get video info")?;

  let mut obj = serde_json::to_value(&details).context("Failed to serialize video details")?;
  obj["video_id"] = serde_json::Value::String(video_id);
  let json = serde_json::to_string_pretty(&obj).context("Failed to format video info JSON")?;
  println!("{}", json);
  Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand: transcript
// ---------------------------------------------------------------------------

/// Transcribe a video and output utterances as JSONL.
///
/// This runs the full whisper pipeline headlessly (no mpv, no TUI).
pub async fn cmd_transcript(video: &str, raw: bool) -> Result<()> {
  let video_id = extract_video_id(video);
  let url = format!("https://youtube.com/watch?v={}", video_id);
  eprintln!("Transcribing video: {}", video_id);

  let utterances = run_transcription(&url).await?;

  if raw {
    // Output raw utterances as JSONL
    for u in &utterances {
      let obj = serde_json::json!({
        "start": u.start as f64 / 100.0,
        "end": u.stop as f64 / 100.0,
        "text": u.text,
      });
      println!("{}", serde_json::to_string(&obj).context("Failed to serialize utterance")?);
    }
  } else {
    // Classify and output with tags
    let triples: Vec<(i64, i64, String)> = utterances.iter().map(|u| (u.start, u.stop, u.text.clone())).collect();
    let classified = summarize::classify(&triples);
    for u in &classified {
      let json = serde_json::to_string(u).context("Failed to serialize classified utterance")?;
      println!("{}", json);
    }
  }

  Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand: summarize
// ---------------------------------------------------------------------------

/// Transcribe + classify + reduce a video to a summary, output as JSON.
pub async fn cmd_summarize(video: &str, raw: bool) -> Result<()> {
  let video_id = extract_video_id(video);
  let url = format!("https://youtube.com/watch?v={}", video_id);

  eprintln!("Fetching video info...");
  let details = youtube::get_video_info(&video_id).await.context("Failed to get video info")?;

  eprintln!("Transcribing video: {} — {}", video_id, details.title);
  let utterances = run_transcription(&url).await?;

  if raw {
    // Raw mode: video info + unprocessed transcript
    let output = serde_json::json!({
      "_hint": "YouTube video raw transcript. No classification or filtering applied. Use without --raw for a summarized version.",
      "video": details,
      "utterances": utterances.iter().map(|u| serde_json::json!({
        "start": u.start as f64 / 100.0,
        "end": u.stop as f64 / 100.0,
        "text": u.text,
      })).collect::<Vec<_>>(),
    });
    let json = serde_json::to_string_pretty(&output).context("Failed to serialize raw output")?;
    println!("{}", json);
  } else {
    // Classify + reduce
    let triples: Vec<(i64, i64, String)> = utterances.iter().map(|u| (u.start, u.stop, u.text.clone())).collect();
    let classified = summarize::classify(&triples);
    let output = summarize::reduce(&details, &classified);
    let json = serde_json::to_string_pretty(&output).context("Failed to serialize summary")?;
    println!("{}", json);
  }

  eprintln!("Done.");
  Ok(())
}

/// Read JSONL from stdin (pipe mode), extract video_id, and summarize.
///
/// Enables: `yp channel | fzf | yp summarize`
pub async fn cmd_summarize_stdin(raw: bool) -> Result<()> {
  use std::io::BufRead;

  let stdin = std::io::stdin();
  let line = stdin
    .lock()
    .lines()
    .next()
    .ok_or_else(|| anyhow!("No input on stdin. Provide a video ID, use --latest, or pipe from `yp channel | fzf`."))?
    .context("Failed to read from stdin")?;

  let trimmed = line.trim();
  if trimmed.is_empty() {
    return Err(anyhow!("Empty input on stdin."));
  }

  // Try parsing as JSON to extract video_id field
  if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed)
    && let Some(id) = obj.get("video_id").and_then(|v| v.as_str())
  {
    return cmd_summarize(id, raw).await;
  }

  // Fall back: treat the whole line as a video ID or URL
  cmd_summarize(trimmed, raw).await
}

/// Summarize the latest N videos from a channel.
pub async fn cmd_summarize_latest(channel: &str, count: usize, raw: bool) -> Result<()> {
  let channel_url =
    youtube::detect_channel_url(channel).ok_or_else(|| anyhow!("Could not detect channel URL from: {}", channel))?;

  eprintln!("Listing latest {} video(s) from: {}", count, channel_url);
  let entries = youtube::list_channel_videos(&channel_url, 1, count).await.context("Failed to list channel videos")?;

  if entries.is_empty() {
    return print_json_error("no_videos", "No videos found in the channel");
  }

  if count == 1 {
    // Single video: output as a JSON object (not array)
    let entry = &entries[0];
    cmd_summarize(&entry.video_id, raw).await?;
  } else {
    // Multiple videos: output as JSON array
    print!("[");
    for (i, entry) in entries.iter().enumerate() {
      eprintln!("\n--- Video {}/{}: {} ---", i + 1, entries.len(), entry.title);
      let video_id = &entry.video_id;
      let url = format!("https://youtube.com/watch?v={}", video_id);

      let details = match youtube::get_video_info(video_id).await {
        Ok(d) => d,
        Err(e) => {
          eprintln!("Warning: failed to get info for {}: {}", video_id, e);
          continue;
        }
      };

      let utterances = match run_transcription(&url).await {
        Ok(u) => u,
        Err(e) => {
          eprintln!("Warning: transcription failed for {}: {}", video_id, e);
          continue;
        }
      };

      let json = if raw {
        serde_json::to_string_pretty(&serde_json::json!({
          "_hint": "YouTube video raw transcript.",
          "video": details,
          "utterances": utterances.iter().map(|u| serde_json::json!({
            "start": u.start as f64 / 100.0,
            "end": u.stop as f64 / 100.0,
            "text": u.text,
          })).collect::<Vec<_>>(),
        }))
        .context("Failed to serialize raw output")?
      } else {
        let triples: Vec<(i64, i64, String)> = utterances.iter().map(|u| (u.start, u.stop, u.text.clone())).collect();
        let classified = summarize::classify(&triples);
        let output = summarize::reduce(&details, &classified);
        serde_json::to_string_pretty(&output).context("Failed to serialize summary")?
      };

      if i > 0 {
        print!(",");
      }
      print!("{}", json);
    }
    println!("]");
  }

  eprintln!("Done.");
  Ok(())
}

// ---------------------------------------------------------------------------
// Shared transcription runner
// ---------------------------------------------------------------------------

/// Run the headless transcription pipeline and collect all utterances.
///
/// Uses `ipc_socket: None` to skip mpv IPC and go straight to `yt-dlp -g`.
async fn run_transcription(url: &str) -> Result<Vec<whisper_cli::Utternace>> {
  let (tx, mut rx) = mpsc::unbounded_channel::<TranscriptEvent>();
  let whisper_cache: Arc<StdMutex<Option<whisper_cli::Whisper>>> = Arc::new(StdMutex::new(None));

  let handle = crate::transcript::spawn_transcription_pipeline(tx, url.to_string(), whisper_cache, None);

  let mut all_utterances: Vec<whisper_cli::Utternace> = Vec::new();
  let mut chunk_count: u32 = 0;

  while let Some(event) = rx.recv().await {
    match event {
      TranscriptEvent::AudioExtracted => {
        eprintln!("Audio URL resolved, transcribing...");
      }
      TranscriptEvent::DownloadProgress(downloaded, total) => {
        if total > 0 {
          let pct = (downloaded as f64 / total as f64 * 100.0) as u32;
          let mb_down = downloaded / (1024 * 1024);
          let mb_total = total / (1024 * 1024);
          eprintln!("Downloading whisper model... {}MB / {}MB [{}%]", mb_down, mb_total, pct);
        }
      }
      TranscriptEvent::ChunkTranscribed(utterances) => {
        chunk_count += 1;
        let count = utterances.len();
        all_utterances.extend(utterances);
        eprintln!("Chunk {} transcribed ({} segments, {} total)", chunk_count, count, all_utterances.len());
      }
      TranscriptEvent::Transcribed => {
        eprintln!("Transcription complete: {} total segments", all_utterances.len());
        break;
      }
      TranscriptEvent::Failed(msg) => {
        // Wait for the spawned task to finish before returning.
        let _ = handle.await;
        return Err(anyhow!("Transcription failed: {}", msg));
      }
    }
  }

  let _ = handle.await;
  Ok(all_utterances)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn extract_video_id_bare() {
    assert_eq!(extract_video_id("dQw4w9WgXcQ"), "dQw4w9WgXcQ");
  }

  #[test]
  fn extract_video_id_full_url() {
    assert_eq!(extract_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), "dQw4w9WgXcQ");
  }

  #[test]
  fn extract_video_id_short_url() {
    assert_eq!(extract_video_id("https://youtu.be/dQw4w9WgXcQ"), "dQw4w9WgXcQ");
  }

  #[test]
  fn extract_video_id_with_extra_params() {
    assert_eq!(extract_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL123"), "dQw4w9WgXcQ");
  }

  #[test]
  fn extract_video_id_with_whitespace() {
    assert_eq!(extract_video_id("  dQw4w9WgXcQ  "), "dQw4w9WgXcQ");
  }

  #[test]
  fn extract_video_id_short_url_with_params() {
    assert_eq!(extract_video_id("https://youtu.be/dQw4w9WgXcQ?t=30"), "dQw4w9WgXcQ");
  }
}
