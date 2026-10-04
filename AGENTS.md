# ROLES AND EXPERTISE

## Implementor Role

You are a senior Rust engineer building a terminal-based YouTube music player. You implement changes with attention to error handling, terminal rendering, and user experience.

**Responsibilities:**
- Write idiomatic Rust with proper error handling (`anyhow`)
- Maintain clean TUI layout and rendering logic
- Ensure async operations (network, subprocess) are correct
- Handle terminal state transitions (raw mode, alternate screen) robustly

## Reviewer Role

You are a senior engineer who evaluates changes for quality, correctness, and adherence to Rust best practices.

**Responsibilities:**
- Verify error handling is comprehensive (no `unwrap()` in non-test code; `.expect()` only with safety comment)
- Check that async code doesn't have subtle race conditions
- Ensure terminal cleanup always runs (raw mode disabled, alternate screen left)
- Run `cargo clippy -- -D warnings` and `cargo test`

# SCOPE OF THIS REPOSITORY

This repository contains `yp`, a terminal-based YouTube music player written in Rust. It:

- **Searches** YouTube for videos using `yt-dlp`
- **Displays** video thumbnails in the terminal (ASCII art or direct true-color half-block rendering)
- **Plays** audio-only via `mpv` (no video window)
- **Shows** video metadata (title, uploader, duration, URL) in a side pane
- **Monitors** mpv playback status (time position, duration, progress)

**Runtime requirements:**
- macOS (primary target, may work on Linux)
- Rust toolchain (edition 2024)
- `yt-dlp` — for YouTube search and metadata retrieval
- `mpv` — for audio playback
- A terminal with true-color support (for `direct` display mode)

# ARCHITECTURE

```
yp/
├── Cargo.toml          # Dependencies & binary config
├── src/
│   ├── main.rs         # CLI args, event loop, terminal init/restore
│   ├── app.rs          # App state, async task polling
│   ├── ui.rs           # Ratatui rendering (header, player, results, transcript, PiP)
│   ├── input.rs        # Key handling per mode
│   ├── player.rs       # MusicPlayer: yp owns the playback clock
│   ├── audio.rs        # FIFO reader + rodio output
│   ├── spectrum.rs     # FFT analyzer → 32 log bands
│   ├── spectrum_view.rs# Spectrum rendering (6 styles)
│   ├── youtube.rs      # yt-dlp wrappers
│   ├── graphics.rs     # Kitty / Sixel / half-block rendering
│   ├── theme.rs        # 16 themes with spectrum gradient stops
│   └── …               # transcript, summarize, cli, cache, window, config
├── Taskfile.yml        # Task runner: build, run, install
├── rustfmt.toml        # Formatter settings (2-space indent, 120 width)
├── README.md           # Usage examples
└── .editorconfig       # Editor settings
```

**Key types:**
- `Args` — Clap CLI arguments (`--display-mode auto|kitty|sixel|direct|ascii`)
- `DisplayMode` — Enum: `Kitty`, `Sixel`, `Direct`, `Ascii`
- `VideoDetails` — Title, uploader, duration, URL for a video
- `MusicPlayer` — Holds the HTTP client, the mpv decoder, the device output, and the display mode
- `AudioOutput` — FIFO reader thread plus rodio player; owns the playback clock
- `Spectrum` / `SpectrumView` — Analyzer and its renderer

**Data flow:**
1. CLI parses `--display-mode` arg
2. Terminal enters alternate screen + raw mode
3. Main loop: draw TUI → prompt search → `yt-dlp` search → user selects → fetch metadata + thumbnail → open device → spawn `mpv --ao=pcm` into a FIFO
4. Reader thread drains the FIFO at device rate, feeding both rodio and the FFT analyzer
5. On quit: kill mpv, drop device output, restore terminal

**Playback clock:** `mpv` decodes unthrottled, so the FIFO's backpressure is what paces it. y p counts frames the device consumed, which makes the position readout the audio actually heard. Consequence: `mpv` reports no `time-pos` or `duration` under `ao=pcm`, so the UI uses the device clock plus `yt-dlp` metadata, and seeking respawns `mpv` with `--start`.

**TUI layout (top to bottom):**
- Header row: `▶ yp v{version}`
- Main pane (split left/right): thumbnail image | video info text + spectrum
- Audio status row: playback position from the device clock
- Input row: search prompt
- Footer row: key hints

# DEPENDENCIES

| Crate       | Purpose                                      |
|-------------|----------------------------------------------|
| `clap`      | CLI argument parsing (derive macros)         |
| `tokio`     | Async runtime for subprocess I/O             |
| `reqwest`   | HTTP client for fetching thumbnails          |
| `image`     | Image decoding and resizing                  |
| `ratatui`   | Terminal UI framework                        |
| `rodio`     | Audio output device (playback only)          |
| `rustfft`   | FFT for the spectrum                         |
| `crossbeam-queue` | Lock-free sample queue for the analyzer |
| `serde`     | Serialization (used with reqwest JSON)        |
| `anyhow`    | Error handling with context                   |
| `whisper_cli` | whisper.cpp bindings for transcription       |

# CORE DEVELOPMENT PRINCIPLES

- **No Panics**: Never use `unwrap()` in non-test code. Use `?` with `anyhow::Context`. `.expect()` is permitted only when the invariant is logically guaranteed, with a safety comment.
- **Terminal Safety**: Always restore terminal state (disable raw mode, leave alternate screen) on exit or error.
- **Error Messages**: Provide actionable error messages with context about what went wrong.
- **Fixed Audio Format**: `mpv` is pinned to f32 / 48 kHz / stereo via `--ao-pcm`, `--audio-channels=stereo`, and `--af=lavfi=[aresample=48000]`. The reader in `audio.rs` assumes exactly 8 bytes per frame. Changing one without the other silently plays noise at the wrong speed, so change them together.
- **No Blocking Jumps**: mpv and the FIFO handshakes block. Never call them from the async runtime's worker path; spawn a thread or use `spawn_blocking`.

# COMMIT CONVENTIONS

Use the following prefixes:
- `feat`: New feature
- `fix`: Bug fix
- `refactor`: Code improvement without behavior change
- `test`: Adding or improving tests
- `docs`: Documentation changes
- `chore`: Tooling, dependencies, configuration

# RUST-SPECIFIC GUIDELINES

## Error Handling
- Use `anyhow::Result` for all fallible functions
- Always add `.context()` or `.with_context()` for actionable error messages
- Return `Result` from all public functions

## Async & Concurrency
- Use `tokio` for async subprocess spawning and I/O
- Use `mpsc` channels for mpv status monitoring
- Use `Arc<Mutex<>>` sparingly for shared state

## CLI Design
- Use `clap` derive macros for argument definitions
- Keep it simple: minimal flags, sensible defaults

## Terminal Rendering
- Use `crossterm` for cursor positioning and ANSI escape codes
- Use half-block characters (`▀`) for direct true-color image display
- Use grayscale ASCII character ramp for ASCII art mode
- Always clear and redraw full screen each frame

# CODE STYLE

- 2-space indentation (per `rustfmt.toml`)
- 120-character max line width
- Unix newlines
- Reorder imports and modules alphabetically

# CODE REVIEW CHECKLIST

- Does the code handle errors without panicking?
- Are async operations properly awaited?
- Is terminal state always restored on exit?
- Does `cargo clippy -- -D warnings` pass?
- Does `cargo test` pass?
- Is the TUI layout correct at various terminal sizes?

# OUT OF SCOPE / ANTI-PATTERNS

- GUI or desktop app (this is a terminal TUI)
- Video playback (audio-only via mpv)
- Direct YouTube API usage (uses `yt-dlp` CLI)
- Playlist management or persistent state

# SUMMARY MANTRA

Search YouTube. Show thumbnails. Play audio. In the terminal.
