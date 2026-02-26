# Code Review TODO

## HIGH

- [x] #1 — Cache resized thumbnail (main.rs:764) — Lanczos3 resize runs every frame; cache by image+area dimensions
- [x] #2 — Panic hook for terminal restore (main.rs:1373) — register `set_hook` calling `ratatui::restore()`

## MEDIUM

- [x] #3 — Atomic yt-dlp parsing (main.rs:651) — use `--print "%(title)s\t%(id)s"` instead of alternating lines
- [x] #4 — Clamp input cursor to widget bounds (main.rs:1189) — add scroll offset for long input
- [x] #5 — Pass video_id in LoadResult (main.rs:412) — avoid fragile URL splitting

## LOW

- [x] #6 — Extract themes to `src/theme.rs` (main.rs:130-310) — reduce main.rs size
