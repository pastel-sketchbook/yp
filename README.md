# yp

A TUI YouTube player with image thumbnails, live transcription, channel browsing, and 12 themes -- all in the terminal. Also a CLI for machines: search, browse channels, transcribe, and summarize videos as JSON for LLM pipelines.

## Features

- **Search & play** -- search YouTube or browse channels, play audio via mpv
- **Spectrum** -- live FFT of the playing audio in the Now Playing pane, gradient by default, six styles via `Ctrl+V`
- **Thumbnails** -- Kitty, Sixel, half-block, or ASCII art (auto-detected)
- **Frame modes** -- static thumbnail, storyboard animation, or live video frames
- **Transcription** -- automatic speech-to-text via whisper.cpp, time-synced to playback
- **Channel browsing** -- enter `@handle` or a channel URL to list videos with paginated loading
- **Filter** -- press `/` to filter results by title or tags with keyword highlighting
- **12 themes** -- 6 dark, 5 light, cycle with `Ctrl+T`, persisted across sessions
- **Preferences** -- theme and frame mode saved to `prefs.toml`
- **CLI subcommands** -- JSON output for search, channel, info, transcript, summarize
- **Pipe-first** -- `yp channel | fzf | yp summarize` composes with Unix tools and LLMs

## Dependencies

```bash
brew install yt-dlp mpv
```

Optional, for video frame mode:

```bash
brew install ffmpeg
```

### Transcription

Auto-transcription uses [whisper-cli-rs](https://github.com/m1guelpf/whisper-cli-rs) by [Miguel Piedrafita](https://github.com/m1guelpf) for speech-to-text via the whisper.cpp engine. The whisper model (`ggml-small.bin`, ~460 MB) is downloaded automatically on first use.

## Usage

```bash
# Auto-detect best display mode (default)
cargo run

# Force a specific display mode
cargo run -- -d kitty
cargo run -- -d sixel
cargo run -- -d direct
cargo run -- -d ascii
```

### Display modes

| Mode | Description |
|------|-------------|
| `auto` | Auto-detect best mode for your terminal (default) |
| `kitty` | Kitty graphics protocol (kitty, WezTerm, ghostty) |
| `sixel` | Sixel graphics (foot, mlterm, contour) |
| `direct` | True-color half-block characters |
| `ascii` | Grayscale ASCII art (works everywhere) |

### Keyboard shortcuts

| Key | Action |
|-----|--------|
| `Enter` | Search / play selected |
| `j` / `k` | Navigate results |
| `/` | Filter results by title or tags |
| `Space` | Pause / resume |
| `←` / `→` | Seek back / forward 10s |
| `Ctrl+A` | Toggle transcript / cancel transcription |
| `Ctrl+T` | Cycle theme |
| `Ctrl+F` | Cycle frame mode (thumbnail / storyboard / video) |
| `Ctrl+V` | Cycle spectrum style |
| `Ctrl+S` | Stop playback |
| `Ctrl+O` | Open video in browser |
| `Esc` | Back / clear / quit |

### Channel browsing

Type a `@handle`, channel URL, or `/channel <name>` in the search bar to browse a channel's videos. Results load in pages as you scroll.

### Spectrum

The Now Playing pane shows a live spectrum of the audio you are hearing, with no setup — it appears as soon as a track is playing. `Ctrl+V` cycles six styles: `gradient` (default), `bars`, `mono`, `mirror`, `dots`, `waterfall`. The choice is saved to `prefs.toml`.

The bars are a real FFT of the samples reaching the sound device, not a decoration. `mpv` decodes to a FIFO instead of the sound card and runs ahead as fast as it can; yp drains that FIFO at exactly device rate, which paces `mpv` to real time and makes the position readout the audio actually heard. Seeking therefore restarts `mpv` with `--start`, since a FIFO cannot be rewound.

Audio must run at 48 kHz to match what `mpv` is told to emit. A device that cannot do so is refused rather than resampled, because resampling would pitch-shift the audio and make the clock wrong.

## Config

Preferences are stored at:

- **macOS**: `~/Library/Application Support/yp/prefs.toml`
- **Linux**: `~/.config/yp/prefs.toml`

Logs are written daily to the same directory under `logs/`.

### YouTube bot check

YouTube sometimes answers `yt-dlp` with "Sign in to confirm you're not a bot". This is IP rate limiting, not a broken install. yp detects that response and explains the fix rather than retrying, because retrying adds load to an already-throttled IP.

To reduce the chance of hitting it, metadata enrichment is deliberately capped: at most 3 concurrent requests and 40 per page load, both in `constants.ron`. A channel can list hundreds of videos, and enriching all of them is what gets an IP flagged. Search and channel listing use different endpoints and keep working even when per-video metadata is throttled.

If it persists, cookies are the actual fix. yp does **not** enable this by default, because reading browser cookies on macOS triggers an interactive Keychain prompt. Opt in per-session:

```bash
export YP_YTDLP_COOKIES_FROM_BROWSER=chrome   # or firefox, brave, edge, safari
yp
```

The flag is inserted before yt-dlp's `--` separator, so your URLs stay intact. You can also point yt-dlp at an exported `cookies.txt` via the `YP_YTDLP_COOKIES` variable if you prefer not to grant Keychain access.

## CLI

All subcommands output JSON to stdout (progress/errors go to stderr). Bare `yp` with no subcommand launches the TUI.

### Subcommands

```bash
# Search YouTube
yp search "ambient guitar"

# List channel videos (enriched with dates, tags, duration, views by default)
yp channel @ChrisH-v4e
yp channel @ChrisH-v4e --fast    # skip enrichment, titles + IDs only

# Video metadata
yp info dQw4w9WgXcQ

# Transcribe a video (classify-reduce by default, --raw for full whisper output)
yp transcript dQw4w9WgXcQ
yp transcript dQw4w9WgXcQ --raw

# Summarize: transcribe + classify + reduce to bounded JSON
yp summarize dQw4w9WgXcQ
yp summarize --latest              # latest from default channel
yp summarize @TwoSetViolin --latest 3

# Generate shell completions
eval "$(yp completions zsh)"
```

### Pipe workflows

```bash
# Browse channel with fzf, summarize selection
yp channel @ChrisH-v4e | fzf | yp summarize

# Non-interactive fuzzy filter
yp channel | fzf -f "algorithm" | yp summarize

# Pipe to an LLM
yp summarize --latest | opencode run "What is this video about?"

# Classic jq pipeline
yp channel --fast | jq -r '.video_id' | head -1 | xargs yp summarize
```
