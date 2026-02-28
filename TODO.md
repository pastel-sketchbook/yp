# Code Review TODO

## HIGH

- [x] #1 — Cache resized thumbnail (main.rs:764) — Lanczos3 resize runs every frame; cache by image+area dimensions
- [x] #2 — Panic hook for terminal restore (main.rs:1373) — register `set_hook` calling `ratatui::restore()`
- [ ] #7 — Ghostty PiP: window resize not taking effect — Automation permission is
  granted and osascript returns success, but `set position`/`set size` via System Events
  don't visibly resize the Ghostty window. The PiP layout renders correctly (thumbnail
  fills the window) but the window stays at its original size. Diagnosis steps:
  1. Check log for `pip: queried AXFullScreen attribute` — confirms native fullscreen vs maximized
  2. Check log for `pip: setting PiP geometry` — confirms target size is correct
  3. Ghostty config has `macos-titlebar-style = hidden`, `window-width = 1600`, no `fullscreen` setting
  4. May be a Ghostty-specific issue where System Events `set position/size` is silently ignored
  Possible fixes:
  - Try `set bounds` instead of separate `set position`/`set size`
  - Try AX API directly (`AXUIElementSetAttributeValue` for `AXPosition`/`AXSize`)
  - Try sending Ghostty a resize via its own config/keybinding mechanism
  - Small Swift helper binary using ApplicationServices framework directly

## MEDIUM

- [x] #3 — Atomic yt-dlp parsing (main.rs:651) — use `--print "%(title)s\t%(id)s"` instead of alternating lines
- [x] #4 — Clamp input cursor to widget bounds (main.rs:1189) — add scroll offset for long input
- [x] #5 — Pass video_id in LoadResult (main.rs:412) — avoid fragile URL splitting

## LOW

- [x] #6 — Extract themes to `src/theme.rs` (main.rs:130-310) — reduce main.rs size
