mod app;
mod config;
mod constants;
mod display;
mod graphics;
mod input;
mod player;
mod theme;
mod transcript;
mod ui;
mod window;
mod youtube;

use anyhow::{Context, Result};
use clap::Parser;
use ratatui::{
  DefaultTerminal,
  crossterm::event::{self, Event, KeyEventKind},
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
}

// --- Helpers ---

/// Parse the time position (in seconds) from an mpv status string.
///
/// Expects format: `Time: MM:SS / ... ` or `Time: H:MM:SS / ...`
pub fn parse_mpv_time_secs(status: &str) -> Option<f64> {
  let time_part = status.strip_prefix("Time: ")?.split(" / ").next()?.trim();
  let parts: Vec<&str> = time_part.split(':').collect();
  match parts.len() {
    2 => {
      let m: f64 = parts[0].parse().ok()?;
      let s: f64 = parts[1].parse().ok()?;
      Some(m * 60.0 + s)
    }
    3 => {
      let h: f64 = parts[0].parse().ok()?;
      let m: f64 = parts[1].parse().ok()?;
      let s: f64 = parts[2].parse().ok()?;
      Some(h * 3600.0 + m * 60.0 + s)
    }
    _ => None,
  }
}

// --- Main ---

#[tokio::main]
async fn main() -> Result<()> {
  // --- Daily file logging ---
  let log_dir = directories::BaseDirs::new()
    .map(|d| d.data_dir().join("yp/logs"))
    .unwrap_or_else(|| std::path::PathBuf::from("/tmp/yp/logs"));
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

  let default_hook = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    ratatui::restore();
    default_hook(info);
  }));

  let mut terminal = ratatui::init();
  let result = run(&mut terminal, args).await;
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
    app.player.check_mpv_status();
    app.expire_error();

    // Update frame source image if available and time position changed
    if let Some(frame_source) = app.frame_source()
      && let Some(status) = app.player.get_last_mpv_status()
      && let Some(time_secs) = parse_mpv_time_secs(&status)
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

    if event::poll(Duration::from_millis(100)).context("Failed to poll for terminal events")? {
      match event::read().context("Failed to read terminal event")? {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
          input::handle_key_event(&mut app, key).await.context("Failed to handle key event")?;
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

  // --- parse_mpv_time_secs ---

  #[test]
  fn parse_mpv_time_mm_ss() {
    let status = "Time: 01:30 / 04:00 | Title: Song | no 37%";
    assert_eq!(parse_mpv_time_secs(status), Some(90.0));
  }

  #[test]
  fn parse_mpv_time_h_mm_ss() {
    let status = "Time: 1:02:03 / 2:00:00 | Title: Song | no 51%";
    assert_eq!(parse_mpv_time_secs(status), Some(3723.0));
  }

  #[test]
  fn parse_mpv_time_zero() {
    let status = "Time: 00:00 / 03:45 | Title: Song | no 0%";
    assert_eq!(parse_mpv_time_secs(status), Some(0.0));
  }

  #[test]
  fn parse_mpv_time_no_prefix() {
    assert_eq!(parse_mpv_time_secs("Something else"), None);
  }

  #[test]
  fn parse_mpv_time_garbage() {
    assert_eq!(parse_mpv_time_secs("Time: abc / def"), None);
  }
}
