mod app;
mod audio;
mod cache;
mod cli;
mod config;
mod constants;
mod display;
mod graphics;
mod input;
mod player;
mod spectrum;
mod spectrum_view;
mod summarize;
mod theme;
mod transcript;
mod ui;
mod wiki;
mod window;
mod youtube;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use ratatui::{
  DefaultTerminal,
  crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
  },
};
use std::time::Duration;
use tracing::info;

use app::App;
use display::{CliDisplayMode, DisplayMode};
use graphics::{kitty_delete_all, kitty_delete_placement, kitty_render_image, sixel_render_image};

// --- CLI ---

#[derive(Parser, Debug)]
#[command(author, version = env!("CARGO_PKG_VERSION"), about, long_about = None)]
struct Args {
  /// Display mode: 'auto', 'kitty', 'sixel', 'direct', or 'ascii' (default: auto-detect)
  #[arg(short, long, default_value = "auto")]
  display_mode: CliDisplayMode,

  #[command(subcommand)]
  command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
  /// Generate shell completions for bash, zsh, fish, elvish, or powershell
  Completions {
    /// The shell to generate completions for
    shell: Shell,
  },

  /// Search `YouTube` and return results as JSON
  Search {
    /// Search query
    query: String,
    /// Max results (default: 20)
    #[arg(short, long, default_value_t = 20)]
    limit: usize,
  },

  /// List videos from a `YouTube` channel (output as JSONL)
  Channel {
    /// Channel handle (@name), URL, or name (default: @ChrisH-v4e)
    channel: Option<String>,
    /// Max videos to list (default: 30, use --all for no limit)
    #[arg(short, long, default_value_t = 30)]
    limit: usize,
    /// Fetch all videos from the channel (overrides --limit)
    #[arg(short, long)]
    all: bool,
    /// Enrich videos with tags via per-video yt-dlp calls (slower)
    #[arg(short, long)]
    enrich: bool,
    /// Number of concurrent enrichment processes (default: 8)
    #[arg(short, long, default_value_t = 8)]
    jobs: usize,
  },

  /// Fetch metadata for a specific video (output as JSON)
  Info {
    /// Video ID or `YouTube` URL
    video: String,
  },

  /// Transcribe a video and return utterances (output as JSONL)
  Transcript {
    /// Video ID or `YouTube` URL (reads from stdin if omitted)
    video: Option<String>,
    /// Disable classification, output raw utterances
    #[arg(short, long)]
    raw: bool,
  },

  /// Transcribe + classify + reduce a video to a summary (output as JSON)
  Summarize {
    /// Video ID, `YouTube` URL, or channel handle (with --latest).
    /// Defaults to the configured channel when used with --latest.
    video: Option<String>,
    /// Summarize the latest N videos from a channel (default: 1)
    #[arg(long, num_args = 0..=1, default_missing_value = "1")]
    latest: Option<usize>,
    /// Output full unprocessed transcript
    #[arg(short, long)]
    raw: bool,
  },

  /// Output cached video IDs for shell completion (hidden)
  #[command(name = "_complete-ids", hide = true)]
  CompleteIds {
    /// Fetch from default channel if cache is empty
    #[arg(long)]
    live: bool,
  },
}

// --- Helpers ---

/// Parse a duration string into seconds.
///
/// `yt-dlp` reports durations as `MM:SS`, `H:MM:SS`, or with a `h`/`m`/`s`
/// suffix, and occasionally as a bare number of seconds. Returns `None` for
/// anything unrecognizable rather than guessing.
#[must_use]
pub fn parse_duration_secs(duration: &str) -> Option<f64> {
  let trimmed = duration.trim();
  if let Ok(secs) = trimmed.parse::<f64>() {
    return Some(secs);
  }
  // Long form: sum every "<number> <unit>" pair, so "3 minutes, 45 seconds" works.
  let mut total = 0.0;
  let mut matched = false;
  for (number, unit) in trimmed.split(',').filter_map(|part| part.trim().split_once(char::is_whitespace)) {
    let Ok(value) = number.trim().parse::<f64>() else {
      continue;
    };
    let factor = match unit.trim() {
      "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
      "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
      "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
      _ => continue,
    };
    total += value * factor;
    matched = true;
  }
  if matched {
    return Some(total);
  }

  // ISO-8601 form: "PT3M45S" or "PT1H2M3S".
  if let Some(iso) = trimmed.strip_prefix("PT") {
    let mut total = 0.0;
    let mut digits = String::new();
    for c in iso.chars() {
      if c.is_ascii_digit() || c == '.' {
        digits.push(c);
      } else {
        let Ok(value) = digits.parse::<f64>() else {
          return None;
        };
        digits.clear();
        total += match c.to_ascii_uppercase() {
          'H' => value * 3600.0,
          'M' => value * 60.0,
          'S' => value,
          _ => return None,
        };
      }
    }
    return Some(total + digits.parse::<f64>().unwrap_or(0.0));
  }

  // Clock form: "3:45", "1:02:03", optionally with a trailing "s". A bare "45s"
  // has no colon, so treat a trailing unit as seconds.
  let compact = trimmed.trim_end_matches(|c: char| c.is_ascii_alphabetic()).trim();
  if !compact.contains(':') && compact.len() != trimmed.trim().len() {
    return compact.parse::<f64>().ok();
  }
  let clock = compact;
  let parts: Vec<f64> = clock.split(':').map(|part| part.parse::<f64>()).collect::<Result<_, _>>().ok()?;
  match parts.as_slice() {
    [m, s] => Some(m * 60.0 + s),
    [h, m, s] => Some(h * 3600.0 + m * 60.0 + s),
    _ => None,
  }
}

/// Format seconds as `M:SS`, or `H:MM:SS` past an hour.
#[must_use]
pub fn format_time(secs: f64) -> String {
  let total = if secs.is_finite() && secs > 0.0 { secs as u64 } else { 0 };
  let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
  if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

// --- Main ---

#[tokio::main]
async fn main() -> Result<()> {
  // --- Daily file logging ---
  let log_dir = directories::BaseDirs::new()
    .map_or_else(|| std::path::PathBuf::from("/tmp/yp/logs"), |d| d.data_dir().join("yp/logs"));
  let file_appender = tracing_appender::rolling::daily(&log_dir, "yp.log");
  let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
  tracing_subscriber::fmt()
    .with_writer(non_blocking)
    // Safety: "yp=debug" is a valid static tracing directive — parse cannot fail.
    .with_env_filter(
      tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("yp=debug".parse().expect("valid tracing directive")),
    )
    .with_ansi(false)
    .with_target(false)
    .init();

  info!("yp v{} starting", env!("CARGO_PKG_VERSION"));

  // Remove old log files (keep only today's)
  let today = chrono::Local::now().format("%Y-%m-%d").to_string();
  if let Ok(entries) = std::fs::read_dir(&log_dir) {
    for entry in entries.flatten() {
      let name = entry.file_name();
      let name = name.to_string_lossy();
      if name.starts_with("yp.log.") && !name.ends_with(&today) {
        let _ = std::fs::remove_file(entry.path());
      }
    }
  }

  let args = Args::parse();

  // Handle non-TUI subcommands before entering the terminal.
  if let Some(command) = args.command {
    return match command {
      Command::Completions { shell } => {
        if shell == Shell::Zsh {
          // Custom zsh completion with dynamic video ID support.
          cli::generate_zsh_completions();
        } else {
          let mut cmd = Args::command();
          let bin_name = cmd.get_name().to_string();
          generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
        }
        Ok(())
      }
      Command::Search { query, limit } => cli::cmd_search(&query, limit).await,
      Command::Channel { channel, limit, all, enrich, jobs } => {
        let channel = channel.unwrap_or_else(|| constants::constants().pastel_sketchbook_channel.clone());
        let count = if all { None } else { Some(limit) };
        cli::cmd_channel(&channel, count, enrich, jobs).await
      }
      Command::Info { video } => cli::cmd_info(&video).await,
      Command::Transcript { video, raw } => {
        if let Some(video) = video {
          cli::cmd_transcript(&video, raw).await
        } else {
          cli::cmd_transcript_stdin(raw).await
        }
      }
      Command::Summarize { video, latest, raw } => {
        if let Some(count) = latest {
          // --latest: treat `video` as a channel handle, default to configured channel
          let channel = video.unwrap_or_else(|| constants::constants().pastel_sketchbook_channel.clone());
          cli::cmd_summarize_latest(&channel, count, raw).await
        } else if let Some(video) = video {
          cli::cmd_summarize(&video, raw).await
        } else {
          // No video arg and no --latest: read from stdin (pipe mode)
          cli::cmd_summarize_stdin(raw).await
        }
      }
      Command::CompleteIds { live } => cli::cmd_complete_ids(live).await,
    };
  }

  let default_hook = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    ratatui::restore();
    default_hook(info);
  }));

  let mut terminal = ratatui::init();
  execute!(std::io::stdout(), EnableMouseCapture)?;
  let result = run(&mut terminal, args).await;
  execute!(std::io::stdout(), DisableMouseCapture)?;
  ratatui::restore();
  result
}

async fn run(terminal: &mut DefaultTerminal, args: Args) -> Result<()> {
  let display_mode = display::resolve_display_mode(args.display_mode);
  info!(display_mode = ?display_mode, "display mode resolved");
  let mut app = App::new(display_mode);
  let uses_graphics_protocol = matches!(display_mode, DisplayMode::Kitty | DisplayMode::Sixel);

  loop {
    app.check_pending().await.context("Failed to check pending async tasks")?;
    app.check_playback().await;
    app.poll_spectrum();
    app.refresh_spectrum();
    app.expire_error();

    // Update frame source image if available and time position changed
    if let Some(frame_source) = app.frame_source()
      && let Some(time_secs) = app.player.position_secs()
    {
      let idx = frame_source.frame_index_at(time_secs);
      if app.frame_idx() != Some(idx)
        && let Some(frame) = frame_source.frame_at(time_secs)
      {
        let vid = frame_source.video_id().to_string();
        app.player.cached_thumbnail = Some((vid, frame));
        app.gfx.resized_thumb = None;
        app.gfx.last_sent = None;
        app.set_frame_idx(idx);
      }
    }

    terminal.draw(|frame| ui::ui(frame, &mut app)).context("Failed to draw terminal frame")?;

    if uses_graphics_protocol {
      // Wrap graphics protocol output in synchronized update markers so the
      // terminal treats the ratatui cell updates + image data as one atomic
      // frame, preventing visible gaps between cell clear and image render.
      use std::io::Write;
      let mut stdout = std::io::stdout();
      write!(stdout, "\x1B[?2026h").context("Failed to write BeginSynchronizedUpdate")?;
      stdout.flush().context("Failed to flush BeginSynchronizedUpdate")?;

      if let Some(area) = app.gfx.thumb_area {
        if let Some((ref video_id, ref image)) = app.player.cached_thumbnail {
          let key = (video_id.clone(), area);
          if app.gfx.last_sent.as_ref() != Some(&key) {
            // Image ID i=1 with placement p=1 atomically replaces the
            // previous image — no need to delete first.
            match display_mode {
              DisplayMode::Kitty => kitty_render_image(image, area).context("Failed to render kitty thumbnail")?,
              DisplayMode::Sixel => sixel_render_image(image, area).context("Failed to render sixel thumbnail")?,
              _ => {}
            }
            app.gfx.last_sent = Some(key);
          }
        }
      } else if app.gfx.last_sent.is_some() {
        if display_mode == DisplayMode::Kitty {
          kitty_delete_placement().context("Failed to delete kitty image placement")?;
        }
        app.gfx.last_sent = None;
      }

      write!(stdout, "\x1B[?2026l").context("Failed to write EndSynchronizedUpdate")?;
      stdout.flush().context("Failed to flush EndSynchronizedUpdate")?;
    }

    // Poll faster while bars are still settling, otherwise the decay and peak
    // hold animate in visible 100 ms steps.
    let frame_delay =
      if app.spectrum_needs_animation() { Duration::from_millis(16) } else { Duration::from_millis(100) };
    if event::poll(frame_delay).context("Failed to poll for terminal events")? {
      match event::read().context("Failed to read terminal event")? {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
          input::handle_key_event(&mut app, key).await.context("Failed to handle key event")?;
        }
        Event::Mouse(m) => {
          input::handle_mouse_event(&mut app, m);
        }
        _ => {}
      }
    }

    if app.should_quit {
      break;
    }
  }

  if display_mode == DisplayMode::Kitty {
    kitty_delete_all().context("Failed to clean up Kitty graphics on exit")?;
  }
  app.restore_pip().await;
  app.player.stop().await.context("Failed to stop player on exit")?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  // --- format_time ---

  #[test]
  fn format_time_mm_ss() {
    assert_eq!(format_time(90.0), "1:30");
    assert_eq!(format_time(225.0), "3:45");
  }

  #[test]
  fn format_time_h_mm_ss() {
    assert_eq!(format_time(3723.0), "1:02:03");
  }

  #[test]
  fn format_time_zero() {
    assert_eq!(format_time(0.0), "0:00");
  }

  #[test]
  fn format_time_clamps_negative_and_non_finite() {
    // The clock can briefly read backwards across a seek or a slow device.
    assert_eq!(format_time(-1.0), "0:00");
    assert_eq!(format_time(f64::NAN), "0:00");
    assert_eq!(format_time(f64::INFINITY), "0:00");
  }

  // --- parse_duration_secs ---

  #[test]
  fn parse_duration_colon_forms() {
    assert_eq!(parse_duration_secs("3:45"), Some(225.0));
    assert_eq!(parse_duration_secs("1:02:03"), Some(3723.0));
    assert_eq!(parse_duration_secs("  2:30  "), Some(150.0));
  }

  #[test]
  fn parse_duration_yt_dlp_suffix_forms() {
    assert_eq!(parse_duration_secs("3 minutes, 45 seconds"), Some(225.0));
    assert_eq!(parse_duration_secs("PT3M45S"), Some(225.0));
    assert_eq!(parse_duration_secs("45s"), Some(45.0));
  }

  #[test]
  fn parse_duration_accepts_bare_seconds() {
    assert_eq!(parse_duration_secs("225"), Some(225.0));
    assert_eq!(parse_duration_secs("225.5"), Some(225.5));
  }

  #[test]
  fn parse_duration_rejects_nonsense() {
    assert_eq!(parse_duration_secs(""), None);
    assert_eq!(parse_duration_secs("unknown"), None);
    assert_eq!(parse_duration_secs("1:2:3:4"), None);
  }

  #[test]
  fn duration_round_trips_through_the_formatter() {
    for secs in [0.0, 45.0, 225.0, 3723.0] {
      let formatted = format_time(secs);
      assert_eq!(parse_duration_secs(&formatted), Some(secs), "round trip of {secs}");
    }
  }

  #[test]
  fn format_time_truncates_rather_than_rounds_up() {
    // 59.9 must stay under a minute so the progress bar does not jump early.
    assert_eq!(format_time(59.9), "0:59");
    assert_eq!(format_time(3599.9), "59:59");
  }
}
