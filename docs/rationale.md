# yp — Technical Rationale

This document explains the key technical decisions in `yp`, a terminal-based YouTube music player written in Rust. It covers architecture choices, trade-offs, and the reasoning behind non-obvious implementation details.

## Architecture Overview

### Single-binary, multi-module design

yp is structured as 7 Rust source files (~3,900 lines total):

| Module | Lines | Responsibility |
|---|---|---|
| `main.rs` | ~1,560 | App state, async task orchestration, event handling, transcription pipeline |
| `youtube.rs` | ~810 | yt-dlp integration, search, channel listing, enrichment, storyboard/video frames |
| `ui.rs` | ~800 | ratatui TUI rendering, layout, theming, text highlighting |
| `graphics.rs` | ~280 | Image rendering: Kitty protocol, Sixel protocol, half-block, ASCII art |
| `theme.rs` | ~220 | 12 themes (6 dark + 6 light), 15-field Theme struct |
| `player.rs` | ~160 | mpv subprocess management, IPC, playback status monitoring |
| `display.rs` | ~70 | Display mode enum, terminal capability auto-detection |

The app was originally a single `main.rs` and was split into modules as complexity grew. Each module has a focused responsibility but they share types freely through `pub` visibility — this is intentional for a single-binary TUI app where encapsulation boundaries don't need to be as strict as in a library.

### Why yt-dlp + mpv (not YouTube Data API + custom audio)

- **No API key required**: yt-dlp scrapes YouTube directly. Users don't need Google Cloud credentials.
- **mpv handles all codec complexity**: YouTube serves audio in various formats (Opus, AAC, WebM). mpv's `--no-video` mode handles format negotiation, buffering, and decoding. Reimplementing this would be enormous.
- **yt-dlp handles anti-bot measures**: YouTube frequently changes its extraction methods. yt-dlp tracks these changes with regular releases. Depending on it via subprocess means yp benefits from `brew upgrade yt-dlp` without recompilation.
- **Trade-off**: Subprocess spawning adds latency (~1-3s per search/info call) and requires external binaries. This is acceptable for a music player where human interaction is the bottleneck.

### Why ratatui (not crossterm directly, not cursive/tui-rs)

- **ratatui** is the maintained fork of tui-rs with active development and a large widget ecosystem.
- Using crossterm directly would mean reimplementing widget layout, list scrolling, text wrapping, and block borders — all of which ratatui provides.
- The immediate-mode rendering model (rebuild the entire frame each tick) simplifies state management: there's no widget tree to synchronize with app state.

## Display Mode System

### Four rendering backends

yp supports four ways to display thumbnail images in the terminal:

1. **Kitty Graphics Protocol** — Sends PNG data directly to the terminal via escape sequences. The terminal decodes and renders at native pixel resolution. Sharpest output.
2. **Sixel** — Encodes images as sixel data (6 vertical pixels per character row). Requires NeuQuant color quantization to 256 colors. Supported by fewer terminals.
3. **Half-block (Direct)** — Uses Unicode `▀` characters with foreground/background true-color to render 2 vertical pixels per cell. Works in any true-color terminal.
4. **ASCII** — Grayscale luminance mapped to a 10-character ramp (`" .:‐=+*#%@"`). Universal fallback.

### Auto-detection logic (`display.rs`)

The probe order is Kitty > Sixel > Direct > ASCII, checked via environment variables:

- `TERM=xterm-kitty` or `TERM_PROGRAM` in {kitty, wezterm, ghostty} → Kitty
- `TERM_PROGRAM` in {foot, mlterm, contour} or `TERM` contains "sixel" → Sixel
- `COLORTERM` is "truecolor" or "24bit" → Direct
- Fallback → ASCII

This avoids runtime capability probing (which would require sending escape sequences and reading responses, with timeout handling). Environment variable checks are instant and reliable for the terminals we target.

### Why Kitty sends full-resolution images

For Kitty protocol, we send the original image and let the terminal scale it via the `c` (columns) and `r` (rows) parameters. This avoids lossy double-resize (once in Rust, once in the terminal) and produces the sharpest result at the terminal's native pixel density. The trade-off is slightly more data over the wire, but PNG encoding keeps the payload reasonable.

### Synchronized output for Kitty/Sixel

Graphics protocol output is wrapped in synchronized update markers (`\x1B[?2026h` / `\x1B[?2026l`) so the terminal treats the ratatui cell clear + image data as one atomic frame. Without this, there's a visible flash between clearing the old image and rendering the new one.

## Storyboard & Video Frame Modes

### Three frame display modes

1. **Thumbnail** — Static image, no extra work.
2. **Storyboard** — YouTube's built-in sprite sheets (~320x180, one frame every ~2s). Fast, no ffmpeg required. Fetched via `yt-dlp --dump-json` which includes storyboard URLs.
3. **Video** — ffmpeg extracts frames from the video stream (640x360, 0.5 fps). Progressive: frames appear on disk as ffmpeg processes. Higher quality but requires ffmpeg.

### Why keep storyboard alongside video frames

Storyboard mode is faster to start (sprite sheet URLs are in yt-dlp's JSON output, downloaded in parallel) and doesn't require ffmpeg as a dependency. For users who just want visual progress indication, storyboard is good enough. Video frames are for users who want higher fidelity.

Users cycle modes with Ctrl+F. The selection persists to `prefs.toml`.

### Sprite sheet frame extraction

YouTube storyboard sprite sheets are grids of small thumbnails (e.g., 5x5 = 25 frames per sheet). `SpriteFrameSource` tracks the grid dimensions, FPS, and per-fragment durations. Given a playback time, it:

1. Walks the fragment durations to find which sprite sheet covers that time
2. Computes the local frame index within that sheet
3. Uses `crop_imm()` to extract the individual frame

This avoids decoding every frame individually — the sprite sheet is decoded once and frames are cropped on demand.

## Channel Browsing & Pagination

### Two listing modes

- **Search mode**: `ytsearch20:<query>` — returns up to 20 results with full metadata.
- **Channel mode**: Detected via `@handle`, YouTube URLs, or `/channel` prefix. Uses `--flat-playlist` for fast initial fetch (30 videos), then paginates in pages of 20.

### Why `--flat-playlist` + background enrichment

Channel listing with `--flat-playlist` returns titles and IDs in ~2s. Full metadata (dates, tags) requires per-video `--skip-download --print` calls that take ~1s each. Rather than blocking the UI for 30+ seconds, we:

1. Show results immediately (title + ID only)
2. Spawn background enrichment tasks (5 concurrent yt-dlp processes)
3. Progressively update entries as metadata arrives

This gives the user something to browse immediately while metadata fills in.

### Pagination trigger

When the user scrolls within 5 items of the bottom of the list, `trigger_load_more()` fires a background fetch for the next 20 videos. The `ChannelSource` struct tracks `total_fetched`, `has_more`, and `loading_more` to prevent duplicate requests and detect end-of-channel.

## Filter System

### In-place filtering with `/`

The filter narrows visible results by matching against title and tags (case-insensitive). It uses an indirection layer (`filtered_indices: Vec<usize>`) that maps visible list positions to actual `search_results` indices. This avoids copying or mutating the search results.

### Keyword highlighting

`highlight_text()` in `ui.rs` performs case-insensitive substring matching and returns a `Vec<Span>` that interleaves normal and highlighted segments. It works on char indices (not byte indices) for Unicode safety, with a fallback to no highlighting if `to_lowercase()` changes the char count (e.g., Turkish İ).

## Auto-Transcription Pipeline

### Why whisper-cli-rs as a library (not a subprocess)

The original implementation used `sox` for recording + `whisper-cli` as a subprocess. This was replaced with `whisper-cli-rs` as a Rust library dependency for:

1. **No subprocess overhead**: Whisper model stays loaded in-process
2. **Structured output**: Direct access to `Utternace` structs with timestamps, instead of parsing CLI text output
3. **Cancellation**: `JoinHandle::abort()` can cancel the task; killing a subprocess is less clean

### Three-stage pipeline

When a track starts playing, `trigger_transcription()` runs:

1. **Extract audio**: `yt-dlp -x --audio-format wav` to a temp file (16kHz mono, as whisper expects)
2. **Download model** (if needed): Our own download with progress events, bypassing whisper-cli-rs's indicatif progress bar
3. **Transcribe**: `whisper-cli-rs` via `spawn_blocking` with stdout/stderr suppression

All three stages run in a single `tokio::spawn()` task, communicating back to the UI via `mpsc::unbounded_channel<TranscriptEvent>`.

### Why we download the model ourselves

`whisper-cli-rs` uses `indicatif` for download progress, which writes directly to stdout and corrupts the TUI. By downloading the model ourselves before calling `Whisper::new()`, the internal download is skipped (file already exists). Our download sends `DownloadProgress(downloaded, total)` events that the TUI renders as a `Gauge` widget.

The download writes to a `.bin.part` temp file, then does an atomic rename to the final path. Progress events are throttled to every 100ms to avoid overwhelming the event loop.

### Why `SuppressStdio` exists

The whisper.cpp C library (which whisper-cli-rs wraps) writes logging directly to file descriptors 1 and 2 (stdout/stderr) via C `printf`/`fprintf`. This bypasses Rust's `std::io` buffering and `tracing` infrastructure, and corrupts the TUI's raw-mode terminal output.

`SuppressStdio` is an RAII guard that:
1. Saves the current fd 1 and fd 2 with `libc::dup()`
2. Opens `/dev/null` and redirects fd 1/2 to it with `libc::dup2()`
3. On drop, restores the original file descriptors

This approach operates at the file descriptor level, which is the only way to intercept C library output. Rust's `std::io::set_output_capture` only captures Rust's own print macros.

### Why a nested tokio runtime inside `spawn_blocking`

`Whisper::new()` is async (it calls `model.download().await` internally). But the transcription via `whisper.transcribe()` is synchronous and CPU-intensive, so it must run in `spawn_blocking`. Inside `spawn_blocking`, there's no tokio runtime context, so we create a `tokio::runtime::Builder::new_current_thread()` to execute the async `Whisper::new()` call. This is safe because:

- The nested runtime is single-threaded and short-lived
- It only runs `Whisper::new()` (which does a no-op download check since we pre-downloaded)
- It's dropped before the synchronous transcription begins

### Time-synced transcript highlighting

Utterance timestamps from whisper-cli-rs are in centiseconds (`i64`). The current mpv playback position is parsed from the status string (`"Time: MM:SS / ..."`) and converted to centiseconds. The active utterance is found with a linear scan (`position(|u| t >= u.start && t < u.stop)`).

Active utterances are styled with `theme.highlight_fg` + `theme.highlight_bg` + bold. Inactive utterances use `theme.muted`. The transcript auto-scrolls to center the active utterance in the visible area.

## Async Architecture

### Event loop design

The main loop is a simple poll-based pattern:

```
loop {
    check_pending()    // Poll all async task receivers
    check_mpv_status() // Drain mpv stdout channel
    update_frames()    // Sync frame source with playback time
    draw()             // Render TUI
    poll(100ms)        // Wait for keyboard input or timeout
}
```

100ms poll timeout gives responsive UI updates (10 fps) while keeping CPU usage minimal when idle.

### Why oneshot channels for request/response tasks

Search, load, and "load more" use `oneshot::Receiver<Result<T>>` because they're single-response operations. The spawned task sends one result and is done. `check_pending()` calls `try_recv()` on each — if `Empty`, it puts the receiver back; if `Ok`, it processes the result.

### Why mpsc for streaming data

mpv status monitoring and video enrichment use `mpsc::Receiver` because they produce multiple values over time. The enrichment channel has capacity 64 (backpressure if the UI can't keep up). The mpv status channel has capacity 10 (only the latest status matters, old ones are drained).

The transcript pipeline uses `mpsc::unbounded_channel` because events are infrequent (at most ~5-6 per transcription: extraction done, download progress updates, transcription done) and must never be dropped.

## Theme System

### 12 built-in themes, no config file themes

Themes are `const` — compiled into the binary. This ensures every theme is always valid (no parsing errors at runtime) and makes cycling instantaneous. The current theme is persisted to `prefs.toml` by name.

Each theme has 15 color fields covering all UI elements: background, foreground, accent, muted text, borders, errors, status bar, selection highlight (bg + fg), list stripe, keyboard shortcut display (bg + fg), tags, and panel backgrounds.

### Thumbnail glow border

The thumbnail pane has a rounded border in a dimmed accent color (`dim_color(theme.accent, 0.35)`). This creates a subtle "glow" effect that visually separates the thumbnail from the rest of the UI without being distracting. The dimming function scales RGB components by a factor, returning non-RGB colors unchanged.

## Error Handling Philosophy

### No `unwrap()` in non-test code

Per `AGENTS.md`, the codebase uses `?` with `anyhow::Context` for all fallible operations. `.expect()` is permitted only when the invariant is logically guaranteed, with a safety comment explaining why.

### Graceful degradation

Many features degrade gracefully instead of failing hard:

- **Frame sources**: If storyboard/video frame fetch fails, the static thumbnail continues to work.
- **Enrichment**: If per-video metadata fetch fails, the entry keeps its title and ID.
- **Transcription**: If any stage fails, an error message is shown but playback continues.
- **Thumbnail fetch**: Tries maxresdefault → sddefault → hqdefault → 0.jpg in sequence.
- **Display mode**: Falls back from Kitty → Sixel → Direct → ASCII based on terminal capabilities.

### Terminal safety

Terminal state (raw mode, alternate screen) must always be restored on exit. This is handled by:

1. `ratatui::init()` / `ratatui::restore()` in `main()`
2. A panic hook that calls `ratatui::restore()` before the default hook
3. mpv process cleanup in `MusicPlayer::stop()` (IPC socket removal, process kill)

## Logging

### Daily rolling file appender

yp uses `tracing` with `tracing-appender::rolling::daily` for structured logging to `~/Library/Application Support/yp/logs/yp.log.YYYY-MM-DD`. On startup, old log files (not matching today's date) are removed.

Console logging is not used because stdout/stderr are occupied by the TUI. File logging is essential for debugging async operations, yt-dlp failures, and mpv communication issues.

## Notable Trade-offs

| Decision | Benefit | Cost |
|---|---|---|
| yt-dlp subprocess | No API key, handles anti-bot | ~1-3s latency per call |
| mpv subprocess | Handles all codecs/formats | External dependency |
| whisper-cli-rs library | Structured output, cancellation | Compiles whisper.cpp (slow build), CMake dependency |
| SuppressStdio fd redirect | Only way to silence C logging | Uses unsafe libc calls |
| Pre-download model ourselves | TUI progress bar, no indicatif corruption | Duplicates download logic |
| Nested tokio runtime | Bridges async model init + sync transcription | Slightly unusual pattern |
| `--flat-playlist` + enrichment | Fast initial channel load | Metadata arrives progressively |
| Full-resolution Kitty images | Sharpest at native pixel density | More data over pipe |
| const themes | Always valid, instant cycling | No user-defined themes |
