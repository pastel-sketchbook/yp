use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

const WIKI_BUNDLE_URL: &str = "https://pastelsketchbook.vercel.app/wiki-bundle.json";
const TRANSCRIPT_BASE_URL: &str = "https://pastelsketchbook.vercel.app/transcripts";

/// Wiki detail for a single video, extracted from the wiki bundle.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WikiDetail {
  pub summary: String,
  pub takeaways: Vec<String>,
  pub topics: Vec<String>,
  #[serde(default)]
  pub related: Vec<RelatedVideo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RelatedVideo {
  pub id: String,
  #[serde(default, rename = "sharedTopics")]
  pub shared_topics: Vec<String>,
  /// Title resolved from the bundle (not present in the JSON `related` array).
  #[serde(skip)]
  pub title: Option<String>,
}

/// A video entry within the bundle (we only need id + detail).
#[derive(Debug, Deserialize)]
struct BundleVideo {
  id: String,
  title: Option<String>,
  detail: Option<WikiDetail>,
}

/// A category in the bundle containing videos.
#[derive(Debug, Deserialize)]
struct BundleCategory {
  videos: Vec<BundleVideo>,
}

/// Top-level wiki bundle structure.
#[derive(Debug, Deserialize)]
struct WikiBundle {
  categories: Vec<BundleCategory>,
}

/// Fetch the wiki bundle and extract the detail for the given video ID.
pub async fn fetch_wiki_detail(client: &Client, video_id: &str) -> Result<Option<WikiDetail>> {
  info!(video_id = %video_id, "wiki: fetching bundle");
  let bundle: WikiBundle = client
    .get(WIKI_BUNDLE_URL)
    .send()
    .await
    .context("Failed to fetch wiki bundle")?
    .json()
    .await
    .context("Failed to parse wiki bundle JSON")?;

  // Build a title lookup from all videos in the bundle.
  let title_map: std::collections::HashMap<String, String> = bundle
    .categories
    .iter()
    .flat_map(|c| &c.videos)
    .filter_map(|v| v.title.as_ref().map(|t| (v.id.clone(), t.clone())))
    .collect();

  for category in bundle.categories {
    for video in category.videos {
      if video.id == video_id {
        debug!(video_id = %video_id, found = video.detail.is_some(), "wiki: found video");
        let detail = video.detail.map(|mut d| {
          // Enrich related videos with titles from the bundle.
          for rel in &mut d.related {
            if rel.title.is_none() {
              rel.title = title_map.get(&rel.id).cloned();
            }
          }
          d
        });
        return Ok(detail);
      }
    }
  }
  debug!(video_id = %video_id, "wiki: video not found in bundle");
  Ok(None)
}

/// Fetch the raw transcript markdown for a video by ID.
pub async fn fetch_raw_transcript(client: &Client, video_id: &str) -> Result<Option<String>> {
  let url = format!("{TRANSCRIPT_BASE_URL}/{video_id}.md");
  info!(video_id = %video_id, "wiki: fetching raw transcript");
  let resp = client.get(&url).send().await.context("Failed to fetch raw transcript")?;
  if resp.status() == reqwest::StatusCode::NOT_FOUND {
    debug!(video_id = %video_id, "wiki: raw transcript not found");
    return Ok(None);
  }
  let text = resp
    .error_for_status()
    .context("Raw transcript request failed")?
    .text()
    .await
    .context("Failed to read raw transcript body")?;
  // Strip YAML frontmatter (between --- delimiters) if present.
  let content =
    if text.starts_with("---") { text.splitn(3, "---").nth(2).unwrap_or(&text).trim_start().to_string() } else { text };
  Ok(Some(content))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn deserialize_wiki_detail() {
    let json = r#"{
      "summary": "Test summary",
      "takeaways": ["Point 1", "Point 2"],
      "topics": ["topic1", "topic2"],
      "related": [{"id": "abc123", "sharedTopics": ["rust"]}]
    }"#;
    let detail: WikiDetail = serde_json::from_str(json).unwrap();
    assert_eq!(detail.summary, "Test summary");
    assert_eq!(detail.takeaways.len(), 2);
    assert_eq!(detail.topics.len(), 2);
    assert_eq!(detail.related.len(), 1);
    assert_eq!(detail.related[0].id, "abc123");
  }

  #[test]
  fn deserialize_wiki_detail_no_related() {
    let json = r#"{
      "summary": "Minimal",
      "takeaways": [],
      "topics": []
    }"#;
    let detail: WikiDetail = serde_json::from_str(json).unwrap();
    assert_eq!(detail.summary, "Minimal");
    assert!(detail.related.is_empty());
  }
}
