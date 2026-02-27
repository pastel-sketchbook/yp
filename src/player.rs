use anyhow::{Context, Result, anyhow};
use image::DynamicImage;
use reqwest::Client;
use std::{
  process::Stdio,
  sync::{Arc, Mutex},
};
use tokio::{
  io::AsyncBufReadExt,
  io::BufReader as TokioBufReader,
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
  last_mpv_status: Arc<Mutex<Option<String>>>,
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
      last_mpv_status: Arc::new(Mutex::new(None)),
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
        // safety: mutex is only locked briefly and we never panic while holding it
        let mut last_status = self.last_mpv_status.lock().expect("mpv status mutex poisoned");
        *last_status = Some(status);
      }
    }
  }

  pub fn get_last_mpv_status(&self) -> Option<String> {
    // safety: mutex is only locked briefly and we never panic while holding it
    self.last_mpv_status.lock().expect("mpv status mutex poisoned").clone()
  }

  pub async fn play(&mut self, details: VideoDetails) -> Result<()> {
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

  pub async fn toggle_pause(&mut self) -> Result<()> {
    let Some(ref socket_path) = self.ipc_socket_path else {
      return Ok(());
    };
    let stream = tokio::net::UnixStream::connect(socket_path).await.context("Failed to connect to mpv IPC socket")?;
    stream.writable().await.context("mpv IPC socket not writable")?;
    stream.try_write(b"{\"command\":[\"cycle\",\"pause\"]}\n").context("Failed to send pause command to mpv")?;
    self.paused = !self.paused;
    Ok(())
  }

  pub async fn stop(&mut self) -> Result<()> {
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
