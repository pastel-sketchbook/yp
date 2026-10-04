use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
  pub name: &'static str,
  /// Gradient stops for the spectrum: low, middle, high band.
  pub spectrum: [Color; 3],
  pub bg: Color,
  pub fg: Color,
  pub accent: Color,
  pub muted: Color,
  pub border: Color,
  pub error: Color,
  pub status: Color,
  pub highlight_bg: Color,
  pub highlight_fg: Color,
  pub stripe_bg: Color,
  pub key_bg: Color,
  pub key_fg: Color,
  pub tag: Color,
  pub panel_bg: Color,
}

pub const THEMES: &[Theme] = &[
  // Default — dark, cyan accent
  Theme {
    name: "Default",
    spectrum: [Color::Rgb(0, 200, 80), Color::Rgb(0, 217, 255), Color::Rgb(255, 80, 80)],
    bg: Color::Reset,
    fg: Color::White,
    accent: Color::Rgb(0, 217, 255),
    muted: Color::DarkGray,
    border: Color::DarkGray,
    error: Color::Rgb(255, 80, 80),
    status: Color::Rgb(0, 217, 255),
    highlight_bg: Color::Rgb(40, 40, 60),
    highlight_fg: Color::Rgb(255, 220, 100),
    stripe_bg: Color::Rgb(28, 28, 34),
    key_bg: Color::DarkGray,
    key_fg: Color::Black,
    tag: Color::Rgb(180, 140, 255),
    panel_bg: Color::Rgb(24, 24, 30),
  },
  // Gruvbox Dark
  Theme {
    name: "Gruvbox",
    spectrum: [Color::Rgb(184, 187, 38), Color::Rgb(250, 189, 47), Color::Rgb(251, 73, 52)],
    bg: Color::Rgb(29, 32, 33),
    fg: Color::Rgb(235, 219, 178),
    accent: Color::Rgb(215, 153, 33),
    muted: Color::Rgb(146, 131, 116),
    border: Color::Rgb(62, 57, 54),
    error: Color::Rgb(251, 73, 52),
    status: Color::Rgb(184, 187, 38),
    highlight_bg: Color::Rgb(50, 48, 47),
    highlight_fg: Color::Rgb(250, 189, 47),
    stripe_bg: Color::Rgb(40, 40, 40),
    key_bg: Color::Rgb(80, 73, 69),
    key_fg: Color::Rgb(235, 219, 178),
    tag: Color::Rgb(131, 165, 152),
    panel_bg: Color::Rgb(37, 36, 36),
  },
  // Solarized Dark
  Theme {
    name: "Solarized",
    spectrum: [Color::Rgb(133, 153, 0), Color::Rgb(181, 137, 0), Color::Rgb(220, 50, 47)],
    bg: Color::Rgb(0, 43, 54),
    fg: Color::Rgb(253, 246, 227),
    accent: Color::Rgb(42, 161, 152),
    muted: Color::Rgb(131, 148, 150),
    border: Color::Rgb(16, 58, 68),
    error: Color::Rgb(220, 50, 47),
    status: Color::Rgb(181, 137, 0),
    highlight_bg: Color::Rgb(7, 54, 66),
    highlight_fg: Color::Rgb(253, 246, 227),
    stripe_bg: Color::Rgb(3, 48, 58),
    key_bg: Color::Rgb(88, 110, 117),
    key_fg: Color::Rgb(253, 246, 227),
    tag: Color::Rgb(108, 113, 196),
    panel_bg: Color::Rgb(7, 54, 66),
  },
  // Ayu Dark
  Theme {
    name: "Ayu",
    spectrum: [Color::Rgb(125, 210, 80), Color::Rgb(255, 180, 84), Color::Rgb(240, 113, 113)],
    bg: Color::Rgb(10, 14, 20),
    fg: Color::Rgb(191, 191, 191),
    accent: Color::Rgb(255, 153, 64),
    muted: Color::Rgb(92, 103, 115),
    border: Color::Rgb(40, 44, 52),
    error: Color::Rgb(240, 113, 113),
    status: Color::Rgb(85, 180, 211),
    highlight_bg: Color::Rgb(20, 24, 32),
    highlight_fg: Color::Rgb(255, 180, 84),
    stripe_bg: Color::Rgb(15, 19, 26),
    key_bg: Color::Rgb(60, 66, 76),
    key_fg: Color::Rgb(191, 191, 191),
    tag: Color::Rgb(210, 154, 230),
    panel_bg: Color::Rgb(18, 22, 30),
  },
  // Flexoki Dark
  Theme {
    name: "Flexoki",
    spectrum: [Color::Rgb(208, 162, 21), Color::Rgb(36, 131, 123), Color::Rgb(209, 77, 65)],
    bg: Color::Rgb(16, 15, 15),
    fg: Color::Rgb(206, 205, 195),
    accent: Color::Rgb(36, 131, 123),
    muted: Color::Rgb(135, 133, 128),
    border: Color::Rgb(40, 39, 38),
    error: Color::Rgb(209, 77, 65),
    status: Color::Rgb(208, 162, 21),
    highlight_bg: Color::Rgb(28, 27, 26),
    highlight_fg: Color::Rgb(208, 162, 21),
    stripe_bg: Color::Rgb(22, 21, 20),
    key_bg: Color::Rgb(52, 51, 49),
    key_fg: Color::Rgb(206, 205, 195),
    tag: Color::Rgb(142, 139, 206),
    panel_bg: Color::Rgb(24, 23, 22),
  },
  // Zoegi Dark
  Theme {
    name: "Zoegi",
    spectrum: [Color::Rgb(92, 168, 112), Color::Rgb(128, 200, 160), Color::Rgb(204, 92, 92)],
    bg: Color::Rgb(20, 20, 20),
    fg: Color::Rgb(204, 204, 204),
    accent: Color::Rgb(64, 128, 104),
    muted: Color::Rgb(89, 89, 89),
    border: Color::Rgb(48, 48, 48),
    error: Color::Rgb(204, 92, 92),
    status: Color::Rgb(86, 139, 153),
    highlight_bg: Color::Rgb(34, 34, 34),
    highlight_fg: Color::Rgb(128, 200, 160),
    stripe_bg: Color::Rgb(27, 27, 27),
    key_bg: Color::Rgb(64, 64, 64),
    key_fg: Color::Rgb(204, 204, 204),
    tag: Color::Rgb(150, 180, 210),
    panel_bg: Color::Rgb(28, 28, 28),
  },
  // FFE Dark (Fuzzy Find Everything)
  Theme {
    name: "FFE Dark",
    spectrum: [Color::Rgb(161, 239, 211), Color::Rgb(240, 169, 136), Color::Rgb(255, 117, 127)],
    bg: Color::Rgb(30, 35, 43),
    fg: Color::Rgb(216, 222, 233),
    accent: Color::Rgb(79, 214, 190),
    muted: Color::Rgb(155, 162, 175),
    border: Color::Rgb(59, 66, 82),
    error: Color::Rgb(255, 117, 127),
    status: Color::Rgb(161, 239, 211),
    highlight_bg: Color::Rgb(46, 52, 64),
    highlight_fg: Color::Rgb(240, 169, 136),
    stripe_bg: Color::Rgb(26, 31, 39),
    key_bg: Color::Rgb(59, 66, 82),
    key_fg: Color::Rgb(216, 222, 233),
    tag: Color::Rgb(137, 220, 235),
    panel_bg: Color::Rgb(26, 31, 39),
  },
  // Postrboard Dark
  Theme {
    name: "Postrboard",
    spectrum: [Color::Rgb(132, 204, 22), Color::Rgb(251, 138, 77), Color::Rgb(248, 113, 113)],
    bg: Color::Rgb(26, 27, 38),
    fg: Color::Rgb(226, 232, 240),
    accent: Color::Rgb(79, 182, 232),
    muted: Color::Rgb(124, 141, 163),
    border: Color::Rgb(42, 45, 61),
    error: Color::Rgb(248, 113, 113),
    status: Color::Rgb(132, 204, 22),
    highlight_bg: Color::Rgb(54, 58, 79),
    highlight_fg: Color::Rgb(251, 138, 77),
    stripe_bg: Color::Rgb(30, 31, 43),
    key_bg: Color::Rgb(54, 58, 79),
    key_fg: Color::Rgb(226, 232, 240),
    tag: Color::Rgb(96, 165, 250),
    panel_bg: Color::Rgb(22, 23, 31),
  },
  // --- Light themes ---
  // Default Light
  Theme {
    name: "Default Light",
    spectrum: [Color::Rgb(0, 140, 50), Color::Rgb(0, 140, 180), Color::Rgb(200, 40, 40)],
    bg: Color::Reset,
    fg: Color::Rgb(40, 40, 50),
    accent: Color::Rgb(0, 140, 180),
    muted: Color::Rgb(120, 120, 130),
    border: Color::Rgb(180, 180, 190),
    error: Color::Rgb(200, 40, 40),
    status: Color::Rgb(0, 140, 180),
    highlight_bg: Color::Rgb(220, 225, 235),
    highlight_fg: Color::Rgb(30, 30, 40),
    stripe_bg: Color::Rgb(240, 240, 245),
    key_bg: Color::Rgb(180, 180, 190),
    key_fg: Color::Rgb(40, 40, 50),
    tag: Color::Rgb(100, 80, 180),
    panel_bg: Color::Rgb(235, 235, 240),
  },
  // Gruvbox Light
  Theme {
    name: "Gruvbox Light",
    spectrum: [Color::Rgb(121, 116, 14), Color::Rgb(215, 153, 33), Color::Rgb(204, 36, 29)],
    bg: Color::Rgb(251, 241, 199),
    fg: Color::Rgb(60, 56, 54),
    accent: Color::Rgb(215, 153, 33),
    muted: Color::Rgb(146, 131, 116),
    border: Color::Rgb(213, 196, 161),
    error: Color::Rgb(204, 36, 29),
    status: Color::Rgb(121, 116, 14),
    highlight_bg: Color::Rgb(235, 219, 178),
    highlight_fg: Color::Rgb(60, 56, 54),
    stripe_bg: Color::Rgb(249, 236, 186),
    key_bg: Color::Rgb(213, 196, 161),
    key_fg: Color::Rgb(60, 56, 54),
    tag: Color::Rgb(69, 133, 136),
    panel_bg: Color::Rgb(242, 233, 185),
  },
  // Solarized Light
  Theme {
    name: "Solarized Light",
    spectrum: [Color::Rgb(133, 153, 0), Color::Rgb(181, 137, 0), Color::Rgb(220, 50, 47)],
    bg: Color::Rgb(253, 246, 227),
    fg: Color::Rgb(88, 110, 117),
    accent: Color::Rgb(42, 161, 152),
    muted: Color::Rgb(147, 161, 161),
    border: Color::Rgb(220, 212, 188),
    error: Color::Rgb(220, 50, 47),
    status: Color::Rgb(133, 153, 0),
    highlight_bg: Color::Rgb(238, 232, 213),
    highlight_fg: Color::Rgb(7, 54, 66),
    stripe_bg: Color::Rgb(245, 239, 218),
    key_bg: Color::Rgb(220, 212, 188),
    key_fg: Color::Rgb(88, 110, 117),
    tag: Color::Rgb(108, 113, 196),
    panel_bg: Color::Rgb(238, 232, 213),
  },
  // Flexoki Light
  Theme {
    name: "Flexoki Light",
    spectrum: [Color::Rgb(102, 128, 11), Color::Rgb(36, 131, 123), Color::Rgb(209, 77, 65)],
    bg: Color::Rgb(255, 252, 240),
    fg: Color::Rgb(16, 15, 15),
    accent: Color::Rgb(36, 131, 123),
    muted: Color::Rgb(111, 110, 105),
    border: Color::Rgb(230, 228, 217),
    error: Color::Rgb(209, 77, 65),
    status: Color::Rgb(102, 128, 11),
    highlight_bg: Color::Rgb(242, 240, 229),
    highlight_fg: Color::Rgb(16, 15, 15),
    stripe_bg: Color::Rgb(247, 245, 234),
    key_bg: Color::Rgb(230, 228, 217),
    key_fg: Color::Rgb(16, 15, 15),
    tag: Color::Rgb(100, 92, 187),
    panel_bg: Color::Rgb(244, 241, 230),
  },
  // Ayu Light
  Theme {
    name: "Ayu Light",
    spectrum: [Color::Rgb(133, 179, 4), Color::Rgb(255, 153, 64), Color::Rgb(240, 113, 113)],
    bg: Color::Rgb(252, 252, 252),
    fg: Color::Rgb(92, 97, 102),
    accent: Color::Rgb(255, 153, 64),
    muted: Color::Rgb(153, 160, 166),
    border: Color::Rgb(207, 209, 210),
    error: Color::Rgb(240, 113, 113),
    status: Color::Rgb(133, 179, 4),
    highlight_bg: Color::Rgb(230, 230, 230),
    highlight_fg: Color::Rgb(92, 97, 102),
    stripe_bg: Color::Rgb(243, 244, 245),
    key_bg: Color::Rgb(207, 209, 210),
    key_fg: Color::Rgb(92, 97, 102),
    tag: Color::Rgb(163, 122, 204),
    panel_bg: Color::Rgb(242, 242, 242),
  },
  // Zoegi Light
  Theme {
    name: "Zoegi Light",
    spectrum: [Color::Rgb(55, 121, 97), Color::Rgb(80, 120, 160), Color::Rgb(204, 92, 92)],
    bg: Color::Rgb(255, 255, 255),
    fg: Color::Rgb(51, 51, 51),
    accent: Color::Rgb(55, 121, 97),
    muted: Color::Rgb(89, 89, 89),
    border: Color::Rgb(230, 230, 230),
    error: Color::Rgb(204, 92, 92),
    status: Color::Rgb(55, 121, 97),
    highlight_bg: Color::Rgb(235, 235, 235),
    highlight_fg: Color::Rgb(51, 51, 51),
    stripe_bg: Color::Rgb(247, 247, 247),
    key_bg: Color::Rgb(230, 230, 230),
    key_fg: Color::Rgb(51, 51, 51),
    tag: Color::Rgb(80, 120, 160),
    panel_bg: Color::Rgb(245, 245, 245),
  },
  // FFE Light (Fuzzy Find Everything)
  Theme {
    name: "FFE Light",
    spectrum: [Color::Rgb(26, 138, 110), Color::Rgb(192, 121, 32), Color::Rgb(201, 67, 78)],
    bg: Color::Rgb(232, 236, 240),
    fg: Color::Rgb(30, 35, 43),
    accent: Color::Rgb(42, 157, 132),
    muted: Color::Rgb(74, 80, 96),
    border: Color::Rgb(201, 205, 214),
    error: Color::Rgb(201, 67, 78),
    status: Color::Rgb(26, 138, 110),
    highlight_bg: Color::Rgb(221, 225, 232),
    highlight_fg: Color::Rgb(192, 121, 32),
    stripe_bg: Color::Rgb(245, 247, 250),
    key_bg: Color::Rgb(201, 205, 214),
    key_fg: Color::Rgb(30, 35, 43),
    tag: Color::Rgb(58, 142, 164),
    panel_bg: Color::Rgb(245, 247, 250),
  },
  // Postrboard Light
  Theme {
    name: "Postrboard Light",
    spectrum: [Color::Rgb(77, 124, 15), Color::Rgb(194, 65, 12), Color::Rgb(220, 38, 38)],
    bg: Color::Rgb(250, 250, 250),
    fg: Color::Rgb(17, 24, 39),
    accent: Color::Rgb(2, 132, 199),
    muted: Color::Rgb(100, 116, 139),
    border: Color::Rgb(203, 213, 225),
    error: Color::Rgb(220, 38, 38),
    status: Color::Rgb(77, 124, 15),
    highlight_bg: Color::Rgb(226, 232, 240),
    highlight_fg: Color::Rgb(194, 65, 12),
    stripe_bg: Color::Rgb(248, 250, 252),
    key_bg: Color::Rgb(203, 213, 225),
    key_fg: Color::Rgb(17, 24, 39),
    tag: Color::Rgb(12, 74, 110),
    panel_bg: Color::Rgb(241, 245, 249),
  },
];

/// Spectrum colors are authored as RGB, so every stop blends without a match arm.
pub fn channels(color: Color) -> Option<[u8; 3]> {
  match color {
    Color::Rgb(r, g, b) => Some([r, g, b]),
    _ => None,
  }
}

/// Linear sRGB interpolation between two colors; `t` is clamped to 0..1.
/// Non-RGB endpoints (Reset, White, DarkGray) fall back to `a` so blending
/// degrades to a flat color instead of panicking mid-draw.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
  let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
  let (Some(a), Some(b)) = (channels(a), channels(b)) else {
    return a;
  };
  let mix = |i: usize| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8;
  Color::Rgb(mix(0), mix(1), mix(2))
}

/// The same color as an explicit RGB triple.
///
/// Several themes use the ANSI names for their text and borders, and [`blend`]
/// leaves a non-RGB endpoint alone. Anything that blends *toward* a role — the
/// fire ramp reaching white, say — would otherwise stop short and never arrive.
/// The four names yp uses map to their usual terminal values; `Reset` has no
/// knowable value, so mid-gray keeps a blend from going dark.
pub fn rgb(color: Color) -> Color {
  match color {
    Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    Color::Black => Color::Rgb(0, 0, 0),
    Color::DarkGray => Color::Rgb(0x55, 0x55, 0x55),
    Color::White => Color::Rgb(0xff, 0xff, 0xff),
    _ => Color::Rgb(0x80, 0x80, 0x80),
  }
}

/// Perceived brightness of a theme color, 0 to 255.
///
/// A named color read as black would call every dark theme light, so the names
/// are resolved first.
fn luma(color: Color) -> f32 {
  let Color::Rgb(r, g, b) = rgb(color) else { unreachable!("rgb always returns an RGB color") };
  let [r, g, b] = [r, g, b].map(f32::from);
  0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Whether the theme paints light text on a dark ground, or the reverse.
///
/// The fire style inverts its ramp by theme: a dark theme wants a hot
/// red-to-white flame, a light theme wants the strongest ink.
pub fn is_light(theme: &Theme) -> bool {
  luma(theme.panel_bg) > luma(theme.fg)
}

/// Continuous spectrum gradient: the low stop at 0, the middle stop at
/// [`MIDDLE`], the high stop at 1. Mid sits above the halfway point so the
/// low half of the ramp spans the full bar, matching the height zones.
const MIDDLE: f32 = 0.675;

pub fn spectrum_gradient(theme: &Theme, t: f32) -> Color {
  let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
  if t <= MIDDLE {
    blend(theme.spectrum[0], theme.spectrum[1], t / MIDDLE)
  } else {
    blend(theme.spectrum[1], theme.spectrum[2], (t - MIDDLE) / (1.0 - MIDDLE))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn is_light_agrees_with_the_theme_names() {
    // The fire style inverts its ramp on this answer, so a theme classified the
    // wrong way burns with the wrong end of the spectrum at full heat.
    for theme in THEMES {
      let named_light = theme.name.ends_with("Light");
      assert_eq!(is_light(theme), named_light, "{} was classified wrongly", theme.name);
    }
  }

  #[test]
  fn rgb_resolves_the_named_colors_yp_uses() {
    // A role blended toward must be reachable, which needs an RGB value.
    for theme in THEMES {
      assert!(matches!(rgb(theme.fg), Color::Rgb(..)), "{}", theme.name);
      assert_eq!(rgb(Color::Rgb(1, 2, 3)), Color::Rgb(1, 2, 3));
    }
    assert_eq!(rgb(Color::White), Color::Rgb(255, 255, 255));
    assert_eq!(rgb(Color::Black), Color::Rgb(0, 0, 0));
    assert_eq!(blend(Color::Rgb(255, 0, 0), rgb(Color::White), 1.0), Color::Rgb(255, 255, 255));
  }

  #[test]
  fn named_colors_are_not_read_as_black() {
    // Themes use the ANSI names for text and borders; treating them as black
    // would call every dark theme light.
    let theme = THEMES[0];
    assert_eq!(theme.fg, Color::White);
    assert!(luma(Color::White) > luma(Color::DarkGray));
    assert!(luma(Color::DarkGray) > luma(Color::Black));
    assert!(!is_light(&theme), "a dark panel with white text is a dark theme");
  }

  #[test]
  fn every_theme_has_three_spectrum_stops() {
    assert_eq!(THEMES.len(), 16);
    for theme in THEMES {
      assert!(
        theme.spectrum.iter().all(|c| matches!(c, Color::Rgb(..))),
        "{} has a non-RGB spectrum stop: {:?}",
        theme.name,
        theme.spectrum
      );
    }
  }

  #[test]
  fn blend_hits_its_endpoints_and_clamps() {
    let (a, b) = (Color::Rgb(0, 100, 200), Color::Rgb(200, 100, 0));
    assert_eq!(blend(a, b, 0.0), a);
    assert_eq!(blend(a, b, 1.0), b);
    assert_eq!(blend(a, b, -1.0), a);
    assert_eq!(blend(a, b, 2.0), b);
    assert_eq!(blend(a, b, f32::NAN), a, "NaN cannot produce an out-of-range mix");
    assert_eq!(blend(a, b, 0.5), Color::Rgb(100, 100, 100));
  }

  #[test]
  fn blend_survives_non_rgb_theme_colors() {
    // bg is Reset and muted is DarkGray in the Default theme.
    assert_eq!(blend(Color::Reset, Color::White, 0.5), Color::Reset);
    assert_eq!(blend(Color::White, Color::Reset, 0.5), Color::White);
    assert_eq!(blend(Color::DarkGray, Color::Reset, 0.5), Color::DarkGray);
  }

  #[test]
  fn gradient_runs_through_the_theme_stops() {
    for theme in THEMES {
      assert_eq!(spectrum_gradient(theme, 0.0), theme.spectrum[0], "{}", theme.name);
      assert_eq!(spectrum_gradient(theme, MIDDLE), theme.spectrum[1], "{}", theme.name);
      assert_eq!(spectrum_gradient(theme, 1.0), theme.spectrum[2], "{}", theme.name);
      // Between stops the gradient must produce genuine in-between colors,
      // otherwise every band would render as a flat stop color.
      let mid = spectrum_gradient(theme, 0.3);
      assert_ne!(mid, theme.spectrum[0], "{}", theme.name);
      assert_ne!(mid, theme.spectrum[1], "{}", theme.name);
      assert_eq!(spectrum_gradient(theme, f32::NAN), theme.spectrum[0], "{}", theme.name);
    }
  }
}
