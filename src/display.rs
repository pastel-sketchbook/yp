use clap::ValueEnum;

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
/// - Kitty: `TERM=xterm-kitty`, or `TERM_PROGRAM` is kitty/WezTerm/ghostty
/// - Sixel: `TERM_PROGRAM` is foot/mlterm, or `TERM` contains "sixel"
/// - Direct: `COLORTERM` is `truecolor` or `24bit`
/// - Ascii: fallback
pub fn detect_display_mode() -> DisplayMode {
  let term = std::env::var("TERM").unwrap_or_default();
  let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default().to_lowercase();

  if term == "xterm-kitty" || matches!(term_program.as_str(), "kitty" | "wezterm" | "ghostty") {
    return DisplayMode::Kitty;
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

pub fn resolve_display_mode(cli: CliDisplayMode) -> DisplayMode {
  match cli {
    CliDisplayMode::Auto => detect_display_mode(),
    CliDisplayMode::Kitty => DisplayMode::Kitty,
    CliDisplayMode::Sixel => DisplayMode::Sixel,
    CliDisplayMode::Direct => DisplayMode::Direct,
    CliDisplayMode::Ascii => DisplayMode::Ascii,
  }
}
