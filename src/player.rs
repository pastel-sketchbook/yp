use anyhow::{Context, Result, anyhow};
use image::DynamicImage;
use reqwest::Client;
use std::process::Stdio;
use tokio::{
  io::BufReader as TokioBufReader,
  io::{AsyncBufReadExt, AsyncWriteExt},
  process::{Child as TokioChild, Command},
  sync::mpsc,
  task::JoinHandle,
};

use crate::display::DisplayMode;

#[derive(Debug, Clone)]
pub struct VideoDetails {
  pub url: String,
  pub title: String,
  pub uploader: Option<String>,
  pub duration: Option<String>,
  pub upload_date: Option<String>,
  pub view_count: Option<String>,
  pub tags: Vec<String>,
}

pub struct MusicPlayer {
  pub http_client: Client,
  pub(crate) current_process: Option<TokioChild>,
  pub display_mode: DisplayMode,
  pub current_details: Option<VideoDetails>,
  pub cached_thumbnail: Option<(String, DynamicImage)>,
  mpv_monitor_handle: Option<JoinHandle<()>>,
  mpv_status_rx: Option<mpsc::Receiver<String>>,
  last_mpv_status: Option<String>,
  ipc_socket_path: Option<String>,
  pub paused: bool,
}

impl MusicPlayer {
  pub fn new(display_mode: DisplayMode) -> Self {
    Self {
      http_client: Client::new(),
      current_process: None,
      display_mode,
      current_details: None,
      cached_thumbnail: None,
      mpv_monitor_handle: None,
      mpv_status_rx: None,
      last_mpv_status: None,
      ipc_socket_path: None,
      paused: false,
    }
  }

  pub fn is_playing(&self) -> bool {
    self.current_process.is_some()
  }

  pub fn check_mpv_status(&mut self) {
    if let Some(rx) = &mut self.mpv_status_rx {
      while let Ok(status) = rx.try_recv() {
        self.last_mpv_status = Some(status);
      }
    }
  }

  pub fn get_last_mpv_status(&self) -> Option<String> {
    self.last_mpv_status.clone()
  }

  pub fn ipc_socket_path(&self) -> Option<&str> {
    self.ipc_socket_path.as_deref()
  }

  pub async fn play(&mut self, details: VideoDetails) -> Result<()> {
    self.stop().await.context("Failed to stop previous playback")?;
    self.current_details = Some(details.clone());
    self.paused = false;

    let socket_path = std::env::temp_dir().join(format!("yp-mpv-{}.sock", std::process::id()));
    let socket_path_str = socket_path.to_str().context("Temp dir path is not valid UTF-8")?.to_string();
    // Remove stale socket if it exists from a previous crash.
    let _ = std::fs::remove_file(&socket_path);

    let mut cmd = Command::new("mpv");
    cmd.args([
      "--no-video",
      "--term-status-msg=Time: ${time-pos/full} / ${duration/full} | Title: ${media-title} | ${pause} ${percent-pos}%",
      &format!("--input-ipc-server={}", socket_path_str),
      &details.url,
    ]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    // Send stderr to null — if piped but never drained, the pipe buffer
    // fills and mpv blocks.
    cmd.stderr(Stdio::null());

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
    self.ipc_socket_path = Some(socket_path_str);
    Ok(())
  }

  pub async fn toggle_pause(&mut self) -> Result<()> {
    let Some(ref socket_path) = self.ipc_socket_path else {
      return Ok(());
    };
    let stream = tokio::net::UnixStream::connect(socket_path).await.context("Failed to connect to mpv IPC socket")?;
    stream.writable().await.context("mpv IPC socket not writable")?;
    let cmd = b"{\"command\":[\"cycle\",\"pause\"]}\n";
    let written = stream.try_write(cmd).context("Failed to send pause command to mpv")?;
    if written < cmd.len() {
      return Err(anyhow!("Partial write to mpv IPC socket: wrote {} of {} bytes", written, cmd.len()));
    }
    self.paused = !self.paused;
    Ok(())
  }

  /// Query mpv's IPC socket for the resolved audio stream URL.
  ///
  /// mpv resolves the YouTube URL to a direct CDN stream URL on startup.
  /// We can reuse that URL to download audio with ffmpeg, skipping the
  /// slow yt-dlp URL resolution step entirely.
  #[allow(dead_code)]
  pub async fn get_stream_url(&self) -> Result<Option<String>> {
    let Some(ref socket_path) = self.ipc_socket_path else {
      return Ok(None);
    };

    let mut stream =
      tokio::net::UnixStream::connect(socket_path).await.context("Failed to connect to mpv IPC socket")?;

    // Ask mpv for the resolved stream URL. `stream-open-filename` gives
    // the URL that mpv's demuxer actually opened (i.e. the CDN URL).
    let cmd = b"{\"command\":[\"get_property\",\"stream-open-filename\"],\"request_id\":1}\n";
    stream.write_all(cmd).await.context("Failed to send get_property to mpv IPC")?;

    // Read lines until we find our response (request_id == 1).
    let reader = TokioBufReader::new(stream);
    let mut lines = reader.lines();

    // mpv may emit event lines before our response; read up to 20 lines.
    for _ in 0..20 {
      let line = tokio::time::timeout(std::time::Duration::from_secs(3), lines.next_line())
        .await
        .context("Timeout waiting for mpv IPC response")?
        .context("Failed to read from mpv IPC socket")?;

      let Some(line) = line else { break };

      // Parse JSON response: {"data":"https://...","request_id":1,"error":"success"}
      if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line)
        && val.get("request_id").and_then(|v| v.as_i64()) == Some(1)
      {
        if val.get("error").and_then(|v| v.as_str()) == Some("success")
          && let Some(url) = val.get("data").and_then(|v| v.as_str())
        {
          return Ok(Some(url.to_string()));
        }
        // Property exists but returned an error or non-string data
        return Ok(None);
      }
    }

    Ok(None)
  }

  pub async fn stop(&mut self) -> Result<()> {
    if let Some(handle) = self.mpv_monitor_handle.take() {
      handle.abort();
      let _ = handle.await;
    }
    self.mpv_status_rx = None;
    self.last_mpv_status = None;

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
