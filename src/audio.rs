//! Audio output: yp owns the clock.
//!
//! `mpv` decodes to a FIFO instead of the sound device and runs unthrottled, so
//! yp pulls those samples at exactly device rate. That makes three things true
//! at once: the FIFO's backpressure paces `mpv` to real time, the position yp
//! reports is the position actually audible, and the spectrum taps the same
//! samples the listener hears rather than audio decoded ahead of them.

use anyhow::{Context, Result, anyhow};
use rodio::{ChannelCount, DeviceSinkBuilder, Player, Source};
use std::{
  fs::File,
  io::Read,
  os::unix::fs::PermissionsExt,
  path::{Path, PathBuf},
  sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU64, Ordering},
  },
  thread::JoinHandle,
};

use crate::spectrum::{Feed, Spectrum};

/// mpv is forced to this format, so every frame is 8 bytes and no negotiation
/// or format sniffing is needed. Changing it means changing `mpv_args`.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

/// [`rodio::SampleRate`] is a `NonZero<u32>`, so it cannot be built with a literal.
fn sample_rate() -> rodio::SampleRate {
  rodio::SampleRate::new(SAMPLE_RATE).expect("SAMPLE_RATE is a valid sample rate")
}
/// Bytes per sample: two channels of 32-bit float.
const BYTES_PER_FRAME: usize = 2 * 4;
/// Samples the reader may run ahead of the device before it blocks.
///
/// The reader **must** wait here rather than discard audio: blocking fills the
/// FIFO, which blocks mpv, which is what paces it to real time. Dropping
/// samples instead would play scrambled fragments of the track.
const MAX_QUEUED_SAMPLES: usize = SAMPLE_RATE as usize * CHANNELS as usize;

/// mpv arguments that produce the deterministic PCM the reader expects.
///
/// `ao=pcm` writes samples rather than playing them, `lavfi=[aresample=...]`
/// and `--audio-channels` pin the format, and `waveheader=no` omits a WAV header
/// so the stream is pure samples.
fn mpv_args(fifo: &Path, socket: &Path, start: Option<f64>) -> Vec<String> {
  let fifo = fifo.display().to_string();
  let socket = socket.display().to_string();
  let mut args = vec![
    "--no-video".to_string(),
    "--ao=pcm".to_string(),
    "--ao-pcm-waveheader=no".to_string(),
    format!("--ao-pcm-file={fifo}"),
    "--audio-channels=stereo".to_string(),
    "--af=lavfi=[aresample=48000]".to_string(),
    format!("--input-ipc-server={socket}"),
  ];
  // Seeks are handled by respawning, since a FIFO cannot be rewound.
  if let Some(seconds) = start {
    args.push(format!("--start={seconds}"));
  }
  args
}

/// Creates a FIFO at `path`, replacing any stale one.
fn make_fifo(path: &Path) -> Result<()> {
  if path.exists() {
    std::fs::remove_file(path).with_context(|| format!("removing stale FIFO at {}", path.display()))?;
  }
  let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).context("FIFO path contains a null byte")?;
  // SAFETY: `c_path` is a valid null-terminated string and `mkfifo` only reads it.
  let result = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
  if result != 0 {
    return Err(std::io::Error::last_os_error()).with_context(|| format!("creating FIFO at {}", path.display()));
  }
  // `mkfifo` honours the process umask, so set the mode explicitly.
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    .with_context(|| format!("securing FIFO at {}", path.display()))?;
  Ok(())
}

/// Samples waiting for the device, shared between the reader and the sink.
struct Queue {
  state: Mutex<State>,
  /// Signalled when either side changes the queue: the reader after appending,
  /// the sink after consuming.
  changed: Condvar,
}

#[derive(Default)]
struct State {
  samples: Vec<f32>,
  /// Set when mpv closed the FIFO, so a waiting reader can give up.
  ended: bool,
}

impl Queue {
  fn new() -> Self {
    Self {
      state: Mutex::new(State { samples: Vec::with_capacity(MAX_QUEUED_SAMPLES), ended: false }),
      changed: Condvar::new(),
    }
  }

  /// Appends decoded samples, blocking while the queue is full.
  ///
  /// This wait is the entire pacing mechanism: mpv decodes far faster than
  /// real time, so the reader has to refuse more until the device catches up.
  /// Only then does the FIFO fill and mpv block on write.
  fn push(&self, samples: &[f32]) {
    let Ok(mut state) = self.state.lock() else {
      return;
    };
    while !state.ended && state.samples.len() + samples.len() > MAX_QUEUED_SAMPLES {
      state = self.changed.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    if state.ended {
      // Playback was torn down while waiting; discard rather than grow.
      return;
    }
    state.samples.extend_from_slice(samples);
    self.changed.notify_all();
  }

  /// Removes up to `out.len()` samples, returning how many were copied.
  ///
  /// Never blocks: this runs on the device thread, where waiting would starve
  /// the callback. Returning zero is an underrun, which the caller turns into
  /// silence.
  fn take(&self, out: &mut [f32]) -> usize {
    let Ok(mut state) = self.state.lock() else {
      return 0;
    };
    let take = out.len().min(state.samples.len());
    out[..take].copy_from_slice(&state.samples[..take]);
    state.samples.drain(..take);
    self.changed.notify_all();
    take
  }

  /// Records that mpv closed the FIFO, releasing a blocked reader.
  fn mark_ended(&self) {
    if let Ok(mut state) = self.state.lock() {
      state.ended = true;
      self.changed.notify_all();
    }
  }

  /// True once the stream ended and everything decoded has been played.
  fn is_drained(&self) -> bool {
    self.state.lock().is_ok_and(|state| state.ended && state.samples.is_empty())
  }

  fn clear(&self) {
    if let Ok(mut state) = self.state.lock() {
      state.samples.clear();
      self.changed.notify_all();
    }
  }
}

/// Plays queued PCM through the default device and reports what was consumed.
pub struct AudioOutput {
  // Dropped last so the player outlives the stream that feeds it.
  player: Player,
  _sink: rodio::MixerDeviceSink,
  queue: Arc<Queue>,
  /// Frames the device has actually played. This is y p's clock.
  played_frames: Arc<AtomicU64>,
  _reader: JoinHandle<()>,
}

impl AudioOutput {
  /// Opens the default output device at exactly [`SAMPLE_RATE`].
  ///
  /// mpv is pinned to the same rate, so a device that cannot run at it is
  /// refused rather than resampled: changing the rate here would pitch-shift
  /// the audio and make the clock wrong.
  pub fn open() -> Result<(Player, rodio::MixerDeviceSink)> {
    let sink = DeviceSinkBuilder::from_default_device()
      .context("Opening the default audio output. Check that an output device exists")?
      .with_sample_rate(sample_rate())
      .with_channels(ChannelCount::new(CHANNELS).expect("CHANNELS is a valid channel count"))
      .with_error_callback(|error| tracing::warn!(%error, "audio device error"))
      .open_stream()
      .context("Starting the audio output stream")?;

    // The request above is a preference, not a guarantee: the backend may pick
    // a different rate. Feeding 48 kHz to a 44.1 kHz device would play at the
    // wrong speed, which is exactly the garbling this pipeline must avoid.
    let negotiated = sink.config().sample_rate();
    if negotiated != sample_rate() || sink.config().channel_count().get() != CHANNELS {
      anyhow::bail!(
        "Audio device negotiated {} Hz / {} channels, but yp feeds {} Hz / {CHANNELS} channels. \
         Feeding mismatched audio would change its speed. \
         Try selecting a 48 kHz output device.",
        negotiated.get(),
        sink.config().channel_count().get(),
        SAMPLE_RATE
      );
    }
    let player = Player::connect_new(sink.mixer());
    Ok((player, sink))
  }

  /// Starts reading `reader` and playing it on the already-open device.
  pub fn start(reader: File, player: Player, sink: rodio::MixerDeviceSink, spectrum: Arc<Spectrum>) -> Result<Self> {
    let queue = Arc::new(Queue::new());
    let played_frames = Arc::new(AtomicU64::new(0));

    // The FFT tap lives on the device side, so the analyzer sees the audio that
    // is being heard rather than whatever mpv decoded ahead of it.
    let source = FifoSource {
      queue: Arc::clone(&queue),
      played_frames: Arc::clone(&played_frames),
      feed: Feed::new(spectrum),
      // The right channel of a frame whose left channel has been emitted.
      pending: None,
    };
    player.append(source);

    let reader_queue = Arc::clone(&queue);
    let reader_handle = std::thread::Builder::new()
      .name("yp-pcm-reader".into())
      .spawn(move || {
        let mut raw = vec![0u8; 64 * 1024];
        // A FIFO read can split a frame, so the tail is carried into the next
        // read instead of being discarded.
        let mut carry: Vec<u8> = Vec::with_capacity(BYTES_PER_FRAME);
        let mut samples: Vec<f32> = Vec::with_capacity(64 * 1024 / 4);
        let mut reader = reader;
        loop {
          let read = match reader.read(&mut raw) {
            Ok(0) => break,
            Ok(read) => read,
            // EINTR is a retryable signal, not a broken pipe.
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
          };
          carry.extend_from_slice(&raw[..read]);
          let usable = carry.len() - carry.len() % BYTES_PER_FRAME;
          if usable == 0 {
            continue;
          }
          let (words, _) = carry[..usable].as_chunks::<4>();
          samples.clear();
          samples.extend(words.iter().map(|chunk| f32::from_le_bytes(*chunk)));
          reader_queue.push(&samples);
          carry.drain(..usable);
        }
        // mpv exiting closes the FIFO, so end-of-stream is how a track finishes.
        reader_queue.mark_ended();
      })
      .map_err(|error| anyhow!(error).context("Starting the PCM reader thread"))?;

    Ok(Self { player, _sink: sink, queue, played_frames, _reader: reader_handle })
  }

  /// Seconds of audio the device has consumed.
  pub fn position_secs(&self) -> f64 {
    self.played_frames.load(Ordering::Acquire) as f64 / f64::from(SAMPLE_RATE)
  }

  /// True once mpv closed the FIFO and every queued sample has played.
  pub fn finished(&self) -> bool {
    self.queue.is_drained()
  }

  pub fn set_paused(&self, paused: bool) {
    if paused {
      self.player.pause();
    } else {
      self.player.play();
    }
  }

  /// Discards anything queued but not yet played, so a restart begins cleanly.
  pub fn clear_queue(&self) {
    self.queue.clear();
  }
}

/// Hands queued samples to rodio, counts what was played, and taps the FFT.
struct FifoSource {
  queue: Arc<Queue>,
  played_frames: Arc<AtomicU64>,
  feed: Feed,
  /// The right channel of a frame whose left channel has already been emitted;
  /// rodio pulls one sample at a time, so a frame spans two calls.
  pending: Option<f32>,
}

impl Iterator for FifoSource {
  type Item = f32;

  fn next(&mut self) -> Option<f32> {
    if let Some(right) = self.pending.take() {
      return Some(right);
    }
    let mut frame = [0.0_f32; 2];
    let got = self.queue.take(&mut frame);
    if got == 0 {
      // An underrun yields silence rather than ending the stream. The clock is
      // deliberately not advanced: no audio was heard, so no time passed.
      return if self.queue.is_drained() { None } else { Some(0.0) };
    }
    self.played_frames.fetch_add(1, Ordering::AcqRel);
    self.feed.push(frame[0], frame[1]);
    if got == 2 {
      self.pending = Some(frame[1]);
    }
    Some(frame[0])
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    (0, None)
  }
}

impl Source for FifoSource {
  fn current_span_len(&self) -> Option<usize> {
    None
  }

  fn channels(&self) -> rodio::ChannelCount {
    rodio::ChannelCount::new(CHANNELS).expect("CHANNELS is a valid channel count")
  }

  fn sample_rate(&self) -> rodio::SampleRate {
    sample_rate()
  }

  fn total_duration(&self) -> Option<std::time::Duration> {
    None
  }

  fn try_seek(&mut self, _pos: std::time::Duration) -> Result<(), rodio::source::SeekError> {
    // A FIFO cannot be rewound; the player respawns mpv instead.
    Err(rodio::source::SeekError::NotSupported { underlying_source: "yp PCM FIFO" })
  }
}

/// Paths and process state for one track's decoder.
pub struct Decoder {
  fifo: PathBuf,
  socket: PathBuf,
  pub child: tokio::process::Child,
  reader: Option<File>,
}

impl Decoder {
  /// Spawns mpv writing PCM into a fresh FIFO.
  pub async fn spawn(url: &str, start: Option<f64>, pid: u32) -> Result<Self> {
    let fifo = std::env::temp_dir().join(format!("yp-pcm-{pid}.fifo"));
    let socket = std::env::temp_dir().join(format!("yp-mpv-{pid}.sock"));
    make_fifo(&fifo)?;

    let mut command = tokio::process::Command::new("mpv");
    command
      .args(mpv_args(&fifo, &socket, start))
      .arg(url)
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      // Send stderr to null — if piped but never drained, the pipe buffer
      // fills and mpv blocks.
      .stderr(std::process::Stdio::null())
      // mpv blocks writing to the FIFO, so a dropped decoder must not leave it
      // running. See `Drop for Decoder`.
      .kill_on_drop(true);

    let child = command.spawn().map_err(|error| {
      if error.kind() == std::io::ErrorKind::NotFound {
        anyhow!("mpv not found. Install it with: brew install mpv (macOS) or apt install mpv (Linux)")
      } else {
        anyhow!(error).context("Failed to spawn mpv process")
      }
    })?;

    // Opening for reading blocks until mpv opens the write end, so this must
    // not run on the async runtime's worker threads.
    let fifo_clone = fifo.clone();
    let reader = std::thread::Builder::new()
      .name("yp-fifo-open".into())
      .spawn(move || File::open(&fifo_clone).context("Opening the PCM FIFO"))
      .context("Starting the FIFO reader thread")?
      .join()
      .map_err(|_| anyhow!("FIFO reader thread panicked while opening {}", fifo.display()))??;

    Ok(Self { fifo, socket, child, reader: Some(reader) })
  }

  pub fn take_reader(&mut self) -> Option<File> {
    self.reader.take()
  }

  pub fn socket_path(&self) -> &Path {
    &self.socket
  }
}

impl Drop for Decoder {
  fn drop(&mut self) {
    // `kill_on_drop` stops mpv when the decoder is dropped without `teardown`
    // running, such as an early return from a failed playback setup. Otherwise
    // it would survive, blocked forever writing into a FIFO nobody reads.
    let _ = self.child.start_kill();
    let _ = std::fs::remove_file(&self.fifo);
    let _ = std::fs::remove_file(&self.socket);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mpv_args_force_a_deterministic_format() {
    let args = mpv_args(Path::new("/tmp/f"), Path::new("/tmp/s"), None);
    let joined = args.join(" ");
    assert!(joined.contains("--ao=pcm"), "{joined}");
    assert!(joined.contains("--ao-pcm-file=/tmp/f"), "{joined}");
    assert!(joined.contains("--ao-pcm-waveheader=no"), "{joined}");
    assert!(joined.contains("--audio-channels=stereo"), "{joined}");
    assert!(joined.contains("aresample=48000"), "{joined}");
    assert!(joined.contains("--input-ipc-server=/tmp/s"), "{joined}");
    assert!(!joined.contains("--start="), "no seek means no --start: {joined}");
  }

  #[test]
  fn seeking_asks_mpv_to_start_partway() {
    let args = mpv_args(Path::new("/tmp/f"), Path::new("/tmp/s"), Some(42.5));
    assert!(args.iter().any(|a| a == "--start=42.5"), "{args:?}");
  }

  #[test]
  fn fifo_is_created_and_secured() {
    let path = std::env::temp_dir().join(format!("yp-test-fifo-{}.fifo", std::process::id()));
    make_fifo(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "FIFO must not be world accessible");
    // Replacing a stale FIFO must succeed rather than fail with EEXIST.
    make_fifo(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
  }

  /// The analyzer a source taps into. Analysis needs no subscriber here, so a
  /// cheap stand-in keeps the source tests focused on pacing and counting.
  fn test_source(queue: Arc<Queue>, played: Arc<AtomicU64>) -> FifoSource {
    FifoSource {
      queue,
      played_frames: played,
      feed: Feed::new(Spectrum::start(SAMPLE_RATE).expect("analyzer starts")),
      pending: None,
    }
  }

  #[test]
  fn a_full_queue_blocks_the_reader_instead_of_dropping_audio() {
    let queue = Arc::new(Queue::new());
    queue.push(&vec![0.25; MAX_QUEUED_SAMPLES]);

    // A further push must wait rather than overwrite: the reader thread is
    // now blocked, which is what will fill the FIFO and throttle mpv.
    let blocked = Arc::clone(&queue);
    let producer = std::thread::spawn(move || blocked.push(&[0.75, 0.75]));
    std::thread::sleep(std::time::Duration::from_millis(150));

    // Draining one frame frees exactly the room the blocked push needs. The
    // value proves the blocked samples were not force-written over the top:
    // the queue still holds the audio that was there first.
    let mut out = [0.0; 2];
    assert_eq!(queue.take(&mut out), 2);
    assert_eq!(out, [0.25, 0.25], "queued audio must survive a blocked push");
    producer.join().expect("producer finished");

    // Drain everything and prove no sample was lost or reordered: the deferred
    // push must arrive at the back, after the audio already queued.
    let mut drained: Vec<f32> = Vec::new();
    let mut buffer = vec![0.0_f32; 8192];
    loop {
      let took = queue.take(&mut buffer);
      if took == 0 {
        break;
      }
      drained.extend_from_slice(&buffer[..took]);
    }
    assert_eq!(drained.len(), MAX_QUEUED_SAMPLES, "no sample may be dropped");
    assert!(drained.starts_with(&[0.25, 0.25]), "oldest audio plays first");
    assert_eq!(drained[MAX_QUEUED_SAMPLES - 2..], [0.75, 0.75], "the deferred push arrives intact");
  }

  #[test]
  fn end_of_stream_releases_a_blocked_reader() {
    let queue = Arc::new(Queue::new());
    queue.push(&vec![0.25; MAX_QUEUED_SAMPLES]);
    let blocked = Arc::clone(&queue);
    let producer = std::thread::spawn(move || blocked.push(&vec![0.75; 4096]));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // Tearing playback down must not leave the reader parked forever.
    queue.mark_ended();
    producer.join().expect("a blocked reader must wake on teardown");
  }

  #[test]
  fn queue_reports_end_of_stream_only_once_drained() {
    let queue = Queue::new();
    queue.push(&[0.5, 0.5]);
    let mut out = [0.0; 2];
    assert_eq!(queue.take(&mut out), 2, "still draining buffered audio");
    assert_eq!(out, [0.5, 0.5]);
    assert!(!queue.is_drained(), "queued audio remains after the stream ends");
    // Re-queue so the ended flag is set while audio is still pending, which is
    // the state a track reaching its end actually produces.
    queue.push(&[0.5, 0.5]);
    queue.mark_ended();
    assert!(!queue.is_drained(), "draining takes priority over the ended flag");
    let mut out = [0.0; 2];
    assert_eq!(queue.take(&mut out), 2, "buffered audio still plays after the end");
    assert!(queue.is_drained(), "an empty ended queue is finished");
  }

  #[test]
  fn take_never_blocks_so_the_device_thread_cannot_stall() {
    let queue = Queue::new();
    let mut out = [0.0; 2];
    let started = std::time::Instant::now();
    // An empty, still-running queue must return immediately rather than wait.
    assert_eq!(queue.take(&mut out), 0);
    assert!(started.elapsed() < std::time::Duration::from_millis(50), "take blocked");
    assert!(!queue.is_drained(), "a running player is not finished");
  }

  #[test]
  fn source_emits_stereo_pairs_and_counts_frames() {
    let queue = Arc::new(Queue::new());
    let played = Arc::new(AtomicU64::new(0));
    queue.push(&[0.1, 0.2, 0.3, 0.4]);
    let mut source = test_source(Arc::clone(&queue), Arc::clone(&played));
    assert_eq!(source.by_ref().collect::<Vec<_>>(), vec![0.1, 0.2, 0.3, 0.4]);
    // Four samples make two stereo frames.
    assert_eq!(played.load(Ordering::Acquire), 2);
    assert_eq!(source.channels().get(), 2);
    assert_eq!(source.sample_rate(), sample_rate());
  }

  #[test]
  fn an_underrun_yields_silence_without_advancing_the_clock() {
    let queue = Arc::new(Queue::new());
    let played = Arc::new(AtomicU64::new(0));
    let mut source = test_source(Arc::clone(&queue), Arc::clone(&played));
    // Nothing queued yet: the sink must see silence, not the end of the track.
    assert_eq!(source.next(), Some(0.0));
    assert_eq!(played.load(Ordering::Acquire), 0, "silence must not move the clock");

    queue.push(&[0.4, 0.5]);
    // A stereo frame spans two pulls, so the right channel comes out first.
    assert_eq!(source.next(), Some(0.4));
    assert_eq!(source.next(), Some(0.5));
    assert_eq!(played.load(Ordering::Acquire), 1, "one frame is one clock tick");

    queue.mark_ended();
    assert_eq!(source.next(), None, "a drained, ended queue ends the stream");
  }

  #[test]
  fn source_refuses_to_seek_a_fifo() {
    let mut source = test_source(Arc::new(Queue::new()), Arc::new(AtomicU64::new(0)));
    assert!(source.try_seek(std::time::Duration::from_secs(1)).is_err());
  }

  /// A real tone in the exact format `mpv` is told to emit. `None` when
  /// `mpv` or `ffmpeg` is unavailable, so the suite still runs elsewhere.
  fn tone_file(tag: u32) -> Option<PathBuf> {
    if std::process::Command::new("mpv").arg("--version").output().is_err() {
      return None;
    }
    let path = std::env::temp_dir().join(format!("yp-tone-{tag}.wav"));
    let status = std::process::Command::new("ffmpeg")
      .args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=2",
        "-ac",
        "2",
        "-ar",
        "48000",
        "-c:a",
        "pcm_f32le",
      ])
      .args(["-y"])
      .arg(&path)
      .status();
    match status {
      Ok(status) if status.success() => Some(path),
      _ => {
        let _ = std::fs::remove_file(&path);
        None
      }
    }
  }

  /// Spawns the real decoder and returns the FIFO path plus the raw bytes read.
  /// `pid` must be unique per concurrent test: the FIFO is named after it.
  fn decode_tone(pid: u32) -> Option<(PathBuf, Vec<u8>)> {
    let tone = tone_file(pid)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    let url = tone.to_string_lossy().to_string();
    let mut decoder = runtime.block_on(Decoder::spawn(&url, None, pid)).expect("mpv should spawn");
    let fifo = decoder.fifo.clone();
    let mut reader = decoder.take_reader().expect("FIFO opened");
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    while let Ok(read) = reader.read(&mut buffer) {
      if read == 0 {
        break;
      }
      bytes.extend_from_slice(&buffer[..read]);
    }
    let _ = std::fs::remove_file(&tone);
    Some((fifo, bytes))
  }

  #[test]
  fn mpv_writes_f32_pcm_the_reader_assumes() {
    let Some((fifo, bytes)) = decode_tone(71_001) else {
      eprintln!("skipping: mpv or ffmpeg unavailable");
      return;
    };
    let _ = std::fs::remove_file(&fifo);
    // 2 s at 48 kHz, stereo, 4 bytes per sample.
    let expected = SAMPLE_RATE as usize * 2 * 2 * 4;
    assert!(bytes.len() >= expected - 4096, "expected ~{expected} bytes, got {}", bytes.len());
    assert_eq!(bytes.len() % BYTES_PER_FRAME, 0, "stream must be whole frames");
    // A WAV header would be misparsed as audio, so it must be absent.
    assert_ne!(&bytes[0..4], b"RIFF", "the wave header must be disabled");
    assert_ne!(&bytes[0..4], b"FFIR", "the wave header must be disabled");
    let (samples, _) = bytes.as_chunks::<4>();
    let samples: Vec<f32> = samples.iter().map(|c| f32::from_le_bytes(*c)).collect();
    assert!(samples.iter().all(|s| s.is_finite()), "samples must be finite f32");
    assert!(
      samples.iter().all(|s| (-1.0..=1.0).contains(s)),
      "samples outside [-1, 1] mean the format is not 32-bit float"
    );
    // Scan the whole clip, since the tone level is low. Any signal well above
    // the noise floor proves real audio came through rather than empty bytes.
    assert!(samples.iter().any(|s| s.abs() > 0.01), "the tone must not be silent");
  }

  #[test]
  fn decoded_pcm_drives_the_analyzer_into_active_frames() {
    let Some((fifo, bytes)) = decode_tone(71_002) else {
      eprintln!("skipping: mpv or ffmpeg unavailable");
      return;
    };
    let _ = std::fs::remove_file(&fifo);
    // Subscribe before feeding: the analyzer only runs while a subscriber exists,
    // so feeding first would discard every block.
    let spectrum = Spectrum::start(SAMPLE_RATE).expect("analyzer starts");
    let mut subscription = spectrum.subscribe();
    let mut feed = crate::spectrum::Feed::new(Arc::clone(&spectrum));
    // Mark playing before feeding: the analyzer only consumes blocks once it is
    // both subscribed and playing, so anything queued earlier is discarded.
    spectrum.set_playing(true);
    let (words, _) = bytes.as_chunks::<4>();
    let (frames, _) = words.as_chunks::<{ CHANNELS as usize }>();
    for frame in frames {
      feed.push(f32::from_le_bytes(frame[0]), f32::from_le_bytes(frame[1]));
    }
    let mut active = None;
    for _ in 0..400 {
      let frame = subscription.frames.borrow_and_update().clone();
      if frame.active {
        active = Some(frame);
        break;
      }
      std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let frame = active.expect("a 440 Hz tone should produce an active frame");
    let loudest = frame.levels.iter().enumerate().fold(
      (0usize, 0.0f32),
      |(bi, bl), (i, l)| {
        if *l > bl { (i, *l) } else { (bi, bl) }
      },
    );
    assert!(loudest.1 > 0.05, "a real tone must light the display: {:?}", frame.levels);
    assert!(loudest.0 < crate::spectrum::BANDS / 2, "440 Hz should peak in the low bands, got band {}", loudest.0);
  }

  #[test]
  fn killing_the_decoder_removes_its_fifo() {
    let Some(tone) = tone_file(71_003) else {
      eprintln!("skipping: mpv or ffmpeg unavailable");
      return;
    };
    let pid = 71_003;
    let fifo = std::env::temp_dir().join(format!("yp-pcm-{pid}.fifo"));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let url = tone.to_string_lossy().to_string();
    let child_id = {
      let decoder = runtime.block_on(Decoder::spawn(&url, None, pid)).expect("mpv spawns");
      assert!(fifo.exists(), "the FIFO must exist while decoding");
      let id = decoder.child.id().expect("mpv has a pid");
      drop(decoder);
      id
    };
    assert!(!fifo.exists(), "dropping the decoder must remove the FIFO, or seeks leak one per press");
    // mpv blocks writing to the FIFO, so a dropped decoder must not leave it
    // running; that would strand a process on every failed playback setup.
    for _ in 0..50 {
      if !process_alive(child_id) {
        return;
      }
      std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("mpv pid {child_id} outlived its decoder");
  }

  /// Whether `pid` is still running, via signal 0 which performs no delivery.
  fn process_alive(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 only performs error checking.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
  }

  #[test]
  fn bytes_per_frame_matches_the_forced_format() {
    assert_eq!(BYTES_PER_FRAME, 4 * CHANNELS as usize);
    assert_eq!(MAX_QUEUED_SAMPLES % BYTES_PER_FRAME, 0);
  }
}
