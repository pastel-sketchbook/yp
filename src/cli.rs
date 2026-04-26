//! CLI subcommand handlers.
//!
//! Each handler calls the core YouTube/transcription functions directly,
//! writes JSON to stdout, and progress/errors to stderr.

use anyhow::{Context, Result, anyhow};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::mpsc;

use crate::cache;
use crate::summarize;
use crate::transcript::TranscriptEvent;
use crate::youtube;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a `YouTube` video ID from a URL or bare ID string.
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
// Shell completions
// ---------------------------------------------------------------------------

/// Generate a custom zsh completion script with dynamic video ID support.
///
/// For the `info`, `transcript`, and `summarize` subcommands, the `video`
/// positional argument is completed by calling `yp _complete-ids` which reads
/// from the local cache (populated by `channel`, `search`, and `info`).
pub fn generate_zsh_completions() {
  print!(
    r#"#compdef yp

_yp_video_ids() {{
  local -a ids
  ids=(${{(f)"$(yp _complete-ids 2>/dev/null)"}})
  _describe 'video' ids
}}

_yp() {{
  local -a commands
  commands=(
    'completions:Generate shell completions'
    'search:Search YouTube and return results as JSON'
    'channel:List videos from a YouTube channel (JSONL)'
    'info:Fetch metadata for a specific video (JSON)'
    'transcript:Transcribe a video (JSONL)'
    'summarize:Transcribe + classify + reduce to a summary (JSON)'
  )

  _arguments -C \
    '-d+[Display mode]:mode:(auto kitty sixel direct ascii)' \
    '--display-mode+[Display mode]:mode:(auto kitty sixel direct ascii)' \
    '-h[Show help]' \
    '--help[Show help]' \
    '-V[Show version]' \
    '--version[Show version]' \
    '1:command:->cmd' \
    '*::arg:->args'

  case $state in
    cmd)
      _describe 'command' commands
      ;;
    args)
      case $words[1] in
        completions)
          _arguments '1:shell:(bash zsh fish elvish powershell)'
          ;;
        search)
          _arguments \
            '1:query:' \
            '-l+[Max results]:limit:' \
            '--limit+[Max results]:limit:'
          ;;
        channel)
          _arguments \
            '1::channel:' \
            '-l+[Max videos]:limit:' \
            '--limit+[Max videos]:limit:' \
            '-a[Fetch all videos]' \
            '--all[Fetch all videos]' \
            '-e[Enrich with tags]' \
            '--enrich[Enrich with tags]' \
            '-j+[Concurrent jobs]:jobs:' \
            '--jobs+[Concurrent jobs]:jobs:'
          ;;
        info)
          _arguments '1:video:_yp_video_ids'
          ;;
        transcript)
          _arguments \
            '1::video:_yp_video_ids' \
            '-r[Output raw utterances]' \
            '--raw[Output raw utterances]'
          ;;
        summarize)
          _arguments \
            '1::video:_yp_video_ids' \
            '--latest+[Summarize latest N from channel]:count:' \
            '-r[Output raw transcript]' \
            '--raw[Output raw transcript]'
          ;;
      esac
      ;;
  esac
}}

_yp "$@"
"#
  );
}

// ---------------------------------------------------------------------------
// Subcommand: search
// ---------------------------------------------------------------------------

/// Search `YouTube` and output results as a JSON array.
pub async fn cmd_search(query: &str, limit: usize) -> Result<()> {
  eprintln!("Searching YouTube for: {query}");
  let mut results = youtube::search_youtube(query).await.context("YouTube search failed")?;
  results.truncate(limit);

  // Cache video IDs for shell completion.
  let cache_pairs: Vec<(&str, &str)> = results.iter().map(|e| (e.video_id.as_str(), e.title.as_str())).collect();
  if let Err(e) = cache::append_videos(&cache_pairs) {
    tracing::warn!("Failed to update video cache: {}", e);
  }

  let json = serde_json::to_string_pretty(&results).context("Failed to serialize search results")?;
  println!("{json}");
  Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand: channel
// ---------------------------------------------------------------------------

/// Build a JSON object for a channel entry, optionally merging enriched metadata.
/// `SearchEntry` fields (from flat-playlist) are used as defaults; `VideoMeta` overrides when present.
fn channel_entry_json(entry: &youtube::SearchEntry, meta: Option<&youtube::VideoMeta>) -> serde_json::Value {
  let mut obj = serde_json::json!({
    "video_id": entry.video_id,
    "title": entry.title,
    "url": format!("https://youtube.com/watch?v={}", entry.video_id),
  });
  // Populate from SearchEntry (flat-playlist data)
  if let Some(date) = &entry.upload_date {
    obj["upload_date"] = serde_json::Value::String(date.clone());
  }
  if let Some(tags) = &entry.tags {
    obj["tags"] = serde_json::Value::String(tags.clone());
  }
  if let Some(dur) = &entry.duration {
    obj["duration"] = serde_json::Value::String(dur.clone());
  }
  if let Some(vc) = &entry.view_count {
    obj["view_count"] = serde_json::Value::String(vc.clone());
  }
  if let Some(up) = &entry.uploader {
    obj["uploader"] = serde_json::Value::String(up.clone());
  }
  // Override with enriched metadata when available
  if let Some(meta) = meta {
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
  obj
}

/// Write a single JSONL line to stdout and flush immediately.
///
/// Explicit flushing is required because Rust uses full buffering when stdout
/// is piped. Without it, downstream processes (e.g. `fzf`) won't see data
/// until the 8 KB buffer fills or the process exits.
fn write_jsonl(obj: &serde_json::Value) -> Result<()> {
  use std::io::Write;
  let stdout = std::io::stdout();
  let mut lock = stdout.lock();
  serde_json::to_writer(&mut lock, obj).context("Failed to write JSONL")?;
  writeln!(lock).context("Failed to write newline")?;
  lock.flush().context("Failed to flush stdout")?;
  Ok(())
}

/// List videos from a `YouTube` channel, output as JSONL.
///
/// `count`: `Some(n)` for n videos, `None` for all.
/// `jobs`: number of concurrent enrichment processes.
pub async fn cmd_channel(channel: &str, count: Option<usize>, enrich: bool, jobs: usize) -> Result<()> {
  let channel_url =
    youtube::detect_channel_url(channel).ok_or_else(|| anyhow!("Could not detect channel URL from: {channel}"))?;

  let label = count.map_or("all".to_string(), |n| n.to_string());
  eprintln!("Listing {label} videos from: {channel_url}");
  let entries = youtube::list_channel_videos(&channel_url, 1, count).await.context("Failed to list channel videos")?;
  eprintln!("Found {} videos", entries.len());

  // Cache video IDs for shell completion.
  let cache_pairs: Vec<(&str, &str)> = entries.iter().map(|e| (e.video_id.as_str(), e.title.as_str())).collect();
  if let Err(e) = cache::append_videos(&cache_pairs) {
    tracing::warn!("Failed to update video cache: {}", e);
  }

  if enrich && !entries.is_empty() {
    eprintln!("Enriching {} videos with metadata ({} concurrent)...", entries.len(), jobs);

    // Build a lookup so we can merge entry titles with enrichment results.
    let entry_map: std::collections::HashMap<String, &youtube::SearchEntry> =
      entries.iter().map(|e| (e.video_id.clone(), e)).collect();
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();

    let video_ids: Vec<String> = entries.iter().map(|e| e.video_id.clone()).collect();
    let (tx, mut rx) = mpsc::channel(video_ids.len().max(1));

    // Spawn enrichment in background
    let enrich_handle = tokio::spawn(youtube::enrich_video_metadata(video_ids, tx, jobs));

    // Stream each enriched entry to stdout as it arrives.
    while let Some(meta) = rx.recv().await {
      if let Some(entry) = entry_map.get(&meta.video_id) {
        write_jsonl(&channel_entry_json(entry, Some(&meta)))?;
        emitted.insert(meta.video_id.clone());
      }
    }
    enrich_handle.await.context("Enrichment task failed")?;

    // Output any entries that failed enrichment (so no data is silently lost).
    for entry in &entries {
      if !emitted.contains(&entry.video_id) {
        write_jsonl(&channel_entry_json(entry, None))?;
      }
    }
  } else {
    // Output as JSONL without enrichment (fast mode)
    for entry in &entries {
      write_jsonl(&channel_entry_json(entry, None))?;
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
  eprintln!("Fetching info for video: {video_id}");

  let details = youtube::get_video_info(&video_id).await.context("Failed to get video info")?;

  // Cache video ID for shell completion.
  if let Err(e) = cache::append_videos(&[(&video_id, &details.title)]) {
    tracing::warn!("Failed to update video cache: {}", e);
  }

  let mut obj = serde_json::to_value(&details).context("Failed to serialize video details")?;
  obj["video_id"] = serde_json::Value::String(video_id);
  let json = serde_json::to_string_pretty(&obj).context("Failed to format video info JSON")?;
  println!("{json}");
  Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand: transcript
// ---------------------------------------------------------------------------

/// Transcribe a video and output utterances as JSONL.
///
/// This runs the full whisper pipeline headlessly (no mpv, no TUI).
#[allow(clippy::cast_precision_loss)]
pub async fn cmd_transcript(video: &str, raw: bool) -> Result<()> {
  let video_id = extract_video_id(video);
  let url = format!("https://youtube.com/watch?v={video_id}");
  eprintln!("Transcribing video: {video_id}");

  let utterances = run_transcription(&url, None).await?;

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
      println!("{json}");
    }
  }

  Ok(())
}

/// Read JSONL from stdin (pipe mode), extract `video_id`, and transcribe.
///
/// Enables: `yp channel | fzf | yp transcript`
pub async fn cmd_transcript_stdin(raw: bool) -> Result<()> {
  use std::io::BufRead;

  let stdin = std::io::stdin();
  let line = stdin
    .lock()
    .lines()
    .next()
    .ok_or_else(|| anyhow!("No input on stdin. Provide a video ID or pipe from `yp channel | fzf`."))?
    .context("Failed to read from stdin")?;

  let trimmed = line.trim();
  if trimmed.is_empty() {
    return Err(anyhow!("Empty input on stdin."));
  }

  // Try parsing as JSON to extract video_id field
  if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed)
    && let Some(id) = obj.get("video_id").and_then(|v| v.as_str())
  {
    return cmd_transcript(id, raw).await;
  }

  // Fall back: treat the whole line as a video ID or URL
  cmd_transcript(trimmed, raw).await
}

// ---------------------------------------------------------------------------
// Subcommand: summarize
// ---------------------------------------------------------------------------

/// Transcribe + classify + reduce a video to a summary, output as JSON.
#[allow(clippy::cast_precision_loss)]
pub async fn cmd_summarize(video: &str, raw: bool) -> Result<()> {
  let video_id = extract_video_id(video);
  let url = format!("https://youtube.com/watch?v={video_id}");

  eprintln!("Fetching video info...");
  let details = youtube::get_video_info(&video_id).await.context("Failed to get video info")?;

  eprintln!("Transcribing video: {} — {}", video_id, details.title);
  let duration_hint = details.duration.as_deref().and_then(parse_duration_secs);
  let utterances = run_transcription(&url, duration_hint).await?;

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
    println!("{json}");
  } else {
    // Classify + reduce
    let triples: Vec<(i64, i64, String)> = utterances.iter().map(|u| (u.start, u.stop, u.text.clone())).collect();
    let classified = summarize::classify(&triples);
    let output = summarize::reduce(&details, &classified);
    let json = serde_json::to_string_pretty(&output).context("Failed to serialize summary")?;
    println!("{json}");
  }

  eprintln!("Done.");
  Ok(())
}

/// Read JSONL from stdin (pipe mode), extract `video_id`, and summarize.
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
#[allow(clippy::cast_precision_loss)]
pub async fn cmd_summarize_latest(channel: &str, count: usize, raw: bool) -> Result<()> {
  let channel_url =
    youtube::detect_channel_url(channel).ok_or_else(|| anyhow!("Could not detect channel URL from: {channel}"))?;

  eprintln!("Listing latest {count} video(s) from: {channel_url}");
  let entries =
    youtube::list_channel_videos(&channel_url, 1, Some(count)).await.context("Failed to list channel videos")?;

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
      let url = format!("https://youtube.com/watch?v={video_id}");

      let details = match youtube::get_video_info(video_id).await {
        Ok(d) => d,
        Err(e) => {
          eprintln!("Warning: failed to get info for {video_id}: {e}");
          continue;
        }
      };

      let duration_hint = details.duration.as_deref().and_then(parse_duration_secs);
      let utterances = match run_transcription(&url, duration_hint).await {
        Ok(u) => u,
        Err(e) => {
          eprintln!("Warning: transcription failed for {video_id}: {e}");
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
      print!("{json}");
    }
    println!("]");
  }

  eprintln!("Done.");
  Ok(())
}

// ---------------------------------------------------------------------------
// Shared transcription runner
// ---------------------------------------------------------------------------

/// Parse a duration string like "16:30" or "1:23:45" into total seconds.
fn parse_duration_secs(s: &str) -> Option<u32> {
  let parts: Vec<&str> = s.split(':').collect();
  match parts.len() {
    2 => {
      let m: u32 = parts[0].parse().ok()?;
      let s: u32 = parts[1].parse().ok()?;
      Some(m * 60 + s)
    }
    3 => {
      let h: u32 = parts[0].parse().ok()?;
      let m: u32 = parts[1].parse().ok()?;
      let s: u32 = parts[2].parse().ok()?;
      Some(h * 3600 + m * 60 + s)
    }
    _ => None,
  }
}

/// Run the headless transcription pipeline and collect all utterances.
///
/// Uses `ipc_socket: None` to skip mpv IPC and go straight to `yt-dlp -g`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
async fn run_transcription(url: &str, duration_hint: Option<u32>) -> Result<Vec<whisper_cli::Utternace>> {
  let (tx, mut rx) = mpsc::unbounded_channel::<TranscriptEvent>();
  let whisper_cache: Arc<StdMutex<Option<whisper_cli::Whisper>>> = Arc::new(StdMutex::new(None));

  let handle = crate::transcript::spawn_transcription_pipeline(tx, url.to_string(), whisper_cache, None, duration_hint);

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
          eprintln!("Downloading whisper model... {mb_down}MB / {mb_total}MB [{pct}%]");
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
        return Err(anyhow!("Transcription failed: {msg}"));
      }
    }
  }

  let _ = handle.await;
  Ok(all_utterances)
}

// ---------------------------------------------------------------------------
// Subcommand: _complete-ids (hidden, for shell completions)
// ---------------------------------------------------------------------------

/// Output cached video IDs for shell completion.
///
/// If the cache is empty and `live` is true, fetches videos from the default
/// channel to seed the cache before outputting.
///
/// Output format: one `video_id\ttitle` line per entry (zsh `_describe` format).
pub async fn cmd_complete_ids(live: bool) -> Result<()> {
  use std::io::Write;

  let mut entries = cache::read_videos();

  // Live fallback: seed from default channel if cache is empty.
  if entries.is_empty() && live {
    let constants = crate::constants::constants();
    let channel = &constants.pastel_sketchbook_channel;
    if let Some(url) = youtube::detect_channel_url(channel)
      && let Ok(videos) = youtube::list_channel_videos(&url, 1, Some(30)).await
    {
      let pairs: Vec<(&str, &str)> = videos.iter().map(|e| (e.video_id.as_str(), e.title.as_str())).collect();
      let _ = cache::append_videos(&pairs);
      entries = videos.into_iter().map(|e| (e.video_id, e.title)).collect();
    }
  }

  let stdout = std::io::stdout();
  let mut lock = stdout.lock();
  for (id, title) in entries.iter().rev() {
    // Escape colons in title (zsh _describe uses colon as delimiter).
    let escaped = title.replace('\\', "\\\\").replace(':', "\\:");
    writeln!(lock, "{id}:{escaped}").context("Failed to write completion entry")?;
  }
  lock.flush().context("Failed to flush completion output")?;

  Ok(())
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

  #[test]
  fn parse_duration_mm_ss() {
    assert_eq!(parse_duration_secs("16:30"), Some(990));
  }

  #[test]
  fn parse_duration_h_mm_ss() {
    assert_eq!(parse_duration_secs("1:23:45"), Some(5025));
  }

  #[test]
  fn parse_duration_zero() {
    assert_eq!(parse_duration_secs("0:00"), Some(0));
  }

  #[test]
  fn parse_duration_invalid() {
    assert_eq!(parse_duration_secs("abc"), None);
  }
}
