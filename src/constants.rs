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

  // UI
  pub max_display_tags: usize,

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
  pub enrich_limit: usize,
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn enrichment_is_bounded_to_avoid_rate_limits() {
    // Each video is one yt-dlp request. YouTube throttles bursts well below
    // raw throughput, so both the width and the total are guardrails rather
    // than tuning knobs.
    let c = constants();
    assert!(c.enrich_concurrency > 0, "enrichment must make progress");
    assert!(c.enrich_concurrency <= 4, "concurrency {} invites throttling", c.enrich_concurrency);
    assert!(c.enrich_limit > 0, "enrichment must not be disabled outright");
    assert!(c.enrich_limit <= 100, "a limit of {} means too many requests", c.enrich_limit);
  }

  #[test]
  fn a_page_load_stays_within_a_sane_request_budget() {
    // The worst case is a full page of fresh results being enriched at once.
    let c = constants();
    let budget = c.enrich_limit.min(c.channel_initial_size);
    assert!(budget <= 60, "a single page load may issue {budget} requests");
  }

  #[test]
  fn print_formats_request_the_fields_the_ui_reads() {
    let c = constants();
    for field in ["%(title)s", "%(id)s", "%(duration_string)s", "%(uploader)s"] {
      assert!(c.print_format.contains(field), "print_format is missing {field}");
    }
    // Enrichment runs after a listing, so it re-requests less.
    assert!(c.enrich_format.contains("%(tags)s"), "tags are the reason to enrich");
    assert!(!c.enrich_format.contains("%(title)s"), "enrichment must not refetch the title");
  }
}
