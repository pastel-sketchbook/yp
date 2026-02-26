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
│   └── main.rs         # Single-file app: CLI, TUI, player, search
├── Taskfile.yml        # Task runner: build, run, install
├── rustfmt.toml        # Formatter settings (2-space indent, 120 width)
├── README.md           # Usage examples
└── .editorconfig       # Editor settings
```

**Key types (all in `main.rs`):**
- `Args` — Clap CLI arguments (`--display-mode ascii|direct`)
- `DisplayMode` — Enum: `Ascii` or `Direct` rendering
- `VideoDetails` — Title, uploader, duration, URL for a video
- `MusicPlayer` — Core struct: holds HTTP client, mpv process, display mode, mpv status

**Data flow:**
1. CLI parses `--display-mode` arg
2. Terminal enters alternate screen + raw mode
3. Main loop: draw TUI → prompt search → `yt-dlp` search → user selects → fetch metadata + thumbnail → spawn `mpv --no-video` → monitor stdout for status
4. On quit: kill mpv, restore terminal

**TUI layout (top to bottom):**
- Header row: `▶ yp v{version}`
- Main pane (split left/right): thumbnail image | video info text
- Audio status row: mpv playback position
- Input row: search prompt (via `dialoguer`)
- Footer row: `[q] Quit`

# DEPENDENCIES

| Crate       | Purpose                                      |
|-------------|----------------------------------------------|
| `clap`      | CLI argument parsing (derive macros)         |
| `tokio`     | Async runtime for subprocess I/O             |
| `reqwest`   | HTTP client for fetching thumbnails          |
| `image`     | Image decoding and resizing                  |
| `crossterm` | Terminal manipulation (cursor, colors, raw)  |
| `dialoguer` | Interactive text input and selection prompts  |
| `textwrap`  | Word-wrapping text to fit pane widths         |
| `termsize`  | Terminal dimension detection                  |
| `serde`     | Serialization (used with reqwest JSON)        |
| `anyhow`    | Error handling with context                   |
| `urlencoding` | URL encoding for search queries             |

# CORE DEVELOPMENT PRINCIPLES

- **No Panics**: Never use `unwrap()` in non-test code. Use `?` with `anyhow::Context`. `.expect()` is permitted only when the invariant is logically guaranteed, with a safety comment.
- **Terminal Safety**: Always restore terminal state (disable raw mode, leave alternate screen) on exit or error.
- **Error Messages**: Provide actionable error messages with context about what went wrong.
- **Single File**: The app currently lives in a single `main.rs`. Extract modules only when complexity warrants it.

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
