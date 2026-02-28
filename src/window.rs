//! macOS window geometry manipulation.
//!
//! Provides query/set operations for the frontmost terminal window position and size.
//! Uses terminal-specific AppleScript (Terminal.app, iTerm2) or generic System Events
//! AppleScript (Ghostty, and potentially other terminals). PiP is only supported on
//! Terminal.app, iTerm2, and Ghostty.
//!
//! For Ghostty, we use `System Events` to get/set the window position and size. This
//! requires macOS Automation permission: the first time it runs, macOS may prompt the
//! user to grant permission. If denied, the error is detected and an actionable message
//! is shown guiding the user to System Settings → Privacy & Security → Automation.

use anyhow::{Context, Result, anyhow};
use tracing::{info, warn};

use crate::constants::constants;

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
  /// Ghostty terminal (TERM_PROGRAM=ghostty).
  Ghostty,
  /// Any other terminal — PiP not supported.
  Other,
}

fn detect_terminal() -> TerminalApp {
  let c = constants();
  match std::env::var("TERM_PROGRAM").as_deref() {
    Ok("Apple_Terminal") => TerminalApp::AppleTerminal,
    Ok("iTerm.app") => TerminalApp::ITerm2,
    Ok(s) if s == c.ghostty_term_program => TerminalApp::Ghostty,
    _ => TerminalApp::Other,
  }
}

/// Returns `true` if the current terminal supports PiP window manipulation.
///
/// Only Terminal.app, iTerm2, and Ghostty are supported. Other terminals
/// (Alacritty, kitty, tmux, etc.) are not — PiP keybinding and footer hint
/// should be hidden when this returns `false`.
pub fn pip_supported() -> bool {
  detect_terminal() != TerminalApp::Other
}

// ---------------------------------------------------------------------------
// osascript helpers
// ---------------------------------------------------------------------------

/// Check if an osascript error is the macOS Automation permission denial (-1743).
///
/// Error -1743 means the calling app is not authorized to send Apple events to the
/// target app (System Events). The user must grant permission in System Settings →
/// Privacy & Security → Automation.
fn is_automation_permission_error(stderr: &str) -> bool {
  stderr.contains("-1743") || stderr.contains("not allowed assistive access")
}

/// Wraps an osascript error with an actionable permission message when the failure
/// is due to missing macOS Automation permission.
fn annotate_permission_error(stderr: &str) -> anyhow::Error {
  if is_automation_permission_error(stderr) {
    warn!(
      "macOS Automation permission denied. Grant in: \
       System Settings → Privacy & Security → Automation → enable \"System Events\" for your terminal"
    );
    anyhow!("Permission denied — enable Automation for System Events in System Settings → Privacy & Security")
  } else {
    anyhow!("osascript failed: {}", stderr.trim())
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
    return Err(annotate_permission_error(&stderr));
  }

  Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
    return Err(annotate_permission_error(&stderr));
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

/// Parse "x, y" from osascript System Events position output.
fn parse_position(s: &str) -> Result<(i32, i32)> {
  let parts: Vec<&str> = s.split(',').map(|p| p.trim()).collect();
  if parts.len() != 2 {
    return Err(anyhow!("Expected 2 comma-separated values for position, got {}: {}", parts.len(), s));
  }
  let x: i32 = parts[0].parse().context("Failed to parse x position")?;
  let y: i32 = parts[1].parse().context("Failed to parse y position")?;
  Ok((x, y))
}

/// Parse "w, h" from osascript System Events size output.
fn parse_size(s: &str) -> Result<(u32, u32)> {
  let parts: Vec<&str> = s.split(',').map(|p| p.trim()).collect();
  if parts.len() != 2 {
    return Err(anyhow!("Expected 2 comma-separated values for size, got {}: {}", parts.len(), s));
  }
  let w: u32 = parts[0].parse().context("Failed to parse width")?;
  let h: u32 = parts[1].parse().context("Failed to parse height")?;
  Ok((w, h))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check if the current window appears to be in fullscreen mode by comparing
/// its size to the screen size. Allows some tolerance for menu bar / dock.
pub fn is_likely_fullscreen(geom: &WindowGeometry, screen: &ScreenSize) -> bool {
  let w_ratio = geom.width as f64 / screen.width as f64;
  let h_ratio = geom.height as f64 / screen.height as f64;
  w_ratio > 0.95 && h_ratio > 0.90
}

/// Query the AXFullScreen attribute of the frontmost Ghostty window.
///
/// Returns `true` if the window is in macOS native fullscreen. Returns `false`
/// for non-Ghostty terminals or if the query fails (conservative default).
pub async fn is_native_fullscreen() -> bool {
  if detect_terminal() != TerminalApp::Ghostty {
    return false;
  }

  let script = format!(
    r#"tell application "System Events"
      tell process "{name}"
        get value of attribute "AXFullScreen" of front window
      end tell
    end tell"#,
    name = constants().ghostty_process_name
  );

  match run_osascript(&script).await {
    Ok(val) => {
      let is_fs = val.trim().eq_ignore_ascii_case("true");
      info!(ax_fullscreen = is_fs, "pip: queried AXFullScreen attribute");
      is_fs
    }
    Err(e) => {
      warn!(err = %e, "pip: failed to query AXFullScreen, assuming not fullscreen");
      false
    }
  }
}

/// Exit macOS native fullscreen for the frontmost Ghostty window.
///
/// Uses System Events AppleScript to set the "AXFullScreen" attribute to false.
/// No-op for non-Ghostty terminals (they don't typically run fullscreen, or
/// handle it differently).
pub async fn exit_fullscreen() -> Result<()> {
  let terminal = detect_terminal();
  if terminal != TerminalApp::Ghostty {
    return Ok(());
  }

  info!("pip: exiting Ghostty fullscreen via System Events");
  let script = format!(
    r#"tell application "System Events"
      tell process "{name}"
        set value of attribute "AXFullScreen" of front window to false
      end tell
    end tell"#,
    name = constants().ghostty_process_name
  );
  run_osascript(&script).await.context("Failed to exit fullscreen via System Events")?;

  Ok(())
}

/// Enter macOS native fullscreen for the frontmost Ghostty window.
///
/// Uses System Events AppleScript to set the "AXFullScreen" attribute to true.
/// No-op for non-Ghostty terminals.
pub async fn enter_fullscreen() -> Result<()> {
  let terminal = detect_terminal();
  if terminal != TerminalApp::Ghostty {
    return Ok(());
  }

  info!("pip: entering Ghostty fullscreen via System Events");
  let script = format!(
    r#"tell application "System Events"
      tell process "{name}"
        set value of attribute "AXFullScreen" of front window to true
      end tell
    end tell"#,
    name = constants().ghostty_process_name
  );
  run_osascript(&script).await.context("Failed to enter fullscreen via System Events")?;

  Ok(())
}

/// Query the current window geometry of the frontmost terminal window.
pub async fn get_window_geometry() -> Result<WindowGeometry> {
  let terminal = detect_terminal();
  info!(terminal = ?terminal, "pip: querying window geometry");

  match terminal {
    TerminalApp::AppleTerminal => {
      let output = run_osascript(
        r#"tell application "Terminal"
          set b to bounds of front window
          return (item 1 of b as text) & "," & (item 2 of b as text) & "," & (item 3 of b as text) & "," & (item 4 of b as text)
        end tell"#,
      )
      .await?;
      let (a, b, c, d) = parse_bounds(&output)?;
      let geom = WindowGeometry { x: a, y: b, width: (c - a).max(0) as u32, height: (d - b).max(0) as u32 };
      info!(geom = ?geom, "pip: current window geometry (AppleTerminal)");
      Ok(geom)
    }
    TerminalApp::ITerm2 => {
      let output = run_osascript(
        r#"tell application "iTerm2"
          tell current window
            set b to bounds
            return (item 1 of b as text) & "," & (item 2 of b as text) & "," & (item 3 of b as text) & "," & (item 4 of b as text)
          end tell
        end tell"#,
      )
      .await?;
      let (a, b, c, d) = parse_bounds(&output)?;
      let geom = WindowGeometry { x: a, y: b, width: (c - a).max(0) as u32, height: (d - b).max(0) as u32 };
      info!(geom = ?geom, "pip: current window geometry (iTerm2)");
      Ok(geom)
    }
    // Ghostty: use System Events (macOS Accessibility API) to query window geometry.
    // This doesn't require any special Ghostty config.
    TerminalApp::Ghostty => {
      let name = &constants().ghostty_process_name;
      let pos_script = format!(
        r#"tell application "System Events"
          tell process "{name}"
            set p to position of front window
            return (item 1 of p as text) & "," & (item 2 of p as text)
          end tell
        end tell"#,
      );
      let pos_output =
        run_osascript(&pos_script).await.context("Failed to get Ghostty window position via System Events")?;

      let size_script = format!(
        r#"tell application "System Events"
          tell process "{name}"
            set s to size of front window
            return (item 1 of s as text) & "," & (item 2 of s as text)
          end tell
        end tell"#,
      );
      let size_output =
        run_osascript(&size_script).await.context("Failed to get Ghostty window size via System Events")?;

      let (x, y) = parse_position(&pos_output)?;
      let (width, height) = parse_size(&size_output)?;
      let geom = WindowGeometry { x, y, width, height };
      info!(geom = ?geom, "pip: current window geometry (Ghostty)");
      Ok(geom)
    }
    TerminalApp::Other => {
      Err(anyhow!("PiP is not supported in this terminal (TERM_PROGRAM={:?})", std::env::var("TERM_PROGRAM").ok()))
    }
  }
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
    // Ghostty: use System Events to set position and size.
    TerminalApp::Ghostty => {
      let script = format!(
        r#"tell application "System Events"
          tell process "{name}"
            set position of front window to {{{x}, {y}}}
            set size of front window to {{{w}, {h}}}
          end tell
        end tell"#,
        name = constants().ghostty_process_name,
        x = geom.x,
        y = geom.y,
        w = geom.width,
        h = geom.height
      );
      run_osascript(&script).await.context("Failed to set Ghostty window geometry via System Events")?;
    }
    TerminalApp::Other => {
      return Err(anyhow!(
        "PiP is not supported in this terminal (TERM_PROGRAM={:?})",
        std::env::var("TERM_PROGRAM").ok()
      ));
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

/// Compute the PiP window geometry: small window at the bottom-right of the screen.
pub async fn pip_geometry() -> Result<WindowGeometry> {
  let c = constants();
  let screen = get_screen_size().await.unwrap_or_else(|e| {
    warn!(err = %e, "pip: failed to get screen size, using 2560x1440 default");
    ScreenSize { width: 2560, height: 1440 }
  });

  Ok(WindowGeometry {
    x: (screen.width.saturating_sub(c.pip_width + c.pip_margin)) as i32,
    y: (screen.height.saturating_sub(c.pip_height + c.pip_margin)) as i32,
    width: c.pip_width,
    height: c.pip_height,
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

  #[test]
  fn parse_position_valid() {
    let (x, y) = parse_position("100, 200").unwrap();
    assert_eq!((x, y), (100, 200));
  }

  #[test]
  fn parse_position_no_spaces() {
    let (x, y) = parse_position("0,0").unwrap();
    assert_eq!((x, y), (0, 0));
  }

  #[test]
  fn parse_position_negative() {
    let (x, y) = parse_position("-50, 100").unwrap();
    assert_eq!((x, y), (-50, 100));
  }

  #[test]
  fn parse_position_wrong_count() {
    assert!(parse_position("100").is_err());
    assert!(parse_position("100, 200, 300").is_err());
  }

  #[test]
  fn parse_size_valid() {
    let (w, h) = parse_size("800, 600").unwrap();
    assert_eq!((w, h), (800, 600));
  }

  #[test]
  fn parse_size_no_spaces() {
    let (w, h) = parse_size("1920,1080").unwrap();
    assert_eq!((w, h), (1920, 1080));
  }

  #[test]
  fn parse_size_wrong_count() {
    assert!(parse_size("800").is_err());
    assert!(parse_size("800, 600, 100").is_err());
  }

  #[test]
  fn fullscreen_detection_fullscreen() {
    let geom = WindowGeometry { x: 0, y: 0, width: 2560, height: 1440 };
    let screen = ScreenSize { width: 2560, height: 1440 };
    assert!(is_likely_fullscreen(&geom, &screen));
  }

  #[test]
  fn fullscreen_detection_windowed() {
    let geom = WindowGeometry { x: 100, y: 100, width: 800, height: 600 };
    let screen = ScreenSize { width: 2560, height: 1440 };
    assert!(!is_likely_fullscreen(&geom, &screen));
  }

  #[test]
  fn fullscreen_detection_nearly_full() {
    // macOS fullscreen with menu bar offset
    let geom = WindowGeometry { x: 0, y: 38, width: 2560, height: 1402 };
    let screen = ScreenSize { width: 2560, height: 1440 };
    assert!(is_likely_fullscreen(&geom, &screen));
  }

  #[test]
  fn permission_error_detected_1743() {
    let stderr = "execution error: Not authorized to send Apple events to System Events. (-1743)";
    assert!(is_automation_permission_error(stderr));
  }

  #[test]
  fn permission_error_detected_assistive_access() {
    let stderr = "System Events got an error: osascript is not allowed assistive access.";
    assert!(is_automation_permission_error(stderr));
  }

  #[test]
  fn permission_error_not_detected_other() {
    let stderr = "execution error: Can't get window 1 of process \"Ghostty\". (-1728)";
    assert!(!is_automation_permission_error(stderr));
  }

  #[test]
  fn annotate_permission_error_is_concise() {
    let stderr = "execution error: Not authorized to send Apple events to System Events. (-1743)";
    let err = annotate_permission_error(stderr);
    let msg = err.to_string();
    // Should be a single line (no newlines) suitable for status bar display
    assert!(!msg.contains('\n'), "Error message should be single-line for TUI: {}", msg);
    assert!(msg.contains("Permission denied"), "Should mention permission: {}", msg);
  }
}
