---
name: yp
description: Deep knowledge of the yp codebase — architecture, module responsibilities, data flow, conventions, and common implementation patterns for this terminal-based YouTube music player written in Rust.
---

# yp — Terminal YouTube Player Skill

> Search YouTube. Show thumbnails. Play audio. Transcribe speech. All in the terminal.

## What is yp?

`yp` is a Rust TUI + CLI application (v0.10.3, edition 2024) that serves two purposes:

1. **TUI mode** (bare `yp`) — interactive YouTube search, channel browsing, audio playback via mpv, thumbnail display (Kitty/Sixel/half-block/ASCII), live whisper transcription, 12 themes, PiP mode
2. **CLI mode** (`yp search`, `yp channel`, `yp info`, `yp transcript`, `yp summarize`) — JSON/JSONL output for machine consumption, pipe-composable with `fzf`, `jq`, LLMs

## Runtime Dependencies

| Tool | Purpose | Install |
|------|---------|---------|
| `yt-dlp` | YouTube search, metadata, stream URL resolution | `brew install yt-dlp` |
| `mpv` | Audio-only playback (no video window) | `brew install mpv` |
| `ffmpeg` | Video frame extraction, audio chunk download for transcription | `brew install ffmpeg` (optional) |
| `whisper.cpp` | Speech-to-text engine (via `whisper_cli` crate) | Auto-downloaded `ggml-small.bin` (~460 MB) on first use |

## Project Layout

```
yp/
├── Cargo.toml           # Dependencies & binary config
├── constants.ron         # Compile-time constants (RON format, embedded via include_str!)
├── rustfmt.toml          # 2-space indent, 120 width, Unix newlines
├── Taskfile.yml          # build, run, install, version bump tasks
├── AGENTS.md             # AI agent instructions
├── docs/rationale/       # Design decision documents (ADR-style)
└── src/
    ├── main.rs           # CLI args, event loop, terminal init/restore
    ├── app.rs            # App struct, state management, async task polling
    ├── ui.rs             # Ratatui widget rendering (header, player, results, transcript, PiP)
    ├── input.rs          # Keyboard event handling per mode (Input, Results, Filter)
    ├── player.rs         # MusicPlayer: mpv subprocess, IPC, pause/stop
    ├── youtube.rs        # yt-dlp wrappers: search, channel listing, video info, thumbnails, frame sources
    ├── display.rs        # Display mode detection (Kitty > Sixel > Direct > ASCII), tmux support
    ├── graphics.rs       # Image rendering: ThumbnailWidget, Kitty protocol, Sixel protocol
    ├── theme.rs          # 12 Theme structs (6 dark + 6 light) with 14 color fields
    ├── config.rs         # Preferences persistence (prefs.toml via directories crate)
    ├── constants.rs      # LazyLock<Constants> from embedded constants.ron
    ├── transcript.rs     # Chunked whisper transcription pipeline (resolve URL → download → transcribe)
    ├── summarize.rs      # Classify-reduce pipeline (NonSpeech/Filler/Repetition/TopicShift/KeySegment)
    ├── cli.rs            # CLI subcommand handlers (search, channel, info, transcript, summarize)
    ├── cache.rs          # Video ID cache for zsh tab completion (TSV file, deduped, 2000 max)
    └── window.rs         # macOS window manipulation (PiP via AppleScript/System Events)
```

## Architecture Deep Dive

### Core Types

| Type | File | Purpose |
|------|------|---------|
| `App` | `app.rs` | Central state: input, mode, search results, player, themes, transcript, PiP |
| `MusicPlayer` | `player.rs` | mpv process lifecycle, IPC socket, status monitoring |
| `VideoDetails` | `player.rs` | Metadata for Now Playing (title, uploader, duration, tags, URL) |
| `SearchEntry` | `youtube.rs` | Search/channel result item (title, video_id, optional metadata) |
| `VideoMeta` | `youtube.rs` | Enriched metadata from per-video yt-dlp calls |
| `FrameSource` | `youtube.rs` | Enum: `Sprites(SpriteFrameSource)` or `Video(VideoFrameSource)` |
| `Theme` | `theme.rs` | 14-field color scheme (bg, fg, accent, muted, border, error, etc.) |
| `Config` | `config.rs` | Persisted prefs: theme_name, frame_mode |
| `Constants` | `constants.rs` | Compile-time tunables from `constants.ron` |
| `TranscriptState` | `transcript.rs` | State machine: Idle → ExtractingAudio → Transcribing → Ready |
| `ClassifiedUtterance` | `summarize.rs` | Utterance with tag (NonSpeech/Filler/Repetition/TopicShift/KeySegment/Normal) |
| `WindowGeometry` | `window.rs` | Pixel position + size for PiP |

### App Modes

```
AppMode::Input    ←→  AppMode::Results  ←→  AppMode::Filter
    ↑                      ↓                      ↓
    └──────────────────────┘──────────────────────┘
```

- **Input**: Search bar focused, typing queries or `@channel` handles
- **Results**: List focused, j/k navigation, Enter to play, `/` to filter
- **Filter**: Filter bar focused, real-time title/tag filtering with keyword highlighting

### Data Flow: Search → Play

```
1. User types query → trigger_search()
2. Detect channel URL vs regular search → spawn yt-dlp as oneshot
3. Poll search_rx in check_pending() → populate search_results
4. User selects entry → trigger_load()
5. Spawn parallel: get_video_info() + fetch_thumbnail()
6. Poll load_rx → player.play(details) spawns mpv --no-video
7. Auto-trigger transcription pipeline (resolve URL → chunk → whisper)
8. Monitor mpv stdout for status: time position, duration, progress
9. Update frame source (storyboard/video) based on playback position
```

### Data Flow: Transcription Pipeline

```
1. trigger_transcription(url) → spawn_transcription_pipeline()
2. resolve_stream_url():
   a. Fast path: query mpv IPC socket for stream-open-filename (~0.5-4s)
   b. Fallback: yt-dlp -g --format bestaudio (~10-30s)
3. Download whisper model if needed (ggml-small.bin, progress bar in TUI)
4. Loop:
   a. ffmpeg -ss offset -t 30s → download 30s WAV chunk
   b. spawn_blocking: whisper.transcribe(chunk)
   c. Send ChunkTranscribed(utterances) with adjusted timestamps
5. Send Transcribed when all chunks done
```

### Data Flow: PiP Mode

```
1. Ctrl+M → toggle_pip()
2. If fullscreen: exit_fullscreen() → wait for macOS animation
3. Save original geometry → compute pip_geometry() (bottom-right, 550×350)
4. set_window_geometry() via AppleScript (Terminal.app, iTerm2, or Ghostty System Events)
5. Render compact layout: status bar + thumbnail + [Ctrl+M] hint
6. On restore: set_window_geometry(original) → re-enter fullscreen if needed
```

### Display Mode Detection

```
detect_display_mode() probe order:
1. Kitty: TERM=xterm-kitty, KITTY_WINDOW_ID, TERM_PROGRAM=kitty/wezterm/ghostty, GHOST_TERMINAL=1
2. Tmux: walk tmux client process ancestry looking for kitty/ghostty/wezterm
3. Sixel: TERM_PROGRAM=foot/mlterm/contour, TERM contains "sixel"
4. Direct: COLORTERM=truecolor/24bit → half-block characters (▀)
5. ASCII: fallback grayscale character ramp
```

### Graphics Protocols

- **Kitty**: APC escape sequences, PNG encoded, base64 chunked (4096 bytes), `i=1 p=1` for atomic replacement, tmux passthrough wrapping
- **Sixel**: DCS escape sequences, NeuQuant color quantization (256 colors), RLE compression
- **Direct**: Half-block characters (`▀`) with fg/bg true-color
- **ASCII**: Grayscale ramp: `" ", ".", ":", "-", "=", "+", "*", "#", "%", "@"`

### Synchronized Update Protocol

The main loop wraps graphics protocol output in `\x1B[?2026h` / `\x1B[?2026l` markers (synchronized update) so the terminal renders ratatui cell updates + image data as one atomic frame, preventing visible flicker.

### Theme System

12 themes (6 dark, 6 light): Default, Gruvbox, Solarized, Flexoki, Ayu, Zoegi + their light variants, plus FFE dark and FFE Light. Each theme has 14 color fields. Cycle with `Ctrl+T`, persisted in `prefs.toml`.

### Summarize Pipeline (CLI)

```
classify(utterances) → tag each as NonSpeech/Filler/Repetition/TopicShift/KeySegment/Normal
reduce(video, classified) → SummaryOutput:
  - Suppress noise (NonSpeech, Filler, Repetition)
  - Group into TopicSegments (split at TopicShift boundaries, max 30)
  - Extract KeySegments (max 50)
  - Compute stats: filler_ratio, non_speech_secs, time_range
```

### CLI Pipe Composability

All subcommands output JSON/JSONL to stdout, progress/errors to stderr:
```bash
yp channel @Handle | fzf | yp summarize    # browse → select → summarize
yp channel --fast | jq -r '.video_id' | head -1 | xargs yp summarize
yp summarize --latest | opencode run "What is this video about?"
```

### Shell Completions

Custom zsh completion script with dynamic video ID support:
- `yp _complete-ids` reads from local cache (`~/Library/Caches/yp/videos.tsv`)
- Cache populated by `channel`, `search`, `info` commands
- `eval "$(yp completions zsh)"` to install

## Key Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `ratatui` | 0.30.0 | TUI framework (crossterm backend) |
| `clap` | 4.6.0 | CLI argument parsing (derive macros) |
| `tokio` | 1.50.0 | Async runtime (full features) |
| `reqwest` | 0.13.2 | HTTP client (thumbnails, sprite sheets, model download) |
| `image` | 0.25.10 | Image decoding, resizing (Lanczos3), color conversion |
| `serde` / `serde_json` | 1.0.x | JSON serialization for CLI output |
| `ron` | 0.12.0 | Constants file format |
| `tracing` / `tracing-appender` | 0.1.x | Daily file logging to `~/Library/Application Support/yp/logs/` |
| `whisper_cli` | git | Whisper.cpp Rust bindings for transcription |
| `base64` | 0.22.1 | Kitty protocol image encoding |
| `color_quant` | 1.1.0 | NeuQuant quantization for Sixel protocol |
| `futures` | 0.3.32 | Stream combinators for concurrent enrichment |
| `unicode-width` | 0.2.2 | CJK-aware display width calculation |
| `libc` | 0.2.x | stderr suppression for whisper.cpp C library logging |

## Coding Conventions

### Error Handling
- Use `anyhow::Result` with `.context()` / `.with_context()` everywhere
- **No `unwrap()`** in non-test code
- `.expect()` only with a `// Safety:` comment explaining the invariant
- Actionable error messages: "yt-dlp not found. Install with: brew install yt-dlp"

### Async Patterns
- `oneshot::channel` for single-result async tasks (search, load, frame source)
- `mpsc::channel` for streaming results (enrichment, transcript events)
- `JoinHandle` stored in `AsyncTasks` for cancellation via `.abort()`
- `try_recv()` polling in `check_pending()` — non-blocking, called every frame

### Terminal Safety
- Panic hook calls `ratatui::restore()` before propagating
- Exit path always: kill mpv → restore PiP → `ratatui::restore()`
- mpv stderr sent to `Stdio::null()` to prevent pipe buffer deadlock

### Code Style
- 2-space indentation, 120-char max width, Unix newlines (per `rustfmt.toml`)
- Imports reordered alphabetically
- `pub(crate)` for internal API boundaries between modules
- Test modules at bottom of each file with `#[cfg(test)]`

### Commit Prefixes
`feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`

## Common Patterns

### Adding a New CLI Subcommand
1. Add variant to `Command` enum in `main.rs`
2. Add match arm in `main()` dispatching to `cli::cmd_*`
3. Implement handler in `cli.rs` — output JSON to stdout, progress to stderr
4. Update zsh completion script in `cli::generate_zsh_completions()`

### Adding a New Theme
1. Add `Theme { name: "...", ... }` to the `THEMES` array in `theme.rs`
2. No other changes needed — `next_theme()` cycles automatically

### Adding a New Keyboard Shortcut
1. Check for global shortcuts first (Ctrl+C, Ctrl+T, etc.) at top of `handle_key_event()` in `input.rs`
2. Add mode-specific handling in `handle_input_key()`, `handle_results_key()`, or `handle_filter_key()`
3. Add footer hint in `render_footer()` in `ui.rs`

### Adding a New Async Operation
1. Add a `oneshot::Receiver` field to `AsyncTasks` in `app.rs`
2. Spawn with `tokio::spawn` and send result via `oneshot::channel::tx`
3. Poll in `check_pending()` with `try_recv()` pattern
4. Set `status_message` before spawning, clear on completion

### Adding a New Display Mode
1. Add variant to `DisplayMode` enum in `display.rs`
2. Add detection logic in `detect_display_mode()`
3. Add rendering branch in `graphics.rs` (`ThumbnailWidget::render()` or separate function)
4. Add `CliDisplayMode` variant in `display.rs`
5. Handle in `resolve_display_mode()` and the main loop's graphics protocol section

### Adding Constants
1. Add field to the `Constants` struct in `constants.rs`
2. Add the value in `constants.ron` with a comment
3. Access via `constants().field_name`

## Build & Run

```bash
# Development
cargo run                    # auto-detect display mode
cargo run -- -d kitty        # force Kitty mode

# Release
task build                   # cargo fmt + cargo build --release
task run                     # build + run release binary
task install                 # copy to ~/bin

# Quality
cargo clippy -- -D warnings  # must pass with zero warnings
cargo test                   # unit tests in all modules

# Version management
task version:patch           # bump 0.0.x
task version:minor           # bump 0.x.0
task version:tag             # git tag from Cargo.toml version
```

## Config & Data Paths

| Path | Purpose |
|------|---------|
| `~/Library/Application Support/yp/prefs.toml` | Theme + frame mode preferences |
| `~/Library/Application Support/yp/logs/yp.log.YYYY-MM-DD` | Daily log files (old ones auto-deleted) |
| `~/Library/Caches/yp/videos.tsv` | Video ID cache for shell completions |
| `/tmp/yp-mpv-{pid}.sock` | mpv IPC socket (cleaned on exit) |
| `/tmp/yp-chunk-{pid}.wav` | Temporary audio chunk for whisper (cleaned after use) |
| `/tmp/yp-frames-{pid}-{video_id}/` | Temporary video frames directory (cleaned on drop) |

## Known Issues

- **Ghostty PiP resize**: System Events `set position`/`set size` returns success but doesn't visibly resize the Ghostty window. Under investigation — may need AX API or Swift helper. (See `TODO.md` #7)

## Anti-Patterns to Avoid

- **No GUI or desktop app** — this is a terminal TUI
- **No video playback** — audio-only via mpv
- **No direct YouTube API** — always use yt-dlp CLI
- **No playlist management** — stateless, search-and-play
- **No `unwrap()` in production code** — use `?` with context
- **No piped stderr for mpv** — causes buffer deadlock, must use `Stdio::null()`
