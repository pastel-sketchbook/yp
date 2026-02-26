use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use color_quant::NeuQuant;
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use ratatui::{
  buffer::Buffer,
  layout::Rect,
  style::{Color, Style},
  widgets::Widget,
};
use std::io::{Cursor, Write};

use crate::display::DisplayMode;

// --- Thumbnail Widget ---

pub struct ThumbnailWidget<'a> {
  pub image: &'a DynamicImage,
  pub display_mode: DisplayMode,
}

const ASCII_CHARS: [&str; 10] = [" ", ".", ":", "-", "=", "+", "*", "#", "%", "@"];

impl Widget for ThumbnailWidget<'_> {
  fn render(self, area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
      return;
    }
    match self.display_mode {
      DisplayMode::Direct => render_direct(self.image, area, buf),
      DisplayMode::Ascii => render_ascii(self.image, area, buf),
      DisplayMode::Kitty | DisplayMode::Sixel => {}
    }
  }
}

fn render_direct(image: &DynamicImage, area: Rect, buf: &mut Buffer) {
  // Image is already resized by the caller; just convert to RGB8.
  let resized = image.to_rgb8();
  let img_w = resized.width().min(area.width as u32);
  let img_h = resized.height();
  let cell_h = img_h.div_ceil(2);
  let offset_x = (area.width as u32).saturating_sub(img_w) / 2;
  let offset_y = (area.height as u32).saturating_sub(cell_h) / 2;

  for y in 0..cell_h.min(area.height as u32) {
    for x in 0..img_w {
      let upper = resized.get_pixel(x, y * 2);
      let lower_y = y * 2 + 1;
      let fg = Color::Rgb(upper[0], upper[1], upper[2]);
      let bg = if lower_y < img_h {
        let lower = resized.get_pixel(x, lower_y);
        Color::Rgb(lower[0], lower[1], lower[2])
      } else {
        Color::Reset
      };
      buf.set_string(
        area.x + offset_x as u16 + x as u16,
        area.y + offset_y as u16 + y as u16,
        "▀",
        Style::default().fg(fg).bg(bg),
      );
    }
  }
}

fn render_ascii(image: &DynamicImage, area: Rect, buf: &mut Buffer) {
  // Image is already resized by the caller; just convert to grayscale.
  let resized = image.to_luma8();
  let img_w = resized.width().min(area.width as u32);
  let img_h = resized.height().min(area.height as u32);
  let offset_x = (area.width as u32).saturating_sub(img_w) / 2;
  let offset_y = (area.height as u32).saturating_sub(img_h) / 2;

  for y in 0..img_h {
    for x in 0..img_w {
      let pixel = resized.get_pixel(x, y)[0];
      let idx = ((pixel as f32 / 255.0) * (ASCII_CHARS.len() - 1) as f32).round() as usize;
      let idx = idx.min(ASCII_CHARS.len() - 1);
      buf.set_string(
        area.x + offset_x as u16 + x as u16,
        area.y + offset_y as u16 + y as u16,
        ASCII_CHARS[idx],
        Style::default(),
      );
    }
  }
}

// --- Kitty Graphics Protocol ---
//
// Sends an image to the terminal using the Kitty graphics protocol (OSC APC).
//
//   Transmit:  \x1B_G a=T,f=100,t=d,c=<cols>,r=<rows>,q=2,m=1;<base64 chunk>\x1B\\
//   Continue:  \x1B_G m=1;<base64 chunk>\x1B\\
//   Last:      \x1B_G m=0;<base64 chunk>\x1B\\
//   Delete:    \x1B_G a=d,d=a,q=2\x1B\\
//
// The image is encoded as PNG, base64'd, and sent in <=4096-byte chunks.
// `c` and `r` tell the terminal how many columns/rows to scale the image over.

const KITTY_CHUNK_SIZE: usize = 4096;

/// Delete all Kitty images currently displayed.
pub fn kitty_delete_all() -> Result<()> {
  let mut stdout = std::io::stdout();
  write!(stdout, "\x1B_Ga=d,d=a,q=2\x1B\\")?;
  stdout.flush().context("Failed to flush kitty delete")?;
  Ok(())
}

/// Render an image at `area` using the Kitty graphics protocol.
pub fn kitty_render_image(image: &DynamicImage, area: Rect) -> Result<()> {
  if area.is_empty() {
    return Ok(());
  }

  // Encode the full-resolution image as PNG. The Kitty protocol's c/r
  // parameters tell the terminal how many columns/rows to scale into,
  // so sending the original avoids lossy double-resize and produces
  // the sharpest result at the terminal's native pixel density.
  let mut png_buf = Vec::new();
  image
    .write_to(&mut Cursor::new(&mut png_buf), ImageFormat::Png)
    .context("Failed to encode thumbnail as PNG for kitty")?;

  let b64 = BASE64.encode(&png_buf);
  let chunks: Vec<&[u8]> = b64.as_bytes().chunks(KITTY_CHUNK_SIZE).collect();
  let last = chunks.len().saturating_sub(1);

  let mut stdout = std::io::stdout();

  write!(stdout, "\x1B[{};{}H", area.y + 1, area.x + 1)?;

  for (i, chunk) in chunks.iter().enumerate() {
    let data = std::str::from_utf8(chunk).context("base64 chunk was not valid UTF-8")?;
    let more = if i < last { 1 } else { 0 };

    if i == 0 {
      write!(stdout, "\x1B_Ga=T,f=100,t=d,c={},r={},q=2,m={};{}\x1B\\", area.width, area.height, more, data)?;
    } else {
      write!(stdout, "\x1B_Gm={};{}\x1B\\", more, data)?;
    }
  }

  stdout.flush().context("Failed to flush kitty image")?;
  Ok(())
}

// --- Sixel Graphics Protocol ---
//
// Sixel encodes images at pixel resolution directly in the terminal stream.
// Each sixel "row" represents 6 vertical pixels. Colors are defined via
// registers and then pixels are emitted as characters in the range 0x3F–0x7E.
//
// Sequence:
//   DCS q <data> ST
//   DCS = \x1BP,  ST = \x1B\\
//
// Color register:  #<n>;2;<r%>;<g%>;<b%>
// Sixel data char: offset 0x3F (63) + 6-bit bitmap of which of 6 rows are "on"
// $  = carriage return (rewind to start of current sixel row)
// -  = newline (advance to next sixel row)
//
// Color quantization uses NeuQuant (neural-network-based, via `color_quant` crate).

const SIXEL_MAX_COLORS: usize = 256;

/// Render an image at `area` using the Sixel graphics protocol.
pub fn sixel_render_image(image: &DynamicImage, area: Rect) -> Result<()> {
  if area.is_empty() {
    return Ok(());
  }

  let pixel_w = area.width as u32 * 8;
  let pixel_h = area.height as u32 * 16;
  let resized = image.resize_to_fill(pixel_w, pixel_h, FilterType::Lanczos3).into_rgb8();
  let (w, h) = (resized.width() as usize, resized.height() as usize);

  let rgba_pixels: Vec<u8> = resized.pixels().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
  let nq = NeuQuant::new(3, SIXEL_MAX_COLORS, &rgba_pixels);
  let palette: Vec<[u8; 3]> =
    (0..SIXEL_MAX_COLORS).map(|i| nq.color_map_rgb()[i * 3..i * 3 + 3].try_into().unwrap_or([0, 0, 0])).collect();

  let indices: Vec<u8> = resized.pixels().map(|p| nq.index_of(&[p[0], p[1], p[2], 255]) as u8).collect();

  let mut out = String::with_capacity(w * h);

  out.push_str("\x1BPq");
  out.push_str(&format!("\"1;1;{};{}", w, h));

  for (i, c) in palette.iter().enumerate() {
    let r_pct = (c[0] as u32 * 100) / 255;
    let g_pct = (c[1] as u32 * 100) / 255;
    let b_pct = (c[2] as u32 * 100) / 255;
    out.push_str(&format!("#{};2;{};{};{}", i, r_pct, g_pct, b_pct));
  }

  let sixel_rows = h.div_ceil(6);
  for sr in 0..sixel_rows {
    let y_base = sr * 6;

    for (color_idx, _) in palette.iter().enumerate() {
      let color_idx_u8 = color_idx as u8;
      let mut has_pixels = false;
      let mut row_data = Vec::with_capacity(w);

      for x in 0..w {
        let mut sixel_val: u8 = 0;
        for bit in 0..6 {
          let y = y_base + bit;
          if y < h && indices[y * w + x] == color_idx_u8 {
            sixel_val |= 1 << bit;
            has_pixels = true;
          }
        }
        row_data.push(sixel_val);
      }

      if !has_pixels {
        continue;
      }

      out.push_str(&format!("#{}", color_idx));

      let mut i = 0;
      while i < row_data.len() {
        let val = row_data[i];
        let ch = (val + 0x3F) as char;
        let mut run = 1usize;
        while i + run < row_data.len() && row_data[i + run] == val {
          run += 1;
        }
        if run > 3 {
          out.push_str(&format!("!{}{}", run, ch));
        } else {
          for _ in 0..run {
            out.push(ch);
          }
        }
        i += run;
      }
      out.push('$');
    }
    out.push('-');
  }

  out.push_str("\x1B\\");

  let mut stdout = std::io::stdout();
  write!(stdout, "\x1B[{};{}H{}", area.y + 1, area.x + 1, out)?;
  stdout.flush().context("Failed to flush sixel image")?;
  Ok(())
}
