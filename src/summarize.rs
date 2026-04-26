//! Transcript classify-reduce pipeline.
//!
//! Takes raw whisper utterances and produces a structured summary:
//! 1. **Classify** — tag each utterance (`NonSpeech`, Filler, Repetition, `TopicShift`, `KeySegment`, Normal)
//! 2. **Filter** — suppress noise (non-speech, filler, repetition)
//! 3. **Reduce** — compress into bounded topics + key segments

use serde::Serialize;

use crate::player::VideoDetails;

// ---------------------------------------------------------------------------
// Classification types
// ---------------------------------------------------------------------------

/// Classification tag for a single whisper utterance.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UtteranceClass {
  /// Music, silence, applause — no speech content.
  NonSpeech,
  /// Low-information filler words (>50% filler tokens).
  Filler,
  /// Structurally similar to a recent utterance.
  Repetition,
  /// Semantic boundary — significant time gap or periodic marker.
  TopicShift,
  /// High information density (long, many unique words).
  KeySegment,
  /// Standard spoken content.
  Normal,
}

/// A whisper utterance with its classification and normalized form.
#[derive(Debug, Clone, Serialize)]
pub struct ClassifiedUtterance {
  /// Start time in seconds.
  pub start: f64,
  /// End time in seconds.
  pub end: f64,
  /// Original utterance text.
  pub text: String,
  /// Classification tag.
  pub class: UtteranceClass,
}

// ---------------------------------------------------------------------------
// Filler token set
// ---------------------------------------------------------------------------

/// Words that carry minimal information content.
const FILLER_TOKENS: &[&str] = &[
  "um",
  "uh",
  "erm",
  "hmm",
  "like",
  "basically",
  "right",
  "so",
  "yeah",
  "actually",
  "literally",
  "obviously",
  "anyway",
  "well",
  "okay",
  "ok",
];

/// Multi-word filler phrases (checked as substrings of the normalized text).
const FILLER_PHRASES: &[&str] = &["you know", "i mean", "sort of", "kind of", "so yeah"];

// ---------------------------------------------------------------------------
// Non-speech patterns
// ---------------------------------------------------------------------------

/// Patterns that indicate non-speech content from whisper.
const NON_SPEECH_PATTERNS: &[&str] = &[
  "[music]",
  "[applause]",
  "[silence]",
  "[laughter]",
  "[cheering]",
  "[inaudible]",
  "(music)",
  "(applause)",
  "(silence)",
  "♪",
  "♫",
];

// ---------------------------------------------------------------------------
// Classification logic
// ---------------------------------------------------------------------------

/// Normalize text for comparison: lowercase, strip punctuation, collapse whitespace.
fn normalize(text: &str) -> String {
  text
    .to_lowercase()
    .chars()
    .map(|c| if c.is_alphanumeric() || c == ' ' { c } else { ' ' })
    .collect::<String>()
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

/// Compute word-level similarity between two normalized strings (Jaccard index).
#[allow(clippy::cast_precision_loss)]
fn word_similarity(a: &str, b: &str) -> f64 {
  let set_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
  let set_b: std::collections::HashSet<&str> = b.split_whitespace().collect();
  if set_a.is_empty() && set_b.is_empty() {
    return 1.0;
  }
  let intersection = set_a.intersection(&set_b).count();
  let union = set_a.union(&set_b).count();
  if union == 0 { 0.0 } else { intersection as f64 / union as f64 }
}

/// Check if text is non-speech (music, silence, applause, etc.).
///
/// Checks against the **lowercased original text** (not the normalized form)
/// because non-speech patterns like `[Music]` contain brackets that
/// `normalize()` strips.
fn is_non_speech(text: &str) -> bool {
  let lower = text.to_lowercase();
  let trimmed = lower.trim();
  if trimmed.is_empty() {
    return true;
  }
  NON_SPEECH_PATTERNS.iter().any(|p| trimmed.contains(p))
}

/// Check if >50% of words are filler tokens.
#[allow(clippy::cast_precision_loss)]
fn is_filler(normalized: &str) -> bool {
  let words: Vec<&str> = normalized.split_whitespace().collect();
  if words.is_empty() {
    return false;
  }

  // Check multi-word phrases first: count matched phrase words
  let mut phrase_word_count = 0;
  let lower = normalized.to_lowercase();
  for phrase in FILLER_PHRASES {
    if lower.contains(phrase) {
      phrase_word_count += phrase.split_whitespace().count();
    }
  }

  // Count single-word fillers
  let single_filler_count = words.iter().filter(|w| FILLER_TOKENS.contains(w)).count();

  // Total filler words (avoid double-counting by taking the max contribution)
  let filler_total = single_filler_count + phrase_word_count;
  // Use word count as denominator; filler_total can exceed words.len() due to
  // overlap between phrase and single-word counts, so cap the ratio at 1.0.
  let ratio = (filler_total as f64 / words.len() as f64).min(1.0);
  ratio > 0.5
}

/// Classify a sequence of raw whisper utterances.
///
/// `utterances` should be `(start_centiseconds, stop_centiseconds, text)` triples
/// as produced by `whisper_cli::Utternace`.
#[allow(clippy::cast_precision_loss)]
pub fn classify(utterances: &[(i64, i64, String)]) -> Vec<ClassifiedUtterance> {
  // Sliding window of recent normalized forms for repetition detection.
  let mut recent_window: Vec<String> = Vec::with_capacity(10);
  let mut last_end_secs: f64 = 0.0;
  let mut since_last_topic: f64 = 0.0;

  utterances
    .iter()
    .map(|(start_cs, stop_cs, text)| {
      let start = *start_cs as f64 / 100.0;
      let end = *stop_cs as f64 / 100.0;
      let normalized = normalize(text);

      // Priority 1: Non-speech (checked against original text, not normalized)
      if is_non_speech(text) {
        last_end_secs = end;
        return ClassifiedUtterance { start, end, text: text.clone(), class: UtteranceClass::NonSpeech };
      }

      // Priority 2: Filler
      if is_filler(&normalized) {
        last_end_secs = end;
        since_last_topic += end - start;
        return ClassifiedUtterance { start, end, text: text.clone(), class: UtteranceClass::Filler };
      }

      // Priority 3: Repetition (Jaccard similarity > 0.85 with any recent utterance)
      let is_repetition = recent_window.iter().any(|prev| word_similarity(&normalized, prev) > 0.85);
      if is_repetition {
        // Still add to window so we can detect chains of repetition
        if recent_window.len() >= 10 {
          recent_window.remove(0);
        }
        recent_window.push(normalized);
        last_end_secs = end;
        since_last_topic += end - start;
        return ClassifiedUtterance { start, end, text: text.clone(), class: UtteranceClass::Repetition };
      }

      // Priority 4: Topic shift (time gap > 5s or accumulated time > 120s)
      let gap = start - last_end_secs;
      let is_topic_shift = gap > 5.0 || since_last_topic > 120.0;
      if is_topic_shift {
        since_last_topic = 0.0;
        if recent_window.len() >= 10 {
          recent_window.remove(0);
        }
        recent_window.push(normalized);
        last_end_secs = end;
        return ClassifiedUtterance { start, end, text: text.clone(), class: UtteranceClass::TopicShift };
      }

      // Priority 5: Key segment (long utterance with high unique-word density)
      let words: Vec<&str> = normalized.split_whitespace().collect();
      let unique: std::collections::HashSet<&&str> = words.iter().collect();
      let density = if words.is_empty() { 0.0 } else { unique.len() as f64 / words.len() as f64 };
      let is_key = words.len() >= 12 && density >= 0.7;
      let class = if is_key { UtteranceClass::KeySegment } else { UtteranceClass::Normal };

      if recent_window.len() >= 10 {
        recent_window.remove(0);
      }
      recent_window.push(normalized);
      last_end_secs = end;
      since_last_topic += end - start;

      ClassifiedUtterance { start, end, text: text.clone(), class }
    })
    .collect()
}

// ---------------------------------------------------------------------------
// Reduce types
// ---------------------------------------------------------------------------

/// A detected topic segment with time range and representative text.
#[derive(Debug, Clone, Serialize)]
pub struct TopicSegment {
  pub start_secs: f64,
  pub end_secs: f64,
  /// Representative sentence (first utterance in the segment).
  pub summary: String,
  pub utterance_count: u64,
}

/// A high-information moment with timestamp and text.
#[derive(Debug, Clone, Serialize)]
pub struct KeySegment {
  pub at_secs: f64,
  pub text: String,
}

/// Summary statistics from the reduce phase.
#[derive(Debug, Clone, Serialize)]
pub struct SummaryStats {
  pub time_range: (f64, f64),
  pub total_utterances: u64,
  pub suppressed_utterances: u64,
  pub filler_ratio: f64,
  pub non_speech_secs: f64,
  pub topics: Vec<TopicSegment>,
  pub key_segments: Vec<KeySegment>,
}

/// Full summary output: hint + video metadata + summary + filtered utterances.
#[derive(Debug, Clone, Serialize)]
pub struct SummaryOutput {
  pub _hint: String,
  pub video: VideoDetails,
  pub summary: SummaryStats,
  pub utterances: Vec<ClassifiedUtterance>,
}

// ---------------------------------------------------------------------------
// Reduce logic
// ---------------------------------------------------------------------------

/// Maximum number of topic segments in the output.
const MAX_TOPICS: usize = 30;

/// Maximum number of key segments in the output.
const MAX_KEY_SEGMENTS: usize = 50;

/// Reduce classified utterances into a bounded summary.
///
/// - Suppresses `NonSpeech`, Filler, and Repetition
/// - Groups utterances into topic segments (split at `TopicShift` boundaries)
/// - Extracts key segments
/// - Caps output to `MAX_TOPICS` topics and `MAX_KEY_SEGMENTS` key moments
#[allow(clippy::cast_precision_loss)]
pub fn reduce(video: &VideoDetails, classified: &[ClassifiedUtterance]) -> SummaryOutput {
  let total_utterances = classified.len() as u64;

  // Compute statistics
  let mut filler_count: u64 = 0;
  let mut non_speech_secs: f64 = 0.0;
  let mut suppressed: u64 = 0;

  for u in classified {
    match u.class {
      UtteranceClass::NonSpeech => {
        non_speech_secs += u.end - u.start;
        suppressed += 1;
      }
      UtteranceClass::Filler => {
        filler_count += 1;
        suppressed += 1;
      }
      UtteranceClass::Repetition => {
        suppressed += 1;
      }
      _ => {}
    }
  }

  let filler_ratio = if total_utterances == 0 { 0.0 } else { filler_count as f64 / total_utterances as f64 };

  let time_range = if classified.is_empty() {
    (0.0, 0.0)
  } else {
    (classified.first().map_or(0.0, |u| u.start), classified.last().map_or(0.0, |u| u.end))
  };

  // Build topic segments: split at TopicShift boundaries
  let mut topics: Vec<TopicSegment> = Vec::new();
  let mut current_topic_start: Option<f64> = None;
  let mut current_topic_summary: Option<String> = None;
  let mut current_topic_count: u64 = 0;

  for u in classified {
    if u.class == UtteranceClass::NonSpeech {
      continue; // Don't count non-speech in topics
    }

    if u.class == UtteranceClass::TopicShift {
      // Close previous topic if any
      if let Some(start) = current_topic_start {
        topics.push(TopicSegment {
          start_secs: start,
          end_secs: u.start,
          summary: current_topic_summary.take().unwrap_or_default(),
          utterance_count: current_topic_count,
        });
      }
      // Start new topic
      current_topic_start = Some(u.start);
      current_topic_summary = Some(u.text.clone());
      current_topic_count = 1;
    } else {
      if current_topic_start.is_none() {
        // First non-topic-shift utterance starts an implicit first topic
        current_topic_start = Some(u.start);
        current_topic_summary = Some(u.text.clone());
      }
      current_topic_count += 1;
    }
  }

  // Close the last topic
  if let Some(start) = current_topic_start {
    topics.push(TopicSegment {
      start_secs: start,
      end_secs: time_range.1,
      summary: current_topic_summary.unwrap_or_default(),
      utterance_count: current_topic_count,
    });
  }

  // Cap topics
  topics.truncate(MAX_TOPICS);

  // Extract key segments
  let mut key_segments: Vec<KeySegment> = classified
    .iter()
    .filter(|u| u.class == UtteranceClass::KeySegment)
    .map(|u| KeySegment { at_secs: u.start, text: u.text.clone() })
    .collect();
  key_segments.truncate(MAX_KEY_SEGMENTS);

  // Build filtered utterance list (keep Normal, TopicShift, KeySegment)
  let utterances: Vec<ClassifiedUtterance> = classified
    .iter()
    .filter(|u| matches!(u.class, UtteranceClass::Normal | UtteranceClass::TopicShift | UtteranceClass::KeySegment))
    .cloned()
    .collect();

  let hint = format!(
    "YouTube video transcript summary. Summarize mode: filler, music, silence, and repeated utterances suppressed. \
     {suppressed} of {total_utterances} utterances omitted. Full transcript available with --raw."
  );

  SummaryOutput {
    _hint: hint,
    video: video.clone(),
    summary: SummaryStats {
      time_range,
      total_utterances,
      suppressed_utterances: suppressed,
      filler_ratio,
      non_speech_secs,
      topics,
      key_segments,
    },
    utterances,
  }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  fn make_utterance(start_cs: i64, stop_cs: i64, text: &str) -> (i64, i64, String) {
    (start_cs, stop_cs, text.to_string())
  }

  // --- normalize ---

  #[test]
  fn normalize_strips_punctuation_and_lowercases() {
    assert_eq!(normalize("Hello, World! This is a TEST."), "hello world this is a test");
  }

  #[test]
  fn normalize_collapses_whitespace() {
    assert_eq!(normalize("  multiple   spaces  here  "), "multiple spaces here");
  }

  // --- is_non_speech ---

  #[test]
  fn non_speech_music_tag() {
    assert!(is_non_speech("[Music]"));
  }

  #[test]
  fn non_speech_empty() {
    assert!(is_non_speech(""));
  }

  #[test]
  fn non_speech_applause() {
    assert!(is_non_speech("[Applause]"));
  }

  #[test]
  fn non_speech_normal_text() {
    assert!(!is_non_speech("This is normal speech"));
  }

  // --- is_filler ---

  #[test]
  fn filler_mostly_filler_words() {
    assert!(is_filler("um uh like yeah so"));
  }

  #[test]
  fn filler_substantive_text() {
    assert!(!is_filler("the production techniques used in this recording are fascinating"));
  }

  #[test]
  fn filler_mixed_below_threshold() {
    // "actually" is filler, but 1/7 < 50%
    assert!(!is_filler("i actually think the right approach is better"));
  }

  // --- word_similarity ---

  #[test]
  fn similarity_identical() {
    assert!((word_similarity("hello world", "hello world") - 1.0).abs() < f64::EPSILON);
  }

  #[test]
  fn similarity_completely_different() {
    assert!(word_similarity("hello world", "foo bar baz") < 0.1);
  }

  #[test]
  fn similarity_high_overlap() {
    let sim = word_similarity("the quick brown fox", "the quick brown dog");
    assert!(sim > 0.5);
  }

  // --- classify ---

  #[test]
  fn classify_non_speech() {
    let utterances = vec![make_utterance(0, 200, "[Music]")];
    let result = classify(&utterances);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].class, UtteranceClass::NonSpeech);
  }

  #[test]
  fn classify_filler() {
    let utterances = vec![make_utterance(0, 200, "um uh like yeah so basically")];
    let result = classify(&utterances);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].class, UtteranceClass::Filler);
  }

  #[test]
  fn classify_normal() {
    let utterances = vec![make_utterance(0, 300, "Today we discuss music theory")];
    let result = classify(&utterances);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].class, UtteranceClass::Normal);
  }

  #[test]
  fn classify_topic_shift_on_time_gap() {
    let utterances = vec![
      make_utterance(0, 200, "First topic here"),
      make_utterance(800, 1000, "Second topic after a gap"), // 6s gap > 5s threshold
    ];
    let result = classify(&utterances);
    assert_eq!(result.len(), 2);
    assert_eq!(result[1].class, UtteranceClass::TopicShift);
  }

  #[test]
  fn classify_repetition() {
    let utterances = vec![
      make_utterance(0, 200, "This is a specific phrase about music"),
      make_utterance(200, 400, "This is a specific phrase about music"), // exact repeat
    ];
    let result = classify(&utterances);
    assert_eq!(result.len(), 2);
    assert_eq!(result[1].class, UtteranceClass::Repetition);
  }

  #[test]
  fn classify_key_segment_long_and_dense() {
    // 12+ words with high unique-word density
    let text = "The recording process involved layering twelve different guitar tracks \
                with unique effects pedals creating an atmospheric soundscape";
    let utterances = vec![make_utterance(0, 500, text)];
    let result = classify(&utterances);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].class, UtteranceClass::KeySegment);
  }

  // --- reduce ---

  #[test]
  fn reduce_suppresses_noise() {
    let utterances = vec![
      make_utterance(0, 200, "[Music]"),
      make_utterance(200, 400, "Welcome to the show"),
      make_utterance(400, 600, "um uh like yeah so basically"),
      make_utterance(600, 800, "Today we discuss recording"),
    ];
    let classified = classify(&utterances);
    let video = VideoDetails {
      url: "https://youtube.com/watch?v=test".to_string(),
      title: "Test Video".to_string(),
      uploader: Some("Test Channel".to_string()),
      duration: Some("8:00".to_string()),
      upload_date: None,
      view_count: None,
      tags: vec![],
    };
    let output = reduce(&video, &classified);

    assert_eq!(output.summary.total_utterances, 4);
    assert!(output.summary.suppressed_utterances >= 2); // music + filler at minimum
    // Filtered utterances should not contain NonSpeech or Filler
    for u in &output.utterances {
      assert!(
        matches!(u.class, UtteranceClass::Normal | UtteranceClass::TopicShift | UtteranceClass::KeySegment),
        "unexpected class in filtered output: {:?}",
        u.class
      );
    }
  }

  #[test]
  fn reduce_produces_topics() {
    let utterances = vec![
      make_utterance(0, 200, "Introduction to the topic"),
      make_utterance(200, 400, "More about the introduction"),
      make_utterance(1000, 1200, "Now a completely different topic"), // time gap triggers TopicShift
      make_utterance(1200, 1400, "Continuing the second topic"),
    ];
    let classified = classify(&utterances);
    let video = VideoDetails {
      url: "https://youtube.com/watch?v=test".to_string(),
      title: "Test".to_string(),
      uploader: None,
      duration: None,
      upload_date: None,
      view_count: None,
      tags: vec![],
    };
    let output = reduce(&video, &classified);

    assert!(output.summary.topics.len() >= 2, "expected at least 2 topics, got {}", output.summary.topics.len());
  }

  #[test]
  fn reduce_empty_input() {
    let classified = classify(&[]);
    let video = VideoDetails {
      url: "https://youtube.com/watch?v=test".to_string(),
      title: "Empty".to_string(),
      uploader: None,
      duration: None,
      upload_date: None,
      view_count: None,
      tags: vec![],
    };
    let output = reduce(&video, &classified);

    assert_eq!(output.summary.total_utterances, 0);
    assert_eq!(output.summary.suppressed_utterances, 0);
    assert!(output.summary.topics.is_empty());
    assert!(output.summary.key_segments.is_empty());
    assert!(output.utterances.is_empty());
  }

  #[test]
  fn reduce_hint_contains_counts() {
    let utterances = vec![make_utterance(0, 200, "[Music]"), make_utterance(200, 400, "Hello world")];
    let classified = classify(&utterances);
    let video = VideoDetails {
      url: "x".to_string(),
      title: "x".to_string(),
      uploader: None,
      duration: None,
      upload_date: None,
      view_count: None,
      tags: vec![],
    };
    let output = reduce(&video, &classified);

    assert!(output._hint.contains("1 of 2 utterances omitted"));
  }

  // --- centisecond conversion ---

  #[test]
  fn classify_converts_centiseconds_to_seconds() {
    let utterances = vec![make_utterance(1500, 2000, "Five seconds of content")];
    let result = classify(&utterances);
    assert!((result[0].start - 15.0).abs() < f64::EPSILON);
    assert!((result[0].end - 20.0).abs() < f64::EPSILON);
  }
}
