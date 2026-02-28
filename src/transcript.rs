use anyhow::{Context, Result};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::constants::constants;

// --- Auto-transcription ---

/// Result of the transcription pipeline: either in progress or completed.
pub enum TranscriptEvent {
  /// Audio URL resolved, now downloading+transcribing in chunks.
  AudioExtracted,
  /// Whisper model download progress (downloaded bytes, total bytes).
  DownloadProgress(u64, u64),
  /// A chunk of utterances arrived (progressive — append to existing).
  ChunkTranscribed(Vec<whisper_cli::Utternace>),
  /// All chunks transcribed — pipeline complete.
  Transcribed,
  /// Pipeline failed with an error message.
  Failed(String),
}

/// Auto-transcription state machine.
///
/// When a track starts playing, the pipeline automatically:
/// 1. Resolves the audio stream URL (mpv IPC fast path, or yt-dlp -g fallback)
/// 2. Downloads + transcribes audio in 30-second chunks via ffmpeg + whisper
/// 3. Sends utterances progressively as each chunk completes
#[derive(Default)]
pub enum TranscriptState {
  /// No transcription in progress.
  #[default]
  Idle,
  /// Resolving audio URL / downloading first chunk.
  ExtractingAudio { handle: JoinHandle<()> },
  /// Actively transcribing chunks (utterances arriving progressively).
  Transcribing { handle: JoinHandle<()> },
  /// All chunks transcribed — utterances are stored in App.
  Ready,
}

/// Resolve a direct CDN stream URL for the given YouTube video.
///
/// Fast path: query mpv's IPC socket for `stream-open-filename` (~0.5-4s).
/// Fallback: use `yt-dlp -g --format bestaudio` to resolve the URL (~10-30s).
pub async fn resolve_stream_url(ipc_socket: Option<&str>, youtube_url: &str) -> Result<String> {
  use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};

  // Fast path: mpv already resolved the URL, ask for it via IPC.
  if let Some(socket_path) = ipc_socket {
    for attempt in 0..6 {
      let delay = match attempt {
        0 => Duration::from_millis(500),
        1 => Duration::from_secs(1),
        _ => Duration::from_secs(2),
      };
      tokio::time::sleep(delay).await;

      let mut stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(s) => s,
        Err(e) => {
          info!(attempt, err = %e, "transcript: mpv IPC connect failed, retrying");
          continue;
        }
      };

      let cmd = b"{\"command\":[\"get_property\",\"stream-open-filename\"],\"request_id\":1}\n";
      if let Err(e) = stream.write_all(cmd).await {
        info!(attempt, err = %e, "transcript: mpv IPC write failed, retrying");
        continue;
      }

      let reader = TokioBufReader::new(stream);
      let mut lines = reader.lines();
      let mut found_response = false;

      for _ in 0..20 {
        let line = match tokio::time::timeout(Duration::from_secs(3), lines.next_line()).await {
          Ok(Ok(Some(line))) => line,
          _ => break,
        };

        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line)
          && val.get("request_id").and_then(|v| v.as_i64()) == Some(1)
        {
          found_response = true;
          if val.get("error").and_then(|v| v.as_str()) == Some("success")
            && let Some(url) = val.get("data").and_then(|v| v.as_str())
          {
            // Accept only resolved CDN URLs, not the original YouTube URL
            if url.starts_with("http") && !url.contains("youtube.com/watch") && !url.contains("youtu.be/") {
              info!(url = %url, "transcript: got stream URL from mpv IPC");
              return Ok(url.to_string());
            }
          }
          break;
        }
      }

      if found_response {
        info!(attempt, "transcript: mpv IPC responded but no resolved CDN URL yet");
      }
    }
    info!("transcript: mpv IPC exhausted retries, falling back to yt-dlp -g");
  }

  // Fallback: use yt-dlp to resolve the URL (slow but reliable).
  info!("transcript: resolving stream URL via yt-dlp -g");
  let output = tokio::process::Command::new("yt-dlp")
    .args(["-g", "--format", "bestaudio", youtube_url])
    .stdin(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .output()
    .await
    .map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!("yt-dlp not found. Install with: brew install yt-dlp")
      } else {
        anyhow::anyhow!("Failed to start yt-dlp: {}", e)
      }
    })?;

  if !output.status.success() {
    return Err(anyhow::anyhow!("yt-dlp -g failed with status {}", output.status));
  }

  let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
  if url.is_empty() {
    return Err(anyhow::anyhow!("yt-dlp -g returned empty output"));
  }

  info!(url = %url, "transcript: resolved stream URL via yt-dlp -g");
  Ok(url)
}

/// Download the whisper model ourselves (instead of letting whisper-cli-rs do it via indicatif)
/// so we can send progress events to the TUI for a nice progress bar.
pub async fn download_whisper_model(
  tx: &mpsc::UnboundedSender<TranscriptEvent>,
  model_path: &std::path::Path,
) -> Result<()> {
  use futures::StreamExt;

  let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin", whisper_cli::Size::Small);

  info!(url = %url, "transcript: downloading whisper model");

  let response = reqwest::get(&url).await.context("Failed to download whisper model")?;

  let total = response.content_length().unwrap_or(0);
  let mut downloaded: u64 = 0;

  // Ensure parent directory exists
  if let Some(parent) = model_path.parent() {
    std::fs::create_dir_all(parent).context("Failed to create model cache directory")?;
  }

  // Write to a temp file, then rename (atomic)
  let tmp_path = model_path.with_extension("bin.part");
  let mut file = tokio::fs::File::create(&tmp_path).await.context("Failed to create model file")?;

  let mut stream = response.bytes_stream();
  // Throttle progress events: send at most every 100ms
  let mut last_progress = std::time::Instant::now();

  while let Some(chunk) = stream.next().await {
    let chunk = chunk.context("Error downloading model chunk")?;
    tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await.context("Error writing model file")?;

    downloaded += chunk.len() as u64;
    if last_progress.elapsed() >= Duration::from_millis(100) || downloaded >= total {
      let _ = tx.send(TranscriptEvent::DownloadProgress(downloaded, total));
      last_progress = std::time::Instant::now();
    }
  }

  tokio::io::AsyncWriteExt::flush(&mut file).await.context("Error flushing model file")?;
  drop(file);

  // Rename temp file to final path
  tokio::fs::rename(&tmp_path, model_path).await.context("Failed to finalize model file")?;

  info!(path = %model_path.display(), "transcript: whisper model downloaded");
  // Clear progress after download completes
  let _ = tx.send(TranscriptEvent::DownloadProgress(total, total));
  Ok(())
}

/// RAII guard that redirects stderr to /dev/null while alive.
/// Restores original file descriptor on drop.
/// Used to suppress whisper.cpp C library logging that writes directly to fd 2.
pub struct SuppressStdio {
  saved_stderr: libc::c_int,
}

impl SuppressStdio {
  pub fn new() -> Self {
    // Safety: dup() and dup2() are standard POSIX calls. We save the original
    // stderr fd and redirect to /dev/null. We only suppress stderr (fd 2)
    // because stdout (fd 1) is used by the TUI — redirecting it would make
    // terminal rendering invisible while whisper transcription runs.
    unsafe {
      let saved_stderr = libc::dup(2);
      let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
      if devnull >= 0 {
        libc::dup2(devnull, 2);
        libc::close(devnull);
      } else {
        warn!("transcript: failed to open /dev/null for stdio suppression");
      }
      Self { saved_stderr }
    }
  }
}

impl Drop for SuppressStdio {
  fn drop(&mut self) {
    // Safety: restoring the saved file descriptor to its original value.
    unsafe {
      if self.saved_stderr >= 0 {
        libc::dup2(self.saved_stderr, 2);
        libc::close(self.saved_stderr);
      }
    }
  }
}

/// Run the chunked transcription pipeline as an async task.
///
/// Stages:
/// 1. Resolve CDN stream URL (mpv IPC fast path, or yt-dlp fallback)
/// 2. Download whisper model if needed
/// 3. Loop: download 30s chunk via ffmpeg → transcribe → send utterances → next chunk
pub fn spawn_transcription_pipeline(
  tx: mpsc::UnboundedSender<TranscriptEvent>,
  url: String,
  whisper_cache: Arc<StdMutex<Option<whisper_cli::Whisper>>>,
  ipc_socket: Option<String>,
) -> JoinHandle<()> {
  tokio::spawn(async move {
    // Stage 1: Resolve the direct CDN stream URL.
    let stream_url = match resolve_stream_url(ipc_socket.as_deref(), &url).await {
      Ok(resolved) => {
        info!(stream_url = %resolved, "transcript: resolved stream URL");
        resolved
      }
      Err(e) => {
        tracing::error!(err = %e, "transcript: failed to resolve stream URL");
        let _ = tx.send(TranscriptEvent::Failed(format!("{:#}", e)));
        return;
      }
    };

    // Signal that URL is resolved, moving to transcription
    let _ = tx.send(TranscriptEvent::AudioExtracted);

    // Stage 2: Download whisper model if needed
    let model_path = whisper_cli::Size::Small.get_path();
    if !model_path.exists() {
      info!("transcript: whisper model not found, downloading");
      if let Err(e) = download_whisper_model(&tx, &model_path).await {
        let _ = tx.send(TranscriptEvent::Failed(format!("Model download failed: {:#}", e)));
        return;
      }
    }

    // Stage 3: Chunked download + transcription loop.
    // Each iteration: ffmpeg downloads chunk_secs of audio → whisper transcribes → send utterances.
    let chunk_secs = constants().chunk_secs;
    let chunk_path = std::env::temp_dir().join(format!("yp-chunk-{}.wav", std::process::id()));
    let mut offset_secs: u32 = 0;

    loop {
      // Clean up previous chunk
      let _ = std::fs::remove_file(&chunk_path);

      // Download this chunk with ffmpeg
      let chunk_str = chunk_path.to_str().unwrap_or("/tmp/yp-chunk.wav");
      let offset_str = offset_secs.to_string();
      let duration_str = chunk_secs.to_string();

      info!(offset = offset_secs, duration = chunk_secs, "transcript: downloading chunk");

      let ffmpeg_result = tokio::process::Command::new("ffmpeg")
        .args([
          "-y",
          "-ss",
          &offset_str,
          "-t",
          &duration_str,
          "-i",
          &stream_url,
          "-vn",
          "-ar",
          "16000",
          "-ac",
          "1",
          "-f",
          "wav",
          chunk_str,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

      match ffmpeg_result {
        Ok(status) if status.success() => {}
        Ok(status) => {
          // Non-zero exit likely means we've gone past the end of the stream
          info!(offset = offset_secs, code = ?status.code(), "transcript: ffmpeg exited non-zero, assuming end of stream");
          break;
        }
        Err(e) => {
          let msg = if e.kind() == std::io::ErrorKind::NotFound {
            "ffmpeg not found. Install with: brew install ffmpeg".to_string()
          } else {
            format!("Failed to start ffmpeg: {}", e)
          };
          let _ = tx.send(TranscriptEvent::Failed(msg));
          return;
        }
      }

      // Check chunk file size — WAV header is 44 bytes; if file is <=44 bytes, no audio data.
      // Also skip very short chunks (<32KB ≈ <1s of 16kHz mono 16-bit) that cause whisper
      // to panic with GenericError(-3).
      let min_chunk_bytes = constants().min_chunk_bytes;
      let chunk_size = std::fs::metadata(&chunk_path).map(|m| m.len()).unwrap_or(0);
      if chunk_size <= 44 {
        info!(offset = offset_secs, "transcript: chunk has no audio data, end of stream");
        break;
      }
      if chunk_size < min_chunk_bytes {
        info!(offset = offset_secs, size = chunk_size, "transcript: chunk too short for whisper, skipping");
        offset_secs = offset_secs.saturating_add(chunk_secs);
        continue;
      }

      // Transcribe this chunk
      let chunk_for_whisper = chunk_path.clone();
      let cache = Arc::clone(&whisper_cache);
      let chunk_offset = offset_secs;

      let transcribe_result = tokio::task::spawn_blocking(move || {
        // Suppress whisper.cpp C library logging (writes directly to stderr)
        let _guard = SuppressStdio::new();

        // Safety: mutex is never held across an await/yield point and we don't
        // panic while holding the lock, so poisoning cannot occur in practice.
        let mut lock = cache.lock().expect("whisper cache mutex poisoned");
        if lock.is_none() {
          info!("transcript: loading whisper model (Small) — first time, will be cached");
          let model = whisper_cli::Model::new(whisper_cli::Size::Small);
          let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Failed to create tokio runtime for model init")?;
          let whisper = rt.block_on(whisper_cli::Whisper::new(model, Some(whisper_cli::Language::Auto)));
          *lock = Some(whisper);
          info!("transcript: whisper model loaded and cached");
        }

        // Safety: we just checked is_none() and set it above, or it was already Some.
        let whisper = lock.as_mut().expect("whisper instance just set or already present");

        info!(offset = chunk_offset, "transcript: transcribing chunk");
        let transcript =
          whisper.transcribe(&chunk_for_whisper, false, false).context("Whisper transcription failed")?;

        // Adjust timestamps: whisper returns times relative to chunk start,
        // we need them relative to the full track.
        let offset_cs = (chunk_offset as i64) * 100; // centiseconds
        let mut utterances = transcript.utterances;
        for u in &mut utterances {
          u.start = u.start.saturating_add(offset_cs);
          u.stop = u.stop.saturating_add(offset_cs);
        }

        Ok::<Vec<whisper_cli::Utternace>, anyhow::Error>(utterances)
      })
      .await;

      match transcribe_result {
        Ok(Ok(utterances)) => {
          info!(segments = utterances.len(), offset = offset_secs, "transcript: chunk transcribed");
          if !utterances.is_empty() {
            let _ = tx.send(TranscriptEvent::ChunkTranscribed(utterances));
          }
        }
        Ok(Err(e)) => {
          // Skip failed chunk and continue — don't abort the pipeline.
          // Whisper can fail on short/silent chunks (e.g. GenericError(-3)).
          warn!(err = %e, offset = offset_secs, "transcript: chunk transcription failed, skipping");
        }
        Err(e) => {
          // spawn_blocking panicked (whisper.cpp internal crash on bad input).
          // Skip this chunk and continue the pipeline.
          warn!(err = %e, offset = offset_secs, "transcript: chunk task panicked, skipping");
        }
      }

      offset_secs = offset_secs.saturating_add(chunk_secs);
    }

    // Clean up chunk file and signal completion
    let _ = std::fs::remove_file(&chunk_path);
    info!(total_offset = offset_secs, "transcript: all chunks processed");
    let _ = tx.send(TranscriptEvent::Transcribed);
  })
}
