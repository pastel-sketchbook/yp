# Code Review TODO

## HIGH

- [x] #1 — Cache resized thumbnail (main.rs:764) — Lanczos3 resize runs every frame; cache by image+area dimensions
- [x] #2 — Panic hook for terminal restore (main.rs:1373) — register `set_hook` calling `ratatui::restore()`
- [ ] #7 — Ghostty PiP: use direct AX API instead of System Events — Current approach uses
  `System Events` AppleScript which requires macOS **Automation** permission (error -1743).
  Alternative: call `AXUIElementCreateApplication(pid)` directly via JXA/ObjC bridge to
  read/write window position, size, and AXFullScreen. This uses **Accessibility** permission
  instead, which may be more intuitive for users to grant. Steps:
  1. Find Ghostty PID via `NSWorkspace.sharedWorkspace.runningApplications` (no permission needed)
  2. Create AXUIElement via `AXUIElementCreateApplication(pid)`
  3. Read position/size via `AXUIElementCopyAttributeValue` + `CFCopyDescription` parsing
  4. Write position/size via `AXValueCreate` with `NSMakePoint`/`NSMakeSize`
  5. Toggle fullscreen via `AXUIElementSetAttributeValue` with boolean for AXFullScreen
  Note: JXA struct handling (AXValueCreate with NSMakePoint) is uncertain — may need
  a small Swift helper binary instead.

## MEDIUM

- [x] #3 — Atomic yt-dlp parsing (main.rs:651) — use `--print "%(title)s\t%(id)s"` instead of alternating lines
- [x] #4 — Clamp input cursor to widget bounds (main.rs:1189) — add scroll offset for long input
- [x] #5 — Pass video_id in LoadResult (main.rs:412) — avoid fragile URL splitting

## LOW

- [x] #6 — Extract themes to `src/theme.rs` (main.rs:130-310) — reduce main.rs size
