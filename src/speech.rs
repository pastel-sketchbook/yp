//! Deciding when speech-to-text is worth running.
//!
//! Transcription costs a 460 MB model and hours of CPU on a long track, and on
//! instrumental audio it does not merely waste time: whisper hallucinate
//! plausible-looking sentences in unrelated languages, which reads as a real
//! transcript of something that was never said.

use crate::player::VideoDetails;

/// Keywords that mark a track as instrumental when YouTube's own category is
/// not yet known.
///
/// Deliberately narrow. Each of these would be a strange thing to write in the
/// title of a talk, and a false positive here silently suppresses a feature, so
/// ambiguous words like "music" or "live" are left out on their own.
///
/// `asmr` is absent for the same reason: "ASMR roleplay" and "ASMR interview"
/// are both speech, so the bare word says nothing about whether anyone talks.
const INSTRUMENTAL_MARKERS: &[&str] = &[
  "instrumental",
  "no lyrics",
  "no talking",
  "no talking heads",
  "no speech",
  "pure music",
  "background music",
  "study music",
  "relaxing music",
  "sleep music",
  "music box",
  "piano cover",
  "lofi",
  "lo-fi",
  "ambient",
  "meditation music",
  "white noise",
  "brown noise",
  "rain sounds",
];

/// True when a track is worth skipping automatic transcription for.
///
/// Two signals, in order of trust:
///
/// 1. YouTube's `Music` category. That is the platform's own judgement, and it
///    covers the long ambient/tribute uploads that keywords miss entirely.
/// 2. Title and tag keywords, used only until enrichment resolves the
///    category. These fire early, which matters: without this the first few
///    seconds of playback would already have started a transcription that then
///    has to be cancelled.
pub fn likely_no_speech(details: &VideoDetails) -> bool {
  if details.categories.iter().any(|category| category.trim().eq_ignore_ascii_case("music")) {
    return true;
  }
  text_signals_speechless(&details.title, &details.tags)
}

/// Keyword check over the title and tags.
fn text_signals_speechless(title: &str, tags: &[String]) -> bool {
  let title = title.to_lowercase();
  if INSTRUMENTAL_MARKERS.iter().any(|marker| title.contains(marker)) {
    return true;
  }
  tags.iter().any(|tag| {
    let tag = tag.to_lowercase();
    INSTRUMENTAL_MARKERS.iter().any(|marker| tag.contains(marker))
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn details(title: &str, tags: &[&str], categories: &[&str]) -> VideoDetails {
    VideoDetails {
      url: "https://youtube.com/watch?v=x".into(),
      title: title.into(),
      uploader: Some("Some Channel".into()),
      duration: Some("3:32:44".into()),
      upload_date: None,
      view_count: None,
      tags: tags.iter().map(|t| (*t).to_string()).collect(),
      categories: categories.iter().map(|c| (*c).to_string()).collect(),
    }
  }

  #[test]
  fn the_music_category_skips_transcription() {
    // The real case: a 3.5-hour Chopin upload whisper turns into Sinhala.
    let chopin = details(
      "4 Hours Chopin for Studying, Concentration & Relaxation",
      &["chopin", "nocturne", "classical piano"],
      &["Music"],
    );
    assert!(likely_no_speech(&chopin));
  }

  #[test]
  fn a_technology_talk_is_transcribed() {
    let talk = details(
      "Pushing the Boundary of Server Applications with Rust",
      &["rust", "programming"],
      &["Science & Technology"],
    );
    assert!(!likely_no_speech(&talk));
  }

  #[test]
  fn keywords_cover_the_window_before_enrichment_resolves() {
    // Enrichment has not run yet, so there is no category to trust.
    let untagged = details("Lofi Hip Hop Beats to Study To", &[], &[]);
    assert!(likely_no_speech(&untagged), "lofi must be caught by title alone");

    let tagged = details("Deep Focus", &["ambient", "brown noise"], &[]);
    assert!(likely_no_speech(&tagged), "tags must be consulted too");
  }

  #[test]
  fn ordinary_speech_titles_are_not_mistaken_for_music() {
    for title in [
      "Rust is not a silver bullet",
      "Building a music streaming service with Rust",
      "Live coding session",
      "How I learned the piano",
      "Music theory for software engineers",
      "ASMR interview",
    ] {
      assert!(!likely_no_speech(&details(title, &[], &["Education"])), "{title} must stay transcribed");
    }
  }

  #[test]
  fn markers_are_matched_case_insensitively() {
    assert!(likely_no_speech(&details("Chopin Nocturnes INSTRUMENTAL", &[], &[])));
    assert!(likely_no_speech(&details("Evening Calm", &["Rain Sounds"], &[])));
  }

  #[test]
  fn a_music_video_that_is_actually_a_concert_still_skips() {
    // The category is authoritative by design; a live concert album has
    // incidental speech that is not worth transcribing either.
    let concert = details("Full Concert Live in Tokyo", &["live"], &["Music"]);
    assert!(likely_no_speech(&concert));
  }

  #[test]
  fn empty_metadata_does_not_skip_transcription() {
    let bare = details("Some Talk", &[], &[]);
    assert!(!likely_no_speech(&bare), "absence of signals must not suppress the feature");
  }
}
