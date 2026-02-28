//! Application constants loaded from `constants.ron` at compile time.
//!
//! The RON file is embedded via `include_str!` so it's always available —
//! no runtime file I/O. Parsed once on first access via `LazyLock`.

use serde::Deserialize;
use std::sync::LazyLock;

/// All tuneable application constants.
#[derive(Debug, Deserialize)]
pub struct Constants {
  pub pastel_sketchbook_channel: String,

  // Ghostty terminal
  pub ghostty_term_program: String,
  pub ghostty_process_name: String,

  // PiP window
  pub pip_width: u32,
  pub pip_height: u32,
  pub pip_margin: u32,

  // Channel browsing
  pub channel_initial_size: usize,
  pub channel_page_size: usize,

  // Transcription
  pub chunk_secs: u32,
  pub min_chunk_bytes: u64,

  // YouTube / yt-dlp
  pub frame_extract_fps: f64,
  pub frame_extract_width: u32,
  pub enrich_concurrency: usize,
  pub print_format: String,
  pub enrich_format: String,
}

static CONSTANTS: LazyLock<Constants> = LazyLock::new(|| {
  // Safety: the RON file is embedded at compile time; if it's malformed this is a build-time error.
  ron::from_str(include_str!("../constants.ron")).expect("constants.ron must be valid RON (embedded at compile time)")
});

/// Returns a reference to the parsed application constants.
pub fn constants() -> &'static Constants {
  &CONSTANTS
}
