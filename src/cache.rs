//! Video ID cache for shell completions.
//!
//! Stores `video_id\ttitle` lines in a TSV file under the OS cache directory
//! (`~/Library/Caches/yp/videos.tsv` on macOS). Entries are appended by CLI
//! commands (`channel`, `search`, `info`) and read by `_complete-ids` to
//! provide dynamic zsh completions.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Maximum number of entries to keep in the cache (oldest are evicted on compaction).
const MAX_ENTRIES: usize = 2000;

/// Return the cache file path: `<cache_dir>/yp/videos.tsv`.
fn cache_path() -> Option<PathBuf> {
  directories::ProjectDirs::from("", "", "yp").map(|d| d.cache_dir().join("videos.tsv"))
}

/// Append video entries to the cache file, deduplicating on write.
///
/// Each entry is a `(video_id, title)` pair. Duplicates update the title
/// and move the entry to the end (most recent).
pub fn append_videos(entries: &[(&str, &str)]) -> Result<()> {
  let path = match cache_path() {
    Some(p) => p,
    None => return Ok(()), // silently skip if no cache dir
  };

  // Ensure parent directory exists.
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent).context("Failed to create cache directory")?;
  }

  // Read existing entries.
  let mut existing = read_raw(&path);

  // Collect IDs being added so we can remove old duplicates.
  let new_ids: HashMap<&str, &str> = entries.iter().map(|&(id, title)| (id, title)).collect();

  // Remove existing entries that will be re-added.
  existing.retain(|(id, _)| !new_ids.contains_key(id.as_str()));

  // Append new entries at the end (most recent).
  for &(id, title) in entries {
    existing.push((id.to_string(), title.to_string()));
  }

  // Evict oldest if over limit.
  if existing.len() > MAX_ENTRIES {
    existing.drain(..existing.len() - MAX_ENTRIES);
  }

  // Write back atomically via temp file.
  let tmp = path.with_extension("tsv.tmp");
  {
    let mut f = fs::File::create(&tmp).context("Failed to create cache temp file")?;
    for (id, title) in &existing {
      writeln!(f, "{}\t{}", id, title).context("Failed to write cache entry")?;
    }
    f.flush().context("Failed to flush cache file")?;
  }
  fs::rename(&tmp, &path).context("Failed to rename cache temp file")?;

  Ok(())
}

/// Read all cached entries as `(video_id, title)` pairs.
///
/// Returns entries in file order (oldest first), deduplicated (last wins).
/// An empty vec is returned if the cache file doesn't exist or can't be read.
pub fn read_videos() -> Vec<(String, String)> {
  let path = match cache_path() {
    Some(p) => p,
    None => return Vec::new(),
  };
  dedup(read_raw(&path))
}

/// Internal: read lines from the TSV file without deduplication.
fn read_raw(path: &PathBuf) -> Vec<(String, String)> {
  let file = match fs::File::open(path) {
    Ok(f) => f,
    Err(_) => return Vec::new(),
  };
  let mut entries = Vec::new();
  for line in std::io::BufReader::new(file).lines() {
    let line = match line {
      Ok(l) => l,
      Err(_) => continue,
    };
    if let Some((id, title)) = line.split_once('\t') {
      entries.push((id.to_string(), title.to_string()));
    }
  }
  entries
}

/// Deduplicate entries, keeping the *last* occurrence of each video_id.
fn dedup(entries: Vec<(String, String)>) -> Vec<(String, String)> {
  let mut seen = HashMap::new();
  let mut result: Vec<(String, String)> = Vec::with_capacity(entries.len());
  // Walk in reverse so the last occurrence is kept in its original position.
  for (i, (id, title)) in entries.into_iter().enumerate() {
    seen
      .entry(id.clone())
      .and_modify(|idx: &mut usize| {
        // Replace the earlier entry's title and mark it for removal.
        result[*idx].0.clear();
        *idx = i;
      })
      .or_insert(i);
    result.push((id, title));
  }
  result.retain(|(id, _): &(String, String)| !id.is_empty());
  result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn dedup_basic() {
    let entries = vec![
      ("a".to_string(), "First".to_string()),
      ("b".to_string(), "Second".to_string()),
      ("a".to_string(), "Updated".to_string()),
    ];
    let result = dedup(entries);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0], ("b".to_string(), "Second".to_string()));
    assert_eq!(result[1], ("a".to_string(), "Updated".to_string()));
  }

  #[test]
  fn dedup_no_dups() {
    let entries = vec![("a".to_string(), "A".to_string()), ("b".to_string(), "B".to_string())];
    let result = dedup(entries);
    assert_eq!(result.len(), 2);
  }

  #[test]
  fn dedup_empty() {
    assert!(dedup(Vec::new()).is_empty());
  }

  #[test]
  fn read_raw_missing_file() {
    let entries = read_raw(&PathBuf::from("/tmp/yp_nonexistent_cache_12345.tsv"));
    assert!(entries.is_empty());
  }

  #[test]
  fn read_videos_no_panic() {
    // Smoke test: doesn't panic even if cache doesn't exist.
    let _ = read_videos();
  }
}
