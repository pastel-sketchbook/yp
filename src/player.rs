use anyhow::{Context, Result};
use image::DynamicImage;
use reqwest::Client;
use std::sync::Arc;

use crate::{
  audio::{self, AudioOutput, Decoder},
  display::DisplayMode,
  spectrum::Spectrum,
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct VideoDetails {
  pub url: String,
  pub title: String,
  pub uploader: Option<String>,
  pub duration: Option<String>,
  pub upload_date: Option<String>,
  pub view_count: Option<String>,
  pub tags: Vec<String>,
  /// YouTube's own categories, used to skip transcription for music.
  pub categories: Vec<String>,
}

pub struct MusicPlayer {
  pub http_client: Client,
  pub display_mode: DisplayMode,
  pub current_details: Option<VideoDetails>,
  pub cached_thumbnail: Option<(String, DynamicImage)>,
  /// mpv decoding into the FIFO. `None` when nothing is playing.
  decoder: Option<Decoder>,
  /// Device playback, owned by the player rather than by mpv.
  audio: Option<AudioOutput>,
  /// Shared with the analyzer thread; kept alive for the player's lifetime.
  spectrum: Option<Arc<Spectrum>>,
  pub paused: bool,
  /// Test-only override so UI code paths that depend on playback can be
  /// exercised without a live mpv decoder and audio device.
  #[cfg(test)]
  playing_override: bool,
}

impl MusicPlayer {
  pub fn new(display_mode: DisplayMode) -> Self {
    Self {
      http_client: Client::new(),
      display_mode,
      current_details: None,
      cached_thumbnail: None,
      decoder: None,
      audio: None,
      spectrum: None,
      paused: false,
      #[cfg(test)]
      playing_override: false,
    }
  }

  pub fn is_playing(&self) -> bool {
    #[cfg(test)]
    if self.playing_override {
      return true;
    }
    self.decoder.is_some()
  }

  /// Pretends a track is playing. Test-only: the playback paths need a live
  /// decoder and an audio device, neither of which belongs in a unit test.
  #[cfg(test)]
  pub fn set_playing_for_test(&mut self, playing: bool) {
    self.playing_override = playing;
  }

  /// The spectrum source, created on first playback.
  pub fn spectrum(&mut self) -> Result<Option<Arc<Spectrum>>> {
    if self.spectrum.is_none() {
      self.spectrum = Some(Spectrum::start(audio::SAMPLE_RATE).context("Starting the spectrum analyzer")?);
    }
    Ok(self.spectrum.clone())
  }

  /// Playback position in seconds, from the samples the device has consumed.
  pub fn position_secs(&self) -> Option<f64> {
    self.audio.as_ref().map(AudioOutput::position_secs)
  }

  /// True once the device has drained everything mpv decoded.
  pub fn finished(&self) -> bool {
    self.audio.as_ref().is_some_and(AudioOutput::finished)
  }

  /// mpv's IPC socket, used by the transcription pipeline to discover the
  /// stream URL without a second `yt-dlp` call.
  pub fn ipc_socket_path(&self) -> Option<String> {
    self.decoder.as_ref().map(|decoder| decoder.socket_path().display().to_string())
  }

  pub async fn play(&mut self, details: VideoDetails) -> Result<()> {
    self.stop().await.context("Failed to stop previous playback")?;
    self.current_details = Some(details.clone());
    self.paused = false;

    let spectrum = self.spectrum()?.context("Spectrum unavailable")?;

    // Open the device before spawning mpv: if no device exists, failing here
    // leaves nothing to clean up, whereas failing afterwards would orphan a
    // decoding process.
    let (player, sink) = AudioOutput::open()?;

    let mut decoder = Decoder::spawn(&details.url, None, std::process::id()).await?;
    let reader = decoder.take_reader().context("mpv did not open the PCM FIFO")?;
    spectrum.context(Some(&details.url));

    let audio = AudioOutput::start(reader, player, sink, spectrum.clone())?;
    spectrum.set_playing(true);

    self.decoder = Some(decoder);
    self.audio = Some(audio);
    Ok(())
  }

  /// Restarts playback at `secs`. A FIFO cannot be rewound, so seeking means
  /// respawning mpv against the same URL.
  pub async fn seek(&mut self, secs: f64) -> Result<()> {
    let Some(details) = self.current_details.clone() else {
      return Ok(());
    };
    let spectrum = self.spectrum()?.context("Spectrum unavailable")?;
    let was_paused = self.paused;

    self.teardown().await;
    let (player, sink) = AudioOutput::open()?;
    let mut decoder = Decoder::spawn(&details.url, Some(secs.max(0.0)), std::process::id()).await?;
    let reader = decoder.take_reader().context("mpv did not open the PCM FIFO")?;
    let audio = AudioOutput::start(reader, player, sink, spectrum.clone())?;
    // Drop anything the previous device had queued but not yet played, or it
    // would bleed the old position's audio into the new one.
    audio.clear_queue();
    audio.set_paused(was_paused);
    spectrum.set_playing(!was_paused);

    self.decoder = Some(decoder);
    self.audio = Some(audio);
    Ok(())
  }

  pub async fn toggle_pause(&mut self) -> Result<()> {
    let Some(audio) = &self.audio else {
      return Ok(());
    };
    audio.set_paused(!self.paused);
    self.paused = !self.paused;
    if let Some(spectrum) = &self.spectrum {
      spectrum.set_playing(!self.paused);
    }
    Ok(())
  }

  pub async fn stop(&mut self) -> Result<()> {
    self.teardown().await;
    self.current_details = None;
    self.cached_thumbnail = None;
    Ok(())
  }

  /// Drops playback without clearing metadata, so a seek can rebuild on top.
  async fn teardown(&mut self) {
    if let Some(spectrum) = &self.spectrum {
      spectrum.set_playing(false);
      spectrum.context(None);
    }
    // Kill the decoder before dropping the device, otherwise mpv can block
    // writing into a FIFO nobody is draining.
    if let Some(mut decoder) = self.decoder.take() {
      let _ = decoder.child.kill().await;
      let _ = decoder.child.wait().await;
    }
    self.audio = None;
    self.paused = false;
  }
}
