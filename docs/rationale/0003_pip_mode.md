# 0003 — PiP (Picture-in-Picture) Mode

## Problem

When using yp as a background music player, the full TUI occupies an entire terminal window — thumbnail, search results, input box, footer. This is wasteful when the user just wants to see what's playing while working in another app. There's no way to keep a minimal "now playing" indicator visible without dedicating a full-sized terminal to yp.

## Decision

Add a PiP toggle (Ctrl+M) that shrinks the terminal window to a compact size (~550x350px) positioned at the bottom-right corner of the screen, showing only the Now Playing pane and playback status. Pressing Ctrl+M again restores the original window size and position.

## Approach: osascript Window Manipulation

### Why osascript?

On macOS, there are three practical ways to resize a terminal window from within a running process:

1. **Xterm escape sequences** (`\x1b[4;h;wt`, `\x1b[3;x;yt`) — Portable in theory, but querying window geometry requires parsing terminal responses from stdin, which conflicts with crossterm's event loop. Terminal support is inconsistent (Terminal.app has limited support, Alacritty ignores them entirely).

2. **osascript (AppleScript)** — Native macOS mechanism. Can query and set window bounds reliably. Terminal.app and iTerm2 expose their own scripting interfaces (no Accessibility permissions needed). Other terminals fall back to System Events (requires Accessibility).

3. **Native Cocoa APIs** via FFI — Maximum control but requires linking AppKit, adds significant complexity, and is overkill for a simple resize/move.

osascript wins on reliability and simplicity. The tradeoff is macOS-only, which aligns with yp's primary target platform.

### Terminal Detection

The `TERM_PROGRAM` environment variable identifies the running terminal:

| Value            | Terminal     | API Used          | Permissions       |
|------------------|-------------|-------------------|--------------------|
| `Apple_Terminal`  | Terminal.app | App scripting     | None               |
| `iTerm.app`       | iTerm2      | App scripting     | None               |
| `ghostty`         | Ghostty     | System Events     | Accessibility      |
| Other/unset       | —           | Not supported     | —                  |

PiP is only supported on these three terminals. Other terminals silently hide the Ctrl+M keybinding and footer hint.

Terminal.app and iTerm2 use the `bounds` property (`{left, top, right, bottom}`), while Ghostty uses System Events with separate `position` and `size` properties. The window module normalizes both formats to `{x, y, width, height}`.

### Ghostty Fullscreen Handling

Ghostty users commonly run fullscreen. Since macOS native fullscreen windows can't be freely resized/repositioned, PiP must exit fullscreen first. This uses the `AXFullScreen` accessibility attribute via JXA:

```javascript
ObjC.import('ApplicationServices');
var win = Application('System Events').processes.byName('ghostty').windows[0];
win.attributes.byName('AXFullScreen').value = false;
```

After exiting fullscreen, the code waits 750ms for the macOS animation to complete, then re-queries window geometry before resizing to PiP. On restore, fullscreen is re-entered if it was active before PiP.

**Why not xterm escape sequences?** Ghostty does not implement CSI `t` (xterm window manipulation) sequences. We initially attempted this approach but discovered it's not supported in Ghostty's VT implementation.

### Screen Size Query

PiP positions the window at the bottom-right of the screen. Getting the screen resolution uses JXA (JavaScript for Automation) with the AppKit bridge:

```javascript
ObjC.import('AppKit');
$.NSScreen.mainScreen.frame.size.width + ',' + $.NSScreen.mainScreen.frame.size.height
```

This avoids Finder hacks or slow `system_profiler` calls. Falls back to 2560x1440 if the query fails.

## PiP Layout

The compact TUI layout in PiP mode strips everything except what matters:

```
┌─────────────────────────────────┐
│ ♪ 01:23 / 04:56 ▶              │  ← status bar (1 row)
├─────────────────────────────────┤
│ ╭─ Now Playing [pip] ─────────╮ │
│ │                             │ │
│ │  Song Title                 │ │  ← main pane (fills rest)
│ │                             │ │
│ │  Uploader  Artist Name     │ │
│ │  Duration  4:56             │ │
│ ╰─────────────────────────────╯ │
│ [Ctrl+M] exit PiP               │  ← hint (1 row)
└─────────────────────────────────┘
```

No thumbnail, no search results, no input box, no full footer. Just the track info and playback status.

## State Management

- `pip_mode: bool` — whether PiP is active
- `pip_original_geometry: Option<WindowGeometry>` — saved window position/size before entering PiP

On exit (normal or Ctrl+C), `restore_pip()` restores the original geometry so the terminal doesn't stay tiny after yp closes.

## Dimensions

- **PiP window**: 550 x 350 pixels (~65 cols x 22 rows at typical font sizes)
- **Position**: bottom-right corner, 30px margin from screen edge
- **Original window**: saved and restored exactly as it was

## Limitations

- **macOS only** — osascript is a macOS technology. On Linux, this feature is a no-op (the key binding works but the window manipulation will fail gracefully with a logged warning).
- **Accessibility permissions** — Required only for terminals other than Terminal.app and iTerm2. macOS will prompt on first use.
- **No always-on-top** — Neither Terminal.app nor iTerm2 expose a "float above all windows" property via AppleScript. The PiP window can be occluded by other windows. This is acceptable — the user can click the PiP window to bring it forward.
- **Multi-monitor** — The PiP positions relative to the main screen. On multi-monitor setups, it always goes to the bottom-right of the primary display.
