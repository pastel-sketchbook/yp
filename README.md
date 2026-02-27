# yp

Terminal-based YouTube music player. Search YouTube, show thumbnails, play audio — in the terminal.

## Dependencies

```bash
brew install yt-dlp mpv
```

### Voice-to-Text (optional)

Voice search requires `sox` for recording and `whisper-cpp` for transcription.

```bash
brew install sox whisper-cpp
```

The whisper model (`ggml-small.bin`, ~460 MB) is downloaded automatically on first use.

## Usage

```bash
# Use ASCII art (default)
cargo run

# Or explicitly specify ASCII art
cargo run -- --display-mode ascii

# Use direct image display
cargo run -- --display-mode direct
```

You can also use the short form:
```bash
cargo run -- -d ascii
cargo run -- -d direct
```