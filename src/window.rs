//! macOS window geometry manipulation via `osascript`.
//!
//! Provides query/set operations for the frontmost terminal window position and size.
//! Uses terminal-specific AppleScript (Terminal.app, iTerm2) when possible to avoid
//! requiring Accessibility permissions, falling back to System Events for other terminals.

use anyhow::{Context, Result, anyhow};
use tracing::{info, warn};

/// Window geometry in pixels: position (x, y) and size (width, height).
#[derive(Debug, Clone, Copy)]
pub struct WindowGeometry {
  pub x: i32,
  pub y: i32,
  pub width: u32,
  pub height: u32,
}

/// Screen dimensions in pixels.
#[derive(Debug, Clone, Copy)]
pub struct ScreenSize {
  pub width: u32,
  pub height: u32,
}

/// Detected terminal application.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TerminalApp {
  AppleTerminal,
  ITerm2,
  Other,
}

fn detect_terminal() -> TerminalApp {
  match std::env::var("TERM_PROGRAM").as_deref() {
    Ok("Apple_Terminal") => TerminalApp::AppleTerminal,
    Ok("iTerm.app") => TerminalApp::ITerm2,
    _ => TerminalApp::Other,
  }
}

/// Run an osascript command and return trimmed stdout.
async fn run_osascript(script: &str) -> Result<String> {
  let output = tokio::process::Command::new("osascript")
    .args(["-e", script])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .output()
    .await
    .context("Failed to run osascript")?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Err(anyhow!("osascript failed: {}", stderr.trim()));
  }

  Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parse "x, y, w, h" or "x, y, right, bottom" from osascript output.
fn parse_bounds(s: &str) -> Result<(i32, i32, i32, i32)> {
  let parts: Vec<&str> = s.split(',').map(|p| p.trim()).collect();
  if parts.len() != 4 {
    return Err(anyhow!("Expected 4 comma-separated values, got {}: {}", parts.len(), s));
  }
  let a: i32 = parts[0].parse().context("Failed to parse first value")?;
  let b: i32 = parts[1].parse().context("Failed to parse second value")?;
  let c: i32 = parts[2].parse().context("Failed to parse third value")?;
  let d: i32 = parts[3].parse().context("Failed to parse fourth value")?;
  Ok((a, b, c, d))
}

/// Query the current window geometry of the frontmost terminal window.
pub async fn get_window_geometry() -> Result<WindowGeometry> {
  let terminal = detect_terminal();
  info!(terminal = ?terminal, "pip: querying window geometry");

  let output = match terminal {
    // Terminal.app and iTerm2: `bounds` returns {left, top, right, bottom}.
    // No Accessibility permissions needed — uses the app's own scripting interface.
    TerminalApp::AppleTerminal => {
      run_osascript(
        r#"tell application "Terminal"
          set b to bounds of front window
          return (item 1 of b as text) & "," & (item 2 of b as text) & "," & (item 3 of b as text) & "," & (item 4 of b as text)
        end tell"#,
      )
      .await?
    }
    TerminalApp::ITerm2 => {
      run_osascript(
        r#"tell application "iTerm2"
          tell current window
            set b to bounds
            return (item 1 of b as text) & "," & (item 2 of b as text) & "," & (item 3 of b as text) & "," & (item 4 of b as text)
          end tell
        end tell"#,
      )
      .await?
    }
    // Generic fallback via System Events — requires Accessibility permissions.
    // Returns {x, y, width, height} (not bounds).
    TerminalApp::Other => {
      run_osascript(
        r#"tell application "System Events"
          tell (first application process whose frontmost is true)
            tell front window
              set {x, y} to position
              set {w, h} to size
              return (x as text) & "," & (y as text) & "," & (w as text) & "," & (h as text)
            end tell
          end tell
        end tell"#,
      )
      .await?
    }
  };

  let (a, b, c, d) = parse_bounds(&output)?;

  let geom = match terminal {
    // bounds format: left, top, right, bottom → convert to x, y, width, height
    TerminalApp::AppleTerminal | TerminalApp::ITerm2 => {
      WindowGeometry { x: a, y: b, width: (c - a).max(0) as u32, height: (d - b).max(0) as u32 }
    }
    // System Events returns position + size directly
    TerminalApp::Other => WindowGeometry { x: a, y: b, width: c.max(0) as u32, height: d.max(0) as u32 },
  };

  info!(geom = ?geom, "pip: current window geometry");
  Ok(geom)
}

/// Set the window geometry of the frontmost terminal window.
pub async fn set_window_geometry(geom: &WindowGeometry) -> Result<()> {
  let terminal = detect_terminal();
  info!(terminal = ?terminal, geom = ?geom, "pip: setting window geometry");

  match terminal {
    TerminalApp::AppleTerminal => {
      let script = format!(
        r#"tell application "Terminal"
          set bounds of front window to {{{}, {}, {}, {}}}
        end tell"#,
        geom.x,
        geom.y,
        geom.x + geom.width as i32,
        geom.y + geom.height as i32
      );
      run_osascript(&script).await?;
    }
    TerminalApp::ITerm2 => {
      let script = format!(
        r#"tell application "iTerm2"
          tell current window
            set bounds to {{{}, {}, {}, {}}}
          end tell
        end tell"#,
        geom.x,
        geom.y,
        geom.x + geom.width as i32,
        geom.y + geom.height as i32
      );
      run_osascript(&script).await?;
    }
    TerminalApp::Other => {
      let script = format!(
        r#"tell application "System Events"
          tell (first application process whose frontmost is true)
            tell front window
              set position to {{{}, {}}}
              set size to {{{}, {}}}
            end tell
          end tell
        end tell"#,
        geom.x, geom.y, geom.width, geom.height
      );
      run_osascript(&script).await?;
    }
  }

  Ok(())
}

/// Get the main screen dimensions in pixels using AppKit via JXA (JavaScript for Automation).
pub async fn get_screen_size() -> Result<ScreenSize> {
  let output = run_osascript_jxa(
    "ObjC.import('AppKit'); var f = $.NSScreen.mainScreen.frame; Math.floor(f.size.width) + ',' + Math.floor(f.size.height)",
  )
  .await?;

  let parts: Vec<&str> = output.split(',').collect();
  if parts.len() != 2 {
    return Err(anyhow!("Expected 2 values from screen size query, got: {}", output));
  }
  let width: u32 = parts[0].trim().parse().context("Failed to parse screen width")?;
  let height: u32 = parts[1].trim().parse().context("Failed to parse screen height")?;

  info!(width, height, "pip: screen size");
  Ok(ScreenSize { width, height })
}

/// Run a JXA (JavaScript for Automation) script via osascript.
async fn run_osascript_jxa(script: &str) -> Result<String> {
  let output = tokio::process::Command::new("osascript")
    .args(["-l", "JavaScript", "-e", script])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .output()
    .await
    .context("Failed to run osascript (JXA)")?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Err(anyhow!("osascript JXA failed: {}", stderr.trim()));
  }

  Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// PiP window dimensions in pixels.
const PIP_WIDTH: u32 = 550;
const PIP_HEIGHT: u32 = 350;
/// Margin from screen edge in pixels.
const PIP_MARGIN: u32 = 30;

/// Compute the PiP window geometry: small window at the bottom-right of the screen.
pub async fn pip_geometry() -> Result<WindowGeometry> {
  let screen = get_screen_size().await.unwrap_or_else(|e| {
    warn!(err = %e, "pip: failed to get screen size, using 2560x1440 default");
    ScreenSize { width: 2560, height: 1440 }
  });

  Ok(WindowGeometry {
    x: (screen.width.saturating_sub(PIP_WIDTH + PIP_MARGIN)) as i32,
    y: (screen.height.saturating_sub(PIP_HEIGHT + PIP_MARGIN)) as i32,
    width: PIP_WIDTH,
    height: PIP_HEIGHT,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_bounds_valid() {
    let (a, b, c, d) = parse_bounds("100, 200, 900, 700").unwrap();
    assert_eq!((a, b, c, d), (100, 200, 900, 700));
  }

  #[test]
  fn parse_bounds_no_spaces() {
    let (a, b, c, d) = parse_bounds("0,0,800,600").unwrap();
    assert_eq!((a, b, c, d), (0, 0, 800, 600));
  }

  #[test]
  fn parse_bounds_negative() {
    let (a, b, c, d) = parse_bounds("-100, 50, 800, 600").unwrap();
    assert_eq!((a, b, c, d), (-100, 50, 800, 600));
  }

  #[test]
  fn parse_bounds_wrong_count() {
    assert!(parse_bounds("100, 200, 300").is_err());
    assert!(parse_bounds("100, 200, 300, 400, 500").is_err());
  }

  #[test]
  fn parse_bounds_non_numeric() {
    assert!(parse_bounds("abc, 200, 300, 400").is_err());
  }
}
