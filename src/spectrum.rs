// Derived from vtamp (MIT, (c) 2026 Jang-Ho Hwang and vtamp contributors).
// See NOTICE at the repository root for the full licence text.
//! Lossy analysis of the PCM yp is currently playing. Never an audio effect.
//!
//! The samples arrive from the reader thread in [`audio::AudioOutput`] at the
//! rate the device is consuming them, so a frame describes sound that is
//! audible now rather than sound that has merely been decoded.

use crossbeam_queue::ArrayQueue;
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::{
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    mpsc,
  },
  time::{Duration, Instant},
};

/// Number of bars the view draws. Bands are logarithmic, so this is also the
/// number of times the frequency range is subdivided.
pub const BANDS: usize = 32;
/// Frames per submitted block. Small enough that a track change clears stale
/// audio quickly, large enough to keep the queue quiet.
pub const BLOCK_SIZE: usize = 256;
/// Analysis window. 4096 frames at 48 kHz is ~85 ms, long enough to resolve the
/// bass bands without smearing transients.
const FFT_SIZE: usize = 4096;
/// Bottom of the displayed range.
const LOW_HZ: f32 = 40.0;
/// Top of the displayed range; anything above this is folded into the last band.
const HIGH_HZ: f32 = 16_000.0;
/// Display headroom makes ordinary music legible; this is not a calibrated meter.
const FLOOR_DB: f32 = -70.0;
const CEILING_DB: f32 = -10.0;
const INTERVAL: Duration = Duration::from_millis(50);
/// Bounds the producer so a fast reader cannot grow memory without limit.
const QUEUE_DEPTH: usize = 32;
/// How far behind the newest audio a frame may be and still count as active.
const LIVENESS: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpectrumFrame {
  /// Bumped for every new logical stream so a stale view can discard bars.
  pub generation: u64,
  pub current_id: Option<String>,
  /// True while frames describe audio that is currently reaching the device.
  pub active: bool,
  pub low_hz: f32,
  pub high_hz: f32,
  /// Combined power across both channels, which is what every non-stereo style
  /// shows. Reading one channel alone would run about 3 dB quiet.
  pub levels: [f32; BANDS],
  /// The two channels kept apart, for the stereo style alone.
  pub channels: SpectrumChannels,
}

/// Per-channel band magnitudes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpectrumChannels {
  pub left: [f32; BANDS],
  pub right: [f32; BANDS],
}

struct Block {
  generation: u64,
  sequence: u64,
  pcm: [[f32; BLOCK_SIZE]; 2],
}

pub struct Spectrum {
  /// Optional so the worker can hold a `Weak` and terminate on last drop.
  wake: Option<mpsc::SyncSender<()>>,
  samples: ArrayQueue<Block>,
  generation: AtomicU64,
  subscribers: AtomicUsize,
  playing: AtomicBool,
  current_id: Mutex<Option<String>>,
  frames: tokio::sync::watch::Sender<SpectrumFrame>,
  /// Rate of the PCM being fed in, owned by the player rather than assumed.
  rate: u32,
}

impl Default for Spectrum {
  fn default() -> Self {
    Self {
      wake: None,
      samples: ArrayQueue::new(QUEUE_DEPTH),
      generation: AtomicU64::new(0),
      subscribers: AtomicUsize::new(0),
      playing: AtomicBool::new(false),
      current_id: Mutex::new(None),
      frames: tokio::sync::watch::channel(SpectrumFrame::default()).0,
      rate: 48_000,
    }
  }
}

impl Spectrum {
  pub fn start(rate: u32) -> std::io::Result<Arc<Self>> {
    let (wake, notifications) = mpsc::sync_channel(1);
    let spectrum = Arc::new(Self { wake: Some(wake), rate, ..Self::default() });
    let weak = Arc::downgrade(&spectrum);
    std::thread::Builder::new().name("yp-spectrum".into()).spawn(move || {
      let mut analyzer = Analyzer::new();
      loop {
        let active = {
          let Some(spectrum) = weak.upgrade() else {
            break;
          };
          analyzer.update(&spectrum);
          spectrum.enabled()
        };
        // Keep no strong reference while asleep. Dropping the last owner
        // disconnects the channel and ends this worker.
        if active {
          if matches!(notifications.recv_timeout(INTERVAL), Err(mpsc::RecvTimeoutError::Disconnected)) {
            break;
          }
        } else if notifications.recv().is_err() {
          break;
        }
      }
    })?;
    Ok(spectrum)
  }

  /// Starts a new logical stream. The view drops its bars when the generation
  /// changes, so bars never leak across tracks.
  pub fn context(&self, id: Option<&str>) {
    let mut current = self.current_id.lock().expect("spectrum id lock poisoned");
    if current.as_deref() != id {
      *current = id.map(str::to_owned);
      drop(current);
      self.generation.fetch_add(1, Ordering::AcqRel);
      self.wake();
    }
  }

  pub fn set_playing(&self, playing: bool) {
    if self.playing.swap(playing, Ordering::AcqRel) != playing {
      self.wake();
    }
  }

  pub fn subscribe(self: &Arc<Self>) -> Subscription {
    if self.subscribers.fetch_add(1, Ordering::AcqRel) == 0 {
      self.frames.send_replace(SpectrumFrame::default());
      self.wake();
    }
    Subscription { spectrum: self.clone(), frames: self.frames.subscribe() }
  }

  fn wake(&self) {
    if let Some(wake) = &self.wake {
      let _ = wake.try_send(());
    }
  }

  fn enabled(&self) -> bool {
    self.subscribers.load(Ordering::Acquire) > 0 && self.playing.load(Ordering::Acquire)
  }

  fn rate(&self) -> u32 {
    self.rate
  }
}

pub struct Subscription {
  spectrum: Arc<Spectrum>,
  pub frames: tokio::sync::watch::Receiver<SpectrumFrame>,
}

impl Drop for Subscription {
  fn drop(&mut self) {
    if self.spectrum.subscribers.fetch_sub(1, Ordering::AcqRel) == 1 {
      self.spectrum.wake();
    }
  }
}

/// Accumulates interleaved frames from the reader thread into fixed-size blocks.
///
/// The reader owns one of these for the life of a track; it is not shared, so
/// no locking is needed on the audio path.
pub struct Feed {
  spectrum: Arc<Spectrum>,
  pcm: [[f32; BLOCK_SIZE]; 2],
  filled: usize,
  sequence: u64,
}

impl Feed {
  pub fn new(spectrum: Arc<Spectrum>) -> Self {
    Self { spectrum, pcm: [[0.0; BLOCK_SIZE]; 2], filled: 0, sequence: 0 }
  }

  /// Offers one stereo frame. Non-finite input becomes silence so a NaN can
  /// never poison the whole window.
  pub fn push(&mut self, left: f32, right: f32) {
    let (left, right) = (finite(left), finite(right));
    let index = self.filled;
    self.pcm[0][index] = left;
    self.pcm[1][index] = right;
    self.filled += 1;
    if self.filled == BLOCK_SIZE {
      self.submit();
    }
  }

  fn submit(&mut self) {
    let generation = self.spectrum.generation.load(Ordering::Acquire);
    let block = Block { generation, sequence: self.sequence, pcm: self.pcm };
    self.sequence += 1;
    self.filled = 0;
    // A full queue means the analyzer is behind; the newest audio matters more
    // than the oldest, so drop the old block rather than the new one.
    self.spectrum.samples.force_push(block);
    self.spectrum.wake();
  }
}

fn finite(sample: f32) -> f32 {
  if sample.is_finite() { sample } else { 0.0 }
}

struct Analyzer {
  fft: Arc<dyn Fft<f32>>,
  input: Vec<Complex<f32>>,
  scratch: Vec<Complex<f32>>,
  /// Hann window, tapering the block edges so they do not leak across frames.
  window: Vec<f32>,
  pcm: [[f32; FFT_SIZE]; 2],
  cursor: usize,
  filled: usize,
  generation: u64,
  sequence: Option<u64>,
  last_sample: Instant,
}

impl Analyzer {
  fn new() -> Self {
    let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
    let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
    Self {
      fft,
      scratch,
      input: vec![Complex::default(); FFT_SIZE],
      window: (0..FFT_SIZE).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT_SIZE as f32).cos()).collect(),
      pcm: [[0.0; FFT_SIZE]; 2],
      cursor: 0,
      filled: 0,
      generation: 0,
      sequence: None,
      last_sample: Instant::now(),
    }
  }

  fn clear(&mut self) {
    self.cursor = 0;
    self.filled = 0;
    self.sequence = None;
  }

  fn push(&mut self, block: Block) {
    if self.generation != block.generation || self.sequence.is_some_and(|n| block.sequence != n + 1) {
      self.clear();
    }
    self.generation = block.generation;
    self.sequence = Some(block.sequence);
    for i in 0..BLOCK_SIZE {
      for ch in 0..2 {
        self.pcm[ch][self.cursor] = block.pcm[ch][i];
      }
      self.cursor = (self.cursor + 1) % FFT_SIZE;
    }
    self.filled = (self.filled + BLOCK_SIZE).min(FFT_SIZE);
    self.last_sample = Instant::now();
  }

  /// Maps the current window onto [`BANDS`] logarithmic bands in 0..1.
  /// Transforms one channel into per-band magnitudes.
  fn channel_levels(&mut self, channel: usize, rate: u32, high: f32) -> [f32; BANDS] {
    for i in 0..FFT_SIZE {
      self.input[i] = Complex::new(self.pcm[channel][(self.cursor + i) % FFT_SIZE] * self.window[i], 0.0);
    }
    self.fft.process_with_scratch(&mut self.input, &mut self.scratch);
    let power: Vec<f32> = self.input.iter().map(|bin| bin.norm_sqr() * 0.5).collect();

    std::array::from_fn(|band| {
      let low = LOW_HZ * (high / LOW_HZ).powf(band as f32 / BANDS as f32);
      let upper = LOW_HZ * (high / LOW_HZ).powf((band + 1) as f32 / BANDS as f32);
      let start = ((low * FFT_SIZE as f32 / rate as f32).round() as usize).clamp(1, FFT_SIZE / 2);
      let end = ((upper * FFT_SIZE as f32 / rate as f32).round() as usize).clamp(start, FFT_SIZE / 2);
      // The peak within a band keeps a narrow tone legible without inflating
      // wide bands that contain mostly noise.
      let peak = power[start..=end].iter().copied().fold(0.0, f32::max);
      let amplitude = peak.sqrt() * 4.0 / FFT_SIZE as f32;
      ((20.0 * amplitude.max(1e-10).log10() - FLOOR_DB) / (CEILING_DB - FLOOR_DB)).clamp(0.0, 1.0)
    })
  }

  /// Band magnitudes for the combined signal and for each channel.
  ///
  /// The split is what shows stereo width, and the sum is what the bar styles
  /// draw: neither can be recovered from the other, so both are computed.
  fn levels(&mut self, rate: u32) -> ([f32; BANDS], SpectrumChannels) {
    let high = (rate as f32 / 2.0).min(HIGH_HZ);
    if high <= LOW_HZ {
      return ([0.0; BANDS], SpectrumChannels::default());
    }
    let left = self.channel_levels(0, rate, high);
    let right = self.channel_levels(1, rate, high);
    let combined = std::array::from_fn(|band| left[band].max(right[band]));
    (combined, SpectrumChannels { left, right })
  }

  fn update(&mut self, spectrum: &Spectrum) {
    let generation = spectrum.generation.load(Ordering::Acquire);
    let enabled = spectrum.enabled();
    // A bounded drain also bounds work under a very fast producer.
    for _ in 0..QUEUE_DEPTH {
      let Some(block) = spectrum.samples.pop() else {
        break;
      };
      if enabled && block.generation == generation {
        self.push(block);
      }
    }
    if !enabled || self.generation != generation {
      self.clear();
    }
    if spectrum.subscribers.load(Ordering::Acquire) == 0 {
      return;
    }
    let active = enabled && self.filled == FFT_SIZE && self.last_sample.elapsed() < LIVENESS;
    let rate = spectrum.rate();
    let (levels, channels) = if active { self.levels(rate) } else { ([0.0; BANDS], SpectrumChannels::default()) };
    if spectrum.generation.load(Ordering::Acquire) != generation {
      return;
    }
    let next = SpectrumFrame {
      generation,
      current_id: spectrum.current_id.lock().expect("spectrum id lock poisoned").clone(),
      active,
      low_hz: LOW_HZ,
      high_hz: (rate as f32 / 2.0).min(HIGH_HZ),
      levels,
      channels,
    };
    spectrum.frames.send_if_modified(|frame| {
      // Active frames double as liveness heartbeats: a view must keep decaying
      // even when a steady tone produces identical bands every tick.
      if !next.active && *frame == next {
        false
      } else {
        *frame = next;
        true
      }
    });
  }
}
#[cfg(test)]
mod tests {
  use super::*;

  const RATE: u32 = 48_000;

  /// One second of a sine at `hz`, as the reader would feed it.
  fn tone(feed: &mut Feed, hz: f32) {
    let period = RATE as f32 / hz;
    for i in 0..RATE {
      let sample = (std::f32::consts::TAU * i as f32 / period).sin();
      feed.push(sample, sample);
    }
  }

  /// Runs the analyzer until it publishes a frame, or gives up.
  /// Index of the tallest band.
  fn peak_band(levels: &[f32; BANDS]) -> usize {
    levels
      .iter()
      .enumerate()
      .fold(
        (0, f32::NEG_INFINITY),
        |(best, high), (index, level)| {
          if *level > high { (index, *level) } else { (best, high) }
        },
      )
      .0
  }

  /// Waits for a frame that is active and differs from `previous`.
  ///
  /// The watch channel always holds the newest frame, so after feeding new audio
  /// the previously observed frame is still readable; without this the tests
  /// would read stale bars and compare the old audio against itself.
  fn await_change(subscription: &mut Subscription, previous: &SpectrumFrame) -> SpectrumFrame {
    for _ in 0..400 {
      let frame = subscription.frames.borrow_and_update().clone();
      if frame.active && frame != *previous {
        return frame;
      }
      std::thread::sleep(Duration::from_millis(10));
    }
    panic!("no new frame arrived after feeding audio");
  }

  /// Waits for a frame louder than `ceiling`.
  ///
  /// Waiting merely for a *different* frame is not enough: the worker keeps
  /// publishing frames for the audio fed before, so the next one after a change
  /// can still carry the old level. A test that wants "louder" has to say so.
  fn await_louder_than(subscription: &mut Subscription, ceiling: f32) -> SpectrumFrame {
    for _ in 0..400 {
      let frame = subscription.frames.borrow_and_update().clone();
      if frame.active && frame.levels.iter().copied().fold(0.0, f32::max) > ceiling {
        return frame;
      }
      std::thread::sleep(Duration::from_millis(10));
    }
    panic!("no frame louder than {ceiling} arrived");
  }

  fn spectrum() -> (Arc<Spectrum>, Subscription) {
    let spectrum = Spectrum::start(RATE).expect("spectrum worker starts");
    let subscription = spectrum.subscribe();
    spectrum.set_playing(true);
    (spectrum, subscription)
  }

  #[test]
  fn silence_publishes_a_flat_frame() {
    let (spectrum, mut subscription) = spectrum();
    let mut feed = Feed::new(spectrum.clone());
    for _ in 0..RATE {
      feed.push(0.0, 0.0);
    }
    let frame = await_change(&mut subscription, &SpectrumFrame::default());
    assert!(frame.levels.iter().all(|l| *l < 0.05), "silence must not light the bars: {:?}", frame.levels);
    assert_eq!(frame.low_hz, LOW_HZ);
    assert!(frame.high_hz <= HIGH_HZ);
  }

  #[test]
  fn a_bass_tone_lights_the_low_bands_only() {
    let (spectrum, mut subscription) = spectrum();
    let mut feed = Feed::new(spectrum.clone());
    tone(&mut feed, 80.0);
    let frame = await_change(&mut subscription, &SpectrumFrame::default());
    let peak = peak_band(&frame.levels);
    assert!(peak < BANDS / 2, "80 Hz should peak low, got band {peak} of {BANDS}");
    // The top bands must stay dark, otherwise the mapping is not logarithmic.
    let quiet = frame.levels.iter().skip(BANDS * 3 / 4).filter(|l| **l > 0.2).count();
    assert_eq!(quiet, 0, "high bands lit for a bass tone: {:?}", frame.levels);
  }

  #[test]
  fn a_treble_tone_lights_the_high_bands_only() {
    let (spectrum, mut subscription) = spectrum();
    let mut feed = Feed::new(spectrum.clone());
    tone(&mut feed, 8_000.0);
    let frame = await_change(&mut subscription, &SpectrumFrame::default());
    let peak = peak_band(&frame.levels);
    assert!(peak > BANDS / 2, "8 kHz should peak high, got band {peak} of {BANDS}");
  }

  #[test]
  fn louder_audio_produces_taller_bars() {
    let (spectrum, mut subscription) = spectrum();
    let mut feed = Feed::new(spectrum.clone());
    let period = RATE as f32 / 440.0;
    // The display has ~60 dB of range and plenty of headroom, so the two
    // amplitudes are chosen inside that window rather than near full scale.
    for i in 0..RATE {
      let quiet = (std::f32::consts::TAU * i as f32 / period).sin() * 0.005;
      feed.push(quiet, quiet);
    }
    let quiet_frame = await_louder_than(&mut subscription, 0.0);
    let quiet = quiet_frame.levels.iter().copied().fold(0.0, f32::max);
    for i in 0..RATE {
      let loud = (std::f32::consts::TAU * i as f32 / period).sin() * 0.15;
      feed.push(loud, loud);
    }
    let loud_frame = await_louder_than(&mut subscription, quiet + 0.1);
    let loud = loud_frame.levels.iter().copied().fold(0.0, f32::max);
    assert!(loud > quiet + 0.1, "loud {loud} should exceed quiet {quiet}");
    // The split travels with the combined levels, so the stereo style can show
    // width without the bar styles having to make do with one channel.
    for (name, levels) in [("left", &loud_frame.channels.left), ("right", &loud_frame.channels.right)] {
      assert!(levels.iter().copied().fold(0.0, f32::max) > quiet * 0.5, "the {name} channel must carry the signal too");
    }
  }

  #[test]
  fn non_finite_samples_become_silence() {
    let (spectrum, mut subscription) = spectrum();
    let mut feed = Feed::new(spectrum.clone());
    for i in 0..RATE {
      let sample = if i % 3 == 0 { f32::NAN } else { 1e30 };
      feed.push(sample, sample);
    }
    let frame = await_change(&mut subscription, &SpectrumFrame::default());
    assert!(frame.levels.iter().all(|l| l.is_finite()), "non-finite input escaped: {:?}", frame.levels);
  }

  #[test]
  fn a_new_track_changes_the_generation_and_clears_the_view() {
    let (spectrum, mut subscription) = spectrum();
    let mut feed = Feed::new(spectrum.clone());
    tone(&mut feed, 440.0);
    let first = await_change(&mut subscription, &SpectrumFrame::default());
    assert_eq!(first.current_id.as_deref(), None);
    spectrum.context(Some("track-a"));
    tone(&mut feed, 440.0);
    let second = await_change(&mut subscription, &first);
    assert_eq!(second.current_id.as_deref(), Some("track-a"));
    assert!(second.generation > first.generation, "a new track must bump the generation");
  }

  #[test]
  fn pausing_stops_publishing_active_frames() {
    let (spectrum, mut subscription) = spectrum();
    let mut feed = Feed::new(spectrum.clone());
    tone(&mut feed, 440.0);
    await_change(&mut subscription, &SpectrumFrame::default());
    spectrum.set_playing(false);
    // A paused player feeds nothing, so the frame must go stale on its own.
    for _ in 0..200 {
      if !subscription.frames.borrow_and_update().active {
        return;
      }
      std::thread::sleep(Duration::from_millis(10));
    }
    panic!("paused spectrum kept publishing active frames");
  }

  #[test]
  fn feed_buffers_until_a_full_block_is_collected() {
    let (spectrum, _subscription) = spectrum();
    let mut feed = Feed::new(spectrum.clone());
    for _ in 0..BLOCK_SIZE - 1 {
      feed.push(0.5, 0.5);
    }
    assert!(spectrum.samples.is_empty(), "a partial block must not be published");
    feed.push(0.5, 0.5);
    assert_eq!(spectrum.samples.len(), 1, "a full block must be published");
    feed.push(0.5, 0.5);
    assert_eq!(spectrum.samples.len(), 1, "clearing must discard the partial block");
  }
}
