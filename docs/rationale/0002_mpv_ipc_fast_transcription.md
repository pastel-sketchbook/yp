# Fast Transcription via mpv IPC + Chunked Whisper

## Problem

When a track starts playing, yp auto-triggers the transcription pipeline. The original implementation had two bottlenecks:

### Bottleneck 1: URL resolution (yt-dlp)

```
yt-dlp -x --audio-format wav --postprocessor-args "ffmpeg:-ar 16000 -ac 1" -o /tmp/yp-transcript.wav <youtube-url>
```

yt-dlp must re-resolve the YouTube URL to a CDN stream URL (HTTP requests, anti-bot negotiation, signature deciphering) even though mpv already resolved the exact same URL moments earlier. This adds 10–30 seconds.

### Bottleneck 2: Full-file processing

Even with a fast CDN URL, the original pipeline downloaded the **entire** audio stream, then transcribed the **entire** file before showing any results. For a 10-minute video:

- Download full audio: 5–15s
- Whisper transcribes full audio: 10–30s
- **Total: 30–80 seconds** before any transcript text appears

Users reported not seeing transcripts until ~8% / 50 seconds into a track.

## Solution: mpv IPC URL + Chunked Transcription

Two changes that compound:

1. **Reuse mpv's resolved CDN URL** — skip yt-dlp entirely (saves 10–30s)
2. **Chunk-based transcription** — download and transcribe in 30-second segments so the first results appear in ~5–8s

### Architecture

```
trigger_transcription(url)
  │
  ├─ Stage 1: resolve_stream_url()
  │   ├─ Try mpv IPC (up to 6 retries, ~0.5–4s)
  │   └─ Fallback: yt-dlp -g --format bestaudio (~10–30s)
  │
  ├─ Stage 2: Download whisper model (if needed)
  │
  └─ Stage 3: Chunked loop
      ├─ ffmpeg -ss OFFSET -t 30 -i <cdn-url> -vn -ar 16000 -ac 1 chunk.wav
      ├─ whisper.transcribe(chunk.wav)
      ├─ Adjust timestamps: utterance.start += OFFSET * 100 (centiseconds)
      ├─ Send ChunkTranscribed(utterances) → UI appends + shows immediately
      └─ Repeat with OFFSET += 30 until ffmpeg returns empty audio
```

## How mpv IPC Works

### Socket setup

mpv is started with `--input-ipc-server=/tmp/yp-mpv-<pid>.sock`. This Unix domain socket accepts JSON commands per the [mpv IPC documentation](https://mpv.io/manual/master/#json-ipc).

### Query the resolved URL

```json
{"command": ["get_property", "stream-open-filename"], "request_id": 1}
```

`stream-open-filename` is the URL that mpv's demuxer actually opened — the resolved CDN URL (e.g., `https://rr3---sn-xxx.googlevideo.com/videoplayback?...`), not the original `youtube.com/watch?v=...` input.

### Retry logic

mpv needs time after startup to resolve the YouTube URL. `resolve_stream_url()` retries up to 6 times with increasing delays (500ms, 1s, then 2s intervals). Each attempt:

- Connects to the Unix socket (may fail if mpv hasn't created it yet)
- Sends the `get_property` command
- Reads lines with a 3-second timeout, skipping event lines until `request_id: 1`
- Validates the response is an HTTP URL that isn't the original YouTube URL

Total worst-case wait: ~10.5 seconds, still faster than yt-dlp's 10–30s.

### URL validation

The response URL is accepted only if:

- Starts with `http` (not a local path or other scheme)
- Does not contain `youtube.com/watch` or `youtu.be/` (would indicate mpv returned the unresolved input URL)

## How Chunked Transcription Works

### Why 30-second chunks

- Whisper.cpp's native processing window is 30 seconds — it internally splits longer audio anyway
- 30s of 16kHz mono WAV is ~960KB — fast to download from CDN (~1–2s)
- Whisper transcribes 30s of audio in ~3–5s on a modern Mac
- First transcript text appears in **~5–8s** instead of ~50s

### Chunk download with ffmpeg

```
ffmpeg -y -ss 0 -t 30 -i <cdn-url> -vn -ar 16000 -ac 1 -f wav /tmp/yp-chunk-<pid>.wav
ffmpeg -y -ss 30 -t 30 -i <cdn-url> -vn -ar 16000 -ac 1 -f wav /tmp/yp-chunk-<pid>.wav
ffmpeg -y -ss 60 -t 30 ...
```

`-ss` before `-i` does a demuxer-level fast seek, which sends an HTTP Range header to the CDN. YouTube CDN URLs support this, so seeking to offset 60s doesn't require downloading the first 60s.

### End-of-stream detection

The loop terminates when:

- ffmpeg exits with non-zero status (seeked past end of stream), or
- The output WAV file is ≤44 bytes (WAV header with no audio samples)

### Timestamp adjustment

Whisper returns utterance timestamps relative to the chunk start. Each chunk's utterances are adjusted:

```rust
let offset_cs = (chunk_offset_secs as i64) * 100; // centiseconds
for u in &mut utterances {
    u.start = u.start.saturating_add(offset_cs);
    u.stop = u.stop.saturating_add(offset_cs);
}
```

### Progressive UI delivery

Each chunk sends `ChunkTranscribed(Vec<Utternace>)` to the UI via the event channel. The UI handler appends to the existing utterance list — it doesn't replace. The transcript pane becomes visible as soon as the first chunk arrives.

After all chunks are processed, `Transcribed` (no payload) signals completion and transitions the state machine to `Ready`.

## Timing Comparison

| Step | Original (yt-dlp full) | mpv IPC + chunked |
|---|---|---|
| URL resolution | 10–30s (yt-dlp) | 0.5–4s (mpv IPC) |
| First audio chunk | — | ~1–2s (ffmpeg 30s chunk) |
| First transcription | 10–30s (full file) | ~3–5s (30s chunk) |
| **First transcript visible** | **30–80s** | **~5–8s** |
| Full transcription complete | same | slightly longer (per-chunk overhead) |

The trade-off: full transcription takes slightly longer due to per-chunk ffmpeg startup overhead (~0.5s per chunk). For a 10-minute video, that's ~20 chunks × 0.5s = ~10s extra. But the user sees results within seconds instead of waiting a minute.

## Fallback: yt-dlp -g

If mpv IPC fails (socket not ready, URL not resolved, mpv not started with IPC), the pipeline falls back to:

```
yt-dlp -g --format bestaudio <youtube-url>
```

This outputs the direct CDN URL without downloading the audio. It's slower than mpv IPC (10–30s for URL resolution) but gives us the same CDN URL. The chunked download/transcription loop then proceeds identically.

The fallback is also used when there's no IPC socket path at all (e.g., if mpv was stopped before transcription started).

## Event Protocol

```
TranscriptEvent::AudioExtracted      — URL resolved, starting chunk loop
TranscriptEvent::DownloadProgress    — Whisper model download progress
TranscriptEvent::ChunkTranscribed    — Append utterances (one per chunk)
TranscriptEvent::Transcribed         — All chunks done, finalize
TranscriptEvent::Failed              — Error, abort pipeline
```

State machine:
```
Idle → ExtractingAudio (resolving URL)
     → Transcribing (chunks arriving)
     → Ready (all done)
```

## Implementation Details

### resolve_stream_url() (main.rs)

Standalone async function. Tries mpv IPC first, falls back to `yt-dlp -g`. Returns a CDN URL string. Both paths produce the same type of URL, so the chunked loop doesn't need to know which resolved it.

### player.rs

- `ipc_socket_path()` getter exposes the socket path (previously private)
- `get_stream_url()` method encapsulates the full IPC query with timeout and JSON parsing (available for other uses beyond transcription)

### Why standalone functions instead of methods

The transcription task runs in `tokio::spawn`, which requires `'static` futures. The task can't borrow `&self.player` because `self` isn't `'static`. Extracting the socket path as a `String` and passing it to standalone async functions avoids this lifetime issue cleanly.

### Whisper model caching

The whisper model (`Arc<StdMutex<Option<Whisper>>>`) is loaded on the first chunk and reused for all subsequent chunks. This means only the first chunk pays the model loading cost (~1–2s). All subsequent chunks go straight to transcription.
