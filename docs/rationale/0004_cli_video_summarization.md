# ADR-0004: CLI — Video Summarization via Channel Browsing

**Date:** 2026-03-06
**Status:** Accepted

## Context

yp's TUI is built for humans: browse a channel, select a video, watch the thumbnail, listen to audio, read the live transcript. But a growing class of consumers — LLM agents, shell pipelines, MCP tool servers — need the same YouTube data (metadata, transcripts, summaries) in a machine-readable, token-efficient format.

Today, users bridge this gap manually:

1. Open yp TUI, browse a channel, select a video
2. Wait for transcription to complete
3. Mentally summarize, or copy text from the terminal
4. Paste into an LLM chat window

This wastes tokens on raw utterances, loses structured metadata, and forces the user to mediate between two interfaces.

### The Opportunity

yp already has all the plumbing:

| Capability | Module | Status |
|---|---|---|
| Channel detection (`@handle`, URLs, `/channel`) | `youtube.rs` `detect_channel_url()` | Complete |
| Channel listing with pagination | `youtube.rs` `list_channel_videos()` | Complete |
| Background metadata enrichment | `youtube.rs` `enrich_video_metadata()` | Complete |
| Video info (title, uploader, duration, dates, tags) | `youtube.rs` `get_video_info()` | Complete |
| Thumbnail fetching (4 quality levels) | `youtube.rs` `fetch_thumbnail()` | Complete |
| Audio stream URL resolution (mpv IPC + yt-dlp fallback) | `transcript.rs` `resolve_stream_url()` | Complete |
| Chunked whisper transcription pipeline | `transcript.rs` `spawn_transcription_pipeline()` | Complete |
| Storyboard sprite sheets | `youtube.rs` `fetch_sprite_frames()` | Complete |

But all of it is coupled to the TUI event loop, ratatui types, and `mpsc` channels that feed into `App::check_pending()`. A CLI binary can reuse this logic if we extract it into a shared library crate.

### Design Philosophy: Summarize-First

Inspired by [kube-log-viewer's ADR-0007](../../../devops/TUI/kube-log-viewer/docs/rationale/0007_llm_cli_dual_interface.md), which defaults to **troubleshoot mode** (suppress noise, surface anomalies), yp's CLI defaults to **summarize mode**: the primary persona is an LLM consuming a video's content for analysis, summarization, or research.

The inversion is the same:

| Traditional | yp CLI |
|---|---|
| Show the raw transcript, let the human read | Show a structured summary, let the LLM reason |
| Full 3,600 utterances for a 1-hour video | Bounded output: metadata + key segments + topic markers |
| Human decides what matters | Pipeline pre-classifies signal vs. noise |

## Decision

### 1. Single-Crate Architecture (Revised from Original Proposal)

The original proposal called for a Cargo workspace split into `yp-core`, `yp-tui`, and `yp-cli`. In practice, the existing functions in `youtube.rs`, `transcript.rs`, and `player.rs` are already TUI-free and callable directly from CLI subcommands without ratatui. The single-crate approach proved sufficient:

```
yp/
├── Cargo.toml          # Single crate with all dependencies
├── src/
│   ├── main.rs         # CLI arg parsing, TUI entry point
│   ├── cli.rs          # CLI subcommand handlers (search, channel, info, transcript, summarize)
│   ├── summarize.rs    # Transcript classify-reduce pipeline
│   ├── youtube.rs      # YouTube search, channel listing, enrichment, frame sources
│   ├── transcript.rs   # Stream URL resolution, whisper pipeline
│   ├── player.rs       # mpv subprocess, IPC, status monitoring
│   ├── app.rs          # TUI App state, async task orchestration
│   ├── ui.rs           # ratatui rendering
│   ├── input.rs        # Keyboard handling
│   ├── graphics.rs     # Kitty, Sixel, half-block, ASCII rendering
│   ├── theme.rs        # 12 built-in themes
│   ├── config.rs       # prefs.toml
│   ├── display.rs      # Display mode enum, detection
│   ├── constants.rs    # Compile-time constants from RON
│   └── window.rs       # macOS PiP window management
```

CLI subcommands dispatch before entering the TUI event loop — `yp search`, `yp channel`, etc. return JSON to stdout and exit immediately, never touching ratatui or the alternate screen. The `Command` enum is `Option<Command>`, so bare `yp` with no subcommand still launches the TUI.

### 2. Reuse Strategy — Already Decoupled

The core functions turned out to be TUI-free already. No workspace split was needed:

- `search_youtube()`, `list_channel_videos()`, `get_video_info()`, `fetch_thumbnail()` — return `Result<T>`, no ratatui dependency. The CLI calls them directly with `.await`.
- `enrich_video_metadata()` — takes `mpsc::Sender<VideoMeta>`, CLI collects results via the same channel.
- `spawn_transcription_pipeline()` — takes `mpsc::UnboundedSender<TranscriptEvent>`. For CLI, pass `ipc_socket: None` to skip mpv IPC and go straight to `yt-dlp -g` fallback. CLI collects all `TranscriptEvent::ChunkTranscribed` utterances into a `Vec`.

The CLI handlers live in `cli.rs` and dispatch before the TUI event loop in `main.rs`. They never touch ratatui, crossterm, or the alternate screen.

### 3. Transcript Classify-Reduce Pipeline

The core innovation, adapted from kube-log-viewer's log classification. A raw whisper transcript is noisy: filler words, repeated phrases, music-only segments, and silence produce utterances that waste LLM tokens. The pipeline classifies and reduces them before output.

#### Stage 1: Classify (`summarize.rs`)

Every utterance is analyzed and tagged:

```rust
pub enum UtteranceClass {
    /// High-information segment: introduces a topic, states a conclusion,
    /// contains a key fact or opinion.
    KeySegment,
    /// Topic transition: semantic shift from previous utterance(s).
    /// Detected by embedding distance or keyword heuristics.
    TopicShift,
    /// Filler: low information density.
    /// Detected by pattern matching ("um", "uh", "you know", "like",
    /// "basically", "right", "so yeah").
    Filler,
    /// Music/silence: whisper produced no speech or only music tags.
    /// Detected by empty text, "[Music]" markers, or very short utterances
    /// with low confidence.
    NonSpeech,
    /// Repetition: structurally similar to a recent utterance.
    /// Detected by normalized string similarity (Levenshtein on
    /// lowercased, punctuation-stripped text).
    Repetition { canonical: String },
    /// Normal speech: standard spoken content, not classified above.
    Normal,
}
```

Classification rules, applied in priority order:

| Rule | Detects | Class |
|---|---|---|
| Text is empty, or matches `[Music]`, `[Applause]`, `[Silence]` | Non-speech segments | `NonSpeech` |
| Filler pattern: >50% of words are filler tokens | Low-info utterances | `Filler` |
| Normalized Levenshtein distance <0.15 from a recent utterance | Repeated phrases | `Repetition` |
| Significant semantic shift from rolling context window | Topic boundaries | `TopicShift` |
| High keyword density (numbers, proper nouns, technical terms) | Information-dense segments | `KeySegment` |
| Everything else | Standard speech | `Normal` |

**Filler detection** uses a static token set:

```rust
const FILLER_TOKENS: &[&str] = &[
    "um", "uh", "erm", "hmm", "like", "you know", "basically",
    "right", "so", "yeah", "i mean", "sort of", "kind of",
    "actually", "literally", "obviously", "anyway",
];
```

An utterance is classified as `Filler` if >50% of its word tokens (after lowercasing) are filler tokens. This threshold avoids false positives on sentences like "I actually think the right approach is..." where filler words appear in substantive context.

**Repetition detection** normalizes utterances by lowercasing, stripping punctuation, and collapsing whitespace, then computes similarity against a sliding window of the last 10 canonical forms. This catches whisper artifacts where the same phrase is transcribed multiple times, and deliberate repetition in speech ("again, again, the point is...").

#### Stage 2: Filter (Default Summarize Mode)

In summarize mode (the default), the CLI **keeps** only:

- `KeySegment` — always shown
- `TopicShift` — always shown (these mark content boundaries for the LLM)
- `Normal` — shown (standard speech is informative)

And **suppresses**:

- `NonSpeech` — dropped entirely (music, silence, applause)
- `Filler` — dropped unless `--verbose`
- `Repetition` — collapsed into a count: `[... 3 similar utterances omitted]`

Users can override with `--raw` to disable filtering and get the full unprocessed transcript.

#### Stage 3: Reduce (Map-Reduce for Token Economy)

A 1-hour video at whisper's ~1 utterance/second produces ~3,600 utterances. Even after filtering, this can exceed useful LLM context. The reduce stage compresses classified output into a bounded token budget.

**Map phase** (per-utterance, streaming):

```rust
pub struct MappedUtterance {
    pub start_secs: f64,
    pub end_secs: f64,
    pub class: UtteranceClass,
    pub text: String,
    pub normalized: String,    // lowercased, stripped — for dedup
}
```

**Reduce phase** (aggregation):

```rust
pub struct VideoSummary {
    /// Video metadata.
    pub video: VideoDetails,
    /// Time range of the transcript.
    pub time_range: (f64, f64),
    /// Total utterances from whisper.
    pub total_utterances: u64,
    /// Utterances suppressed by the filter.
    pub suppressed_utterances: u64,
    /// Detected topic segments with start time and representative text.
    pub topics: Vec<TopicSegment>,
    /// Key segments: highest-information utterances.
    pub key_segments: Vec<KeySegment>,
    /// Filler density: ratio of filler utterances to total (0.0-1.0).
    pub filler_ratio: f64,
    /// Non-speech segments: total duration in seconds.
    pub non_speech_secs: f64,
}

pub struct TopicSegment {
    /// Start time in seconds.
    pub start_secs: f64,
    /// End time in seconds (start of next topic, or end of video).
    pub end_secs: f64,
    /// Representative sentence(s) for this topic.
    pub summary: String,
    /// Number of utterances in this segment.
    pub utterance_count: u64,
}

pub struct KeySegment {
    /// Timestamp in seconds.
    pub at_secs: f64,
    /// The utterance text.
    pub text: String,
}
```

Bounded output regardless of video length:

- `topics`: capped at 30 segments
- `key_segments`: capped at 50
- Each topic carries a representative sentence, not the full text
- Total output fits within ~2K-6K tokens even for multi-hour videos

### 4. Output Format: JSON as Default

All CLI output is JSON. No flags needed for the common case.

**Channel listing mode** (`yp channel @handle`):

```jsonl
{"video_id":"abc123","title":"Video Title","url":"https://youtube.com/watch?v=abc123","upload_date":"2026-03-01","tags":"rock, ambient","duration":"12:34","view_count":"1,234,567","uploader":"Channel Name"}
{"video_id":"def456","title":"Another Video","url":"https://youtube.com/watch?v=def456","upload_date":"2026-02-28","tags":"piano","duration":"8:21","view_count":"456,789","uploader":"Channel Name"}
```

Each line is a self-contained JSON object (JSON-lines format). Fast, streamable, composable with `jq` and `fzf`. Enrichment is on by default; use `--fast` to skip it for titles and IDs only.

**Video info mode** (`yp info <video-id>`):

```json
{
  "video_id": "abc123",
  "url": "https://youtube.com/watch?v=abc123",
  "title": "Video Title",
  "uploader": "Channel Name",
  "duration": "12:34",
  "upload_date": "2026-03-01",
  "view_count": "1,234,567",
  "tags": ["rock", "ambient", "relaxing"]
}
```

**Transcript mode** (`yp transcript <video-id>`):

```jsonl
{"start":0.0,"end":4.2,"text":"Welcome back to the channel everyone.","class":"normal"}
{"start":4.2,"end":6.8,"text":"Today we're going to talk about something really exciting.","class":"key_segment"}
{"start":6.8,"end":8.1,"text":"[Music]","class":"non_speech"}
{"_collapsed":true,"count":3,"canonical":"um yeah so","class":"filler"}
{"start":12.5,"end":18.3,"text":"The first thing I want to cover is the new release.","class":"topic_shift"}
```

**Summary mode** (`yp summarize <video-id>` — the primary command):

```json
{
  "_hint": "YouTube video transcript summary. Summarize mode: filler, music, silence, and repeated utterances suppressed. 847 of 2,134 utterances omitted. Full transcript available with --raw.",
  "video": {
    "video_id": "abc123",
    "url": "https://youtube.com/watch?v=abc123",
    "title": "Video Title",
    "uploader": "Channel Name",
    "duration": "12:34",
    "upload_date": "2026-03-01",
    "view_count": "1,234,567",
    "tags": ["rock", "ambient"]
  },
  "summary": {
    "time_range": [0.0, 754.0],
    "total_utterances": 2134,
    "suppressed_utterances": 847,
    "filler_ratio": 0.18,
    "non_speech_secs": 42.5,
    "topics": [
      {
        "start_secs": 0.0,
        "end_secs": 120.5,
        "summary": "Introduction and overview of the new album release",
        "utterance_count": 45
      },
      {
        "start_secs": 120.5,
        "end_secs": 340.2,
        "summary": "Track-by-track breakdown of the first four songs",
        "utterance_count": 89
      },
      {
        "start_secs": 340.2,
        "end_secs": 580.0,
        "summary": "Production techniques and studio recording process",
        "utterance_count": 102
      },
      {
        "start_secs": 580.0,
        "end_secs": 754.0,
        "summary": "Closing thoughts and upcoming tour dates",
        "utterance_count": 63
      }
    ],
    "key_segments": [
      {"at_secs": 15.2, "text": "This album took three years to make and it changed how I think about music."},
      {"at_secs": 145.8, "text": "The second track uses a twelve-string tuning that I've never tried before."},
      {"at_secs": 420.1, "text": "We recorded the drums in a church because of the natural reverb."}
    ]
  },
  "utterances": [
    {"start": 0.0, "end": 4.2, "text": "Welcome back to the channel everyone.", "class": "normal"},
    {"start": 4.2, "end": 8.5, "text": "Today we're going to talk about the new album.", "class": "topic_shift"},
    {"start": 12.5, "end": 18.3, "text": "This album took three years to make and it changed how I think about music.", "class": "key_segment"}
  ]
}
```

The `_hint` field is a natural-language preamble — a system-prompt injection that tells the LLM what it's looking at, what's been filtered, and the scale of reduction. The LLM doesn't waste tokens figuring out the data format.

The `summary` block gives the LLM a compressed content overview: topic segments with timestamps and representative text, plus key information-dense moments. The `utterances` array contains only the classified-and-filtered speech. An LLM receiving this can reason about the video's content without processing thousands of raw whisper utterances.

### 5. CLI Interface

```
yp <subcommand> [flags]

SUBCOMMANDS:
    tui           Launch interactive TUI (default when no subcommand)
    search        Search YouTube and return results
    channel       List videos from a YouTube channel
    info          Fetch metadata for a specific video
    transcript    Transcribe a video and return utterances
    summarize     Transcribe + classify + reduce to a summary

SEARCH FLAGS:
    --query <text>            Search query (required)
    --limit <n>               Max results (default: 20)

CHANNEL FLAGS:
    <channel>                 Channel handle (@name), URL, or name
    --limit <n>               Max videos to list (default: 30)
    --fast                    Skip metadata enrichment (faster, no dates/tags/duration/views)

INFO FLAGS:
    <video>                   Video ID or YouTube URL

TRANSCRIPT FLAGS:
    <video>                   Video ID or YouTube URL
    --raw                     Disable classification, output raw utterances
    --verbose                 Include filler utterances
    --format <fmt>            json (default), jsonl, text

SUMMARIZE FLAGS:
    <video>                   Video ID or YouTube URL
    --raw                     Output full unprocessed transcript
    --verbose                 Include filler and repetition in output
    --topics <n>              Max topic segments (default: 30)
    --key-segments <n>        Max key segments (default: 50)
    --format <fmt>            json (default), text

GLOBAL FLAGS:
    --display-mode <mode>     auto, kitty, sixel, direct, ascii (TUI only)
    --version                 Print version
    --help                    Print help
```

### 6. Channel-First Workflow: `--latest`

The most common CLI workflow is "summarize the latest video from a channel." This should be a one-liner:

```bash
# Summarize the latest video from a channel
yp summarize @ChrisH-v4e --latest

# Summarize the 3 most recent videos
yp summarize @ChrisH-v4e --latest 3

# Browse channel with fzf, pipe selection to summarize (stdin mode)
yp channel @ChrisH-v4e | fzf | yp summarize

# Non-interactive fuzzy filter, summarize best match
yp channel @ChrisH-v4e | fzf -f "guitar" | yp summarize

# Classic jq + xargs pipeline
yp channel @ChrisH-v4e --fast | jq -r '.video_id' | head -1 | xargs yp summarize
```

The `--latest` flag combines channel listing + video selection + summarization into a single command. Without it, the user must compose the pipeline manually.

**Implementation**: `--latest [N]` is syntactic sugar for:

1. `list_channel_videos(channel_url, 1, N)` — fetch the first N videos
2. For each video: `get_video_info()` + transcription pipeline + classify-reduce
3. Output as a JSON array of `VideoSummary` objects

For `--latest 1` (the default), the output is a single JSON object (not an array). For N>1, it's a JSON array.

### 7. Pipe-First Philosophy

Following the [zig-saju](../../../langs/zig/zig-saju) and kube-log-viewer precedents, the pipe is the integration layer. No MCP server, no custom tool protocol, no ceremony.

#### The Pipe Workflow

**Step 1: yp fetches, transcribes, classifies, reduces.**

The classify-reduce pipeline (Section 3) runs locally. Filler, music, silence, and repetition are stripped. Topic segments, key moments, and clean utterances survive. The output is a self-contained JSON document:

```bash
# One-shot video summary
yp summarize abc123 \
  | opencode run "Summarize this video. What are the key points?"

# Channel research
yp summarize @TwoSetViolin --latest 3 \
  | opencode run "Compare these three videos. What topics recur?"

# Save first, iterate
yp summarize abc123 > /tmp/video.json
opencode run -f /tmp/video.json "What production techniques are discussed?"
```

**Step 2: Pipe to the LLM.**

**OpenCode** (preferred — supports stdin piping, streaming output):

```bash
yp summarize @ChrisH-v4e --latest \
  | opencode run "What is this video about? List the main topics."
```

**Copilot CLI**:

```bash
yp summarize @ChrisH-v4e --latest \
  | copilot -p "What is this video about?"
```

**Any future LLM CLI** — the pipe is universal.

**Step 3: LLM reasons over pre-triaged data.**

The LLM receives ~2K-6K tokens of structured content data instead of 3,600 raw utterances. It can immediately identify:

- The video's topic structure (introduction, main sections, conclusion)
- Key information-dense moments with timestamps
- The content's overall shape (filler ratio, non-speech duration)
- Metadata context (uploader, date, view count, tags)

No parsing. No scrolling through "[Music]" and "um yeah". The reduce pipeline already did the triage.

#### Copilot CLI: Shell Tool Escalation

When using Copilot CLI with `--allow-tool 'shell(yp)'`, Copilot can call yp autonomously:

```
You: What has @ChrisH-v4e been posting about recently?

Copilot: Let me check the channel's recent videos.
> yp channel @ChrisH-v4e --limit 5
  [5 videos with titles, dates, tags]

Copilot: I see 5 recent videos. Let me summarize the latest one.
> yp summarize abc123
  { "summary": { "topics": [...] }, ... }

Copilot: The latest video is about ambient guitar techniques.
         The main topics are: loop pedal layering, reverb chain design,
         and a live performance of the new piece "Coastal Drift."
```

Same workflow MCP would enable, but through standard shell piping. No protocol, no server, no config.

#### Why Not TTY Detection

Following zig-saju, `yp` CLI subcommands produce **identical output** regardless of whether stdout is a terminal or pipe. The default format is JSON. There is no "pretty for humans, machine for pipes" split — the TUI binary (`yp tui` or bare `yp`) is the human interface, the CLI subcommands are the machine interface.

The only exception: `yp` with no subcommand launches the TUI (backward compatible). All other subcommands produce JSON.

### 8. Transcription Without Playback

The TUI's transcription pipeline is tightly coupled to mpv playback: `resolve_stream_url()` tries the mpv IPC socket first, falling back to `yt-dlp -g`. The CLI doesn't start mpv — it only needs the transcript.

For the CLI, the existing pipeline works with a simple parameter change — pass `ipc_socket: None` to `spawn_transcription_pipeline()`:

1. **Resolve URL**: Falls back to `yt-dlp -g --format bestaudio` directly (skips IPC path)
2. **Download whisper model**: Same as TUI (download to `~/Library/Application Support/yp/`)
3. **Chunked transcription**: Same ffmpeg -> whisper loop, but:
   - Progress is written to stderr (not TUI events)
   - `TranscriptEvent::ChunkTranscribed` utterances are collected into a `Vec`
   - Pipeline runs to completion via the `run_transcription()` helper in `cli.rs`

```rust
// cli.rs: headless transcription (no mpv, no TUI)
async fn run_transcription(url: &str) -> Result<Vec<whisper_cli::Utternace>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<TranscriptEvent>();
    let whisper_cache = Arc::new(StdMutex::new(None));
    let handle = spawn_transcription_pipeline(tx, url.to_string(), whisper_cache, None);
    // collect ChunkTranscribed events into Vec...
}
```

### 9. Text Output Mode

While JSON is the default (machine consumers), `--format text` produces a human-readable summary for terminal users who want quick answers without the TUI:

```
$ yp summarize abc123 --format text

╭──────────────────────────────────────────────────╮
│ Video Title                                       │
│ Channel Name · 12:34 · 2026-03-01 · 1.2M views   │
│ Tags: rock, ambient, relaxing                     │
╰──────────────────────────────────────────────────╯

Summary (2,134 utterances → 4 topics, 847 filtered)
Filler: 18% · Non-speech: 42.5s

── Topics ──────────────────────────────────────────

[0:00 - 2:01] Introduction and overview of the new album release
              (45 utterances)

[2:01 - 5:40] Track-by-track breakdown of the first four songs
              (89 utterances)

[5:40 - 9:40] Production techniques and studio recording process
              (102 utterances)

[9:40 - 12:34] Closing thoughts and upcoming tour dates
               (63 utterances)

── Key Moments ─────────────────────────────────────

  0:15  "This album took three years to make and it changed
         how I think about music."
  2:26  "The second track uses a twelve-string tuning that
         I've never tried before."
  7:00  "We recorded the drums in a church because of the
         natural reverb."
```

This is useful for `yp summarize abc123 --format text | less` or quick terminal inspection, but the JSON format remains primary.

### 10. Shell Completion with Live Lookups

Tab completion accelerates channel and video discovery. Following kube-log-viewer's pattern, the CLI supports both static and dynamic completions.

#### Static Completions (subcommands, flags) — Implemented

The `completions` subcommand is already implemented in the current single-crate binary. It uses `clap_complete` to generate shell-specific completion scripts at runtime:

```rust
#[derive(Subcommand, Debug)]
enum Command {
    /// Generate shell completions for bash, zsh, fish, elvish, or powershell
    Completions {
        /// The shell to generate completions for
        shell: Shell,
    },
}
```

The subcommand dispatches before entering the TUI event loop — `yp completions <shell>` writes the completion script to stdout and exits immediately, never touching the terminal (no alternate screen, no raw mode, no panic hook). This is handled by checking `args.command` before calling `ratatui::init()`.

**Supported shells**: bash, zsh, fish, elvish, powershell — all variants supported by `clap_complete::Shell`.

**User setup** (one-time):

```bash
# bash — add to ~/.bashrc
eval "$(yp completions bash)"

# zsh — add to ~/.zshrc
eval "$(yp completions zsh)"

# fish — add to ~/.config/fish/config.fish
yp completions fish | source
```

This covers:
- Subcommands (`completions`, future `search`, `channel`, `info`, `transcript`, `summarize`)
- All flags (`--display-mode`, `--version`, `--help`)
- Enum values for `--display-mode` (`auto`, `kitty`, `sixel`, `direct`, `ascii`)
- Shell argument for `completions` (`bash`, `zsh`, `fish`, `elvish`, `powershell`)

**Backward compatibility**: The `Command` enum is `Option<Command>` — bare `yp` with no subcommand still launches the TUI. The subcommand infrastructure is extensible: future CLI subcommands (search, channel, summarize) will be added to the same `Command` enum and automatically gain Tab completion.

#### Dynamic Completions (channel videos) — Future

When the user has already specified a channel, Tab on the `<video>` positional argument can list recent video IDs with titles as help text:

```bash
yp summarize @ChrisH-v4e <TAB>
# → abc123  "Ambient Guitar Session #47"  (2026-03-01)
#   def456  "New Pedal Review: Strymon"   (2026-02-28)
#   ghi789  "Live Stream Highlights"      (2026-02-25)
```

**Implementation plan**: `clap_complete` v4+ supports runtime custom completers via `ArgValueCompleter`. Each completer receives the partial command line and returns `CompletionCandidate` values:

```rust
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};

fn channel_video_completer(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    // Parse already-provided args to find the channel handle
    // Call list_channel_videos() with a small page size (10)
    // Return CompletionCandidate::new(video_id).help(title)
    // ...
}
```

This requires a network call (~2s for `yt-dlp --flat-playlist`) which is acceptable for Tab completion — shells like zsh and fish show a spinner during slow completions.

**Practical limitation**: This only works when the channel is specified before the video ID in the command. For bare `yp summarize <TAB>`, there's no channel context to query. This is acceptable — the primary workflow is `yp summarize @channel --latest`, not manual video ID entry.

### 11. Whisper Model Management

The CLI reuses the TUI's whisper model download logic. The model is cached at `~/Library/Application Support/yp/ggml-small.bin` (~460MB). On first run, the CLI downloads it with a progress indicator on stderr:

```
$ yp summarize abc123
Downloading whisper model (ggml-small.bin)... 234MB / 461MB [50%]
```

After the first download, subsequent runs skip this step. The `--model` flag could override the model size in the future (tiny, base, small, medium, large), but small is the default for the best speed/accuracy tradeoff.

### 12. Error Handling

The CLI exits with structured JSON errors on failure, following kube-log-viewer's convention:

```json
{"error": "yt_dlp_not_found", "message": "yt-dlp not found. Install with: brew install yt-dlp"}
```

```json
{"error": "video_not_found", "message": "No video found for ID 'abc123'. Check the URL or video ID."}
```

```json
{"error": "transcription_failed", "message": "Whisper transcription failed on chunk 3: GenericError(-3). The audio may be corrupted or silent.", "video_id": "abc123"}
```

Exit codes:
- `0` — success
- `1` — user error (bad arguments, video not found)
- `2` — runtime error (yt-dlp failed, network error, transcription error)

JSON errors on stdout + non-zero exit code gives LLM agents a parseable failure mode. They can read the `error` field and decide whether to retry or report.

## Consequences

### Benefits

- **Reuse**: The core YouTube and transcription functions are called directly by both TUI and CLI code paths, with zero duplication.
- **Composability**: Every CLI subcommand is a Unix filter. `yp channel | fzf | yp summarize` composes naturally. Stdin pipe mode means `yp summarize` reads JSONL from stdin when no positional arg is given.
- **Token economy**: The classify-reduce pipeline compresses a 1-hour video's transcript from ~3,600 utterances to ~2K-6K tokens of structured JSON. LLMs reason over content, not noise.
- **LLM-native**: The `_hint` field, JSON structure, and bounded output are designed for LLM consumption from day one.
- **Backward compatible**: `yp` with no subcommand still launches the TUI. Existing users see no change.

### Costs

- **Single binary**: No workspace split means simpler builds and no inter-crate coordination. The tradeoff is that `yp info` (metadata-only) still links against ratatui and whisper — a future workspace split could slim the CLI binary if needed.
- **Whisper dependency in CLI**: The CLI binary includes whisper-cli-rs (and its whisper.cpp C dependency), making it a ~15MB binary with a CMake build step. Users who only want metadata (`yp info`, `yp channel`) still pay this cost. A future optimization could feature-gate transcription.
- **Classification quality**: Heuristic-based classification (filler detection, topic segmentation) will have false positives and negatives. The `TopicSegment.summary` field is the first sentence of the segment, not an LLM-generated summary — it's a representative sample, not a synthesis. This is intentional: the CLI does local compute only, no LLM API calls.
- **Transcription latency**: A 12-minute video takes ~60-90 seconds to transcribe on Apple Silicon (M1/M2) with the small model. This is the dominant latency in the pipeline. The `--raw` flag skips classification but not transcription itself. There's no way to avoid this cost without pre-computed transcripts (YouTube's auto-captions, explored below).

### Future Work

- **YouTube auto-captions**: YouTube generates auto-captions for most videos. Fetching these via `yt-dlp --write-auto-sub --sub-lang en --skip-download` would bypass whisper entirely, reducing latency from ~90s to ~3s. The tradeoff is lower transcript quality (YouTube's ASR vs. whisper small) and dependency on YouTube's caption availability. This could be a `--captions` flag that falls back to whisper when captions aren't available.
- **MCP server mode**: `yp --mcp` could expose typed tools over stdio JSON-RPC. Deferred until the pipe workflow proves insufficient.
- **Batch channel summarization**: `yp summarize @channel --latest 10 --parallel` could transcribe multiple videos concurrently. Memory constraints (whisper model is ~500MB in memory) limit parallelism, but sequential processing with progress feedback is a reasonable first step.
- **Embedding-based topic segmentation**: Replace keyword-heuristic topic detection with sentence embeddings (e.g., via a small ONNX model). This would dramatically improve `TopicShift` classification quality but adds another ML model dependency.
