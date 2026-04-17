use clap::ValueEnum;
use tracing::debug;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliDisplayMode {
  Auto,
  Kitty,
  Sixel,
  Direct,
  Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
  Ascii,
  Direct,
  Sixel,
  Kitty,
}

impl DisplayMode {
  pub fn label(self) -> &'static str {
    match self {
      DisplayMode::Ascii => "ASCII",
      DisplayMode::Direct => "Half-block",
      DisplayMode::Sixel => "Sixel",
      DisplayMode::Kitty => "Kitty",
    }
  }
}

/// Detect the best display mode the terminal supports.
///
/// Probe order: Kitty graphics > Sixel > true-color half-block > ASCII
///
/// - Kitty: `TERM=xterm-kitty`, `KITTY_WINDOW_ID` set, `TERM_PROGRAM` is
///   kitty/WezTerm/ghostty, `GHOST_TERMINAL=1`, or (inside tmux) the tmux
///   client's ancestor process is `kitty`/`ghostty`/`wezterm`
/// - Sixel: `TERM_PROGRAM` is foot/mlterm, or `TERM` contains "sixel"
/// - Direct: `COLORTERM` is `truecolor` or `24bit`
/// - Ascii: fallback
pub fn detect_display_mode() -> DisplayMode {
  let term = std::env::var("TERM").unwrap_or_default();
  let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default().to_lowercase();
  let ghost_terminal = std::env::var("GHOST_TERMINAL").unwrap_or_default();
  let kitty_window_id = std::env::var("KITTY_WINDOW_ID").is_ok();

  if term == "xterm-kitty"
    || kitty_window_id
    || matches!(term_program.as_str(), "kitty" | "wezterm" | "ghostty")
    || ghost_terminal == "1"
  {
    return DisplayMode::Kitty;
  }

  // Inside tmux the env vars point at tmux, not the real terminal.
  // Walk the tmux client's process ancestry to find the actual emulator.
  if std::env::var("TMUX").is_ok()
    && let Some(mode) = detect_via_tmux_client()
  {
    return mode;
  }

  if matches!(term_program.as_str(), "foot" | "mlterm" | "contour") || term.contains("sixel") {
    return DisplayMode::Sixel;
  }

  let colorterm = std::env::var("COLORTERM").unwrap_or_default().to_lowercase();
  if colorterm == "truecolor" || colorterm == "24bit" {
    return DisplayMode::Direct;
  }

  DisplayMode::Ascii
}

/// Query the tmux client PID attached to the current pane, then walk its
/// ancestor process chain looking for a known terminal emulator.
fn detect_via_tmux_client() -> Option<DisplayMode> {
  let output = std::process::Command::new("tmux").args(["display-message", "-p", "#{client_pid}"]).output().ok()?;
  let client_pid: u32 = String::from_utf8_lossy(&output.stdout).trim().parse().ok()?;
  debug!(client_pid, "tmux client pid");

  let mut pid = client_pid;
  for _ in 0..10 {
    let out = std::process::Command::new("ps").args(["-o", "ppid=,comm=", "-p", &pid.to_string()]).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (ppid_str, comm) = line.split_once(char::is_whitespace)?;
    let ppid: u32 = ppid_str.trim().parse().ok()?;
    let name = comm.rsplit('/').next().unwrap_or(comm).to_lowercase();
    debug!(ppid, name, "walking ancestor");

    if name.contains("kitty") || name.contains("ghostty") || name.contains("wezterm") {
      return Some(DisplayMode::Kitty);
    }
    if ppid <= 1 {
      break;
    }
    pid = ppid;
  }
  None
}

pub fn resolve_display_mode(cli: CliDisplayMode) -> DisplayMode {
  match cli {
    CliDisplayMode::Auto => detect_display_mode(),
    CliDisplayMode::Kitty => DisplayMode::Kitty,
    CliDisplayMode::Sixel => DisplayMode::Sixel,
    CliDisplayMode::Direct => DisplayMode::Direct,
    CliDisplayMode::Ascii => DisplayMode::Ascii,
  }
}
