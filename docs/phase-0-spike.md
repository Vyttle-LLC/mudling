# Phase 0: the gpui spike

Phase 0 asked whether gpui can carry MUDling's desktop shell (PRD §14). The
spike shows a virtualized chat over 100,000 replayed lines, a command input
that takes IME composition, and a first accessibility pass through
AccessKit.

**Verdict: gpui can carry it.** Every budget measured here was met on this
Mac with the machine reasonably quiet. The frame budget is missed when the
machine is heavily oversubscribed; that's covered under Results. The gaps
below are things to build, not reasons to change toolkit.

All numbers were taken on 2026-10-29 on an Apple M3 Pro (12 cores, 36 GB,
macOS 27.0.1), with a 60 Hz display and release builds. Other agents were
building on the same machine, so **every number is listed with the
one-minute load average** (`uptime`) when it was taken. A load of 12 means
all 12 cores are busy.

## Results

| What (PRD §6) | Budget | Measured | Load | Verdict |
|---|---|---|---|---|
| Append to a 100,000-line scrollback (parse, push and evict), real logs | p99 ≤ 0.2 ms | p50 0.12 µs, **p99 0.25 µs**, max 15 µs | 11.1 | Pass, about 800× under |
| Append, synthetic lines | p99 ≤ 0.2 ms | p50 0.21 µs, **p99 0.50 µs**, max 10 µs | 11.1 | Pass |
| Frame at 50 lines/s, 100,000 lines of scrollback, pinned to the newest line (3 runs) | p99 ≤ 8 ms | p50 1.2 / 1.3 / 1.4 ms, **p99 6.0 / 7.7 / 5.1 ms**, max 24 / 36 / 10 ms | 10.5 / 11.4 / 10.9 | Pass, with little margin at this load |
| The same frames under heavy load (3 runs) | p99 ≤ 8 ms | p50 1.2–1.3 ms, **p99 14–20 ms**, max 34–45 ms | 45 / 48 / 53 | **Miss** (see below) |
| Frame while scrolling back through history as lines arrive (3 runs) | informational (same 8 ms) | p50 1.1–1.3 ms, p99 4.5–6.2 ms | 10.5–11.4 | Within 8 ms |
| Find over 100,000 lines, every match, real logs | ≤ 50 ms | p50 2.6 ms, **p99 4.6 ms** | 11.1 | Pass |
| Find, synthetic lines | ≤ 50 ms | p50 5.1 ms, **p99 8.9 ms** | 11.1 | Pass |
| Find, first 500 matches (newest first) | informational | p99 3.0 ms (logs), 1.9 ms (synthetic) | 11.1 | — |
| Memory: the 100,000 lines themselves | informational (≤ 200 MB with a 10,000-room map) | 8.7 MB heap (logs), 11.7 MB (synthetic) | 11.1 | Well under |
| Memory: the whole app | informational | 35 MB resident after loading, 122 MB after streaming and scrolling | 10.5–11.4 | Well under |
| Time to first frame (launch, read and parse the logs, open the window, draw) | informational | 323 / 205 / 226 ms, of which reading and parsing 100,000 lines took 33–59 ms; the first frame itself took 7–8 ms | 10.5–11.4 | — |

**Scope of the frame numbers.** These frames hold the chat and the command
input only. Phase 0 has no side panels, map or split, so this is not yet the
PRD's "every panel open and the chat split" case. Lines arrived at 50 a
second (about 990 frames in 20 seconds); scrolling drew every display frame
(300 frames in 5 seconds at 60 Hz).

**The miss under heavy load.** At a load average around 50 on 12 cores, the
median frame stays at 1.2 ms but the slowest one in a hundred takes 14–20 ms.
A `sample` profile taken during such a run shows the main thread spending
about 1.2 ms of work per frame; the rest is waiting for a core. gpui's frame
timer measures wall time, so preemption counts against the budget. Plan:

1. Measure on the reference machines with nothing else running (PRD §6),
   which is what the budget is defined against.
2. Report CPU time per frame next to wall time, so contention and real cost
   can be told apart. This needs a frame hook in gpui (gap 7 below).
3. Keep the main thread's work per frame small as panels arrive. The PRD's
   rules for stores and revisions (§5 #5, #7) mean a line redraws only the
   chat, and that is what keeps the headroom.

**Find under load.** Another tester measured find at a p99 of 175–378 ms with
about seven builds running. Find itself is a single scan over the plain text,
with no allocation per line, and it measured 4.6 ms at load 11. To return
early results under any load, find now searches newest first and stops at a
limit; the first 500 matches come back in about 3 ms. Moving find off the UI
thread (PRD §12) is the Phase 1 step that keeps a slow find from ever
touching a frame.

## How each number was measured, and how to reproduce it

Session logs hold other players' chat. They are read at runtime from the
paths given and are never committed (`.gitignore` covers `*.log`, `logs/`
and `reports/`). Every command also has a `--synthetic` form, which uses
made-up lines and runs anywhere.

```sh
cargo build --release

# Append and find, headless. --check exits non-zero when a budget is missed.
./target/release/mudling-bench --json reports/bench.json ~/dev/vyttle/mud-client-spike/logs/session-*.log
./target/release/mudling-bench --check --json reports/bench-synthetic.json

# Frame times, time to first frame and memory, from the desktop binary.
./target/release/mudling-desktop --replay ~/dev/vyttle/mud-client-spike/logs/session-*.log \
    --measure --report reports/frames.json
./target/release/mudling-desktop --synthetic --measure --report reports/frames-synthetic.json

# Look at it: 100,000 lines of history, then 50 new lines a second.
./target/release/mudling-desktop --replay ~/dev/vyttle/mud-client-spike/logs/session-*.log
```

- **Append** (`mudling-bench`): fills a 100,000-line scrollback, then times
  100,000 more appends one by one. Each parses a raw line (ANSI SGR) and
  pushes it, which evicts the oldest line. With the logs (170,709 lines of
  server text from 90 logs) the lines are real; synthetic lines carry ANSI
  color. A counting allocator measures the heap the lines take.
- **Find** (`mudling-bench`): four needles (two common words, a phrase and
  one that never matches, which forces a full scan), 25 rounds each, so the
  p99 comes from 100 samples. It's timed twice: every match, and the first
  500 matches.
- **Frames** (`mudling-desktop --measure`): gpui times each frame it draws
  (render, layout, prepaint, paint and the hand-off to Metal) and prints it
  to stderr when `ZED_MEASUREMENTS=1`. The binary re-runs itself with that
  set and sorts the frames by phase: `startup` (open and settle for 1 s),
  `stream` (20 s at 50 lines a second, pinned to the newest line) and
  `scroll` (5 s scrolling back while lines still arrive). The measured run
  is paced by gpui's frame callbacks rather than timers, because timers fire
  late on a busy machine and would thin out the frames being measured. It
  also draws while another app has focus, and holds off App Nap.
- **Report**: `--report` writes JSON with the phases' p50, p99 and max, the
  load average, time to first frame, the memory and whether the stream
  phase met 8 ms.

### Windows and Linux

The workspace builds for both, and `.github/workflows/ci.yml` builds, tests
and runs the headless benchmark on macOS, Windows and Linux, plus a format
and clippy job. **None of that has run yet**: CI starts once the pull request
is published, and this Mac has no Windows or Linux toolchains, so the
non-macOS code paths were reviewed by hand but not compiled. The benchmark in
CI records numbers without gating on them, since shared runners aren't
reference hardware.

To take the measured replay on a Windows laptop or a Linux desktop, copy the
logs there (never into the repo), then:

```sh
cargo build --release
./target/release/mudling-desktop --replay path/to/session-*.log --measure --report reports/frames.json
# Linux: run once under X11 and once under Wayland. gpui picks Wayland when
# WAYLAND_DISPLAY is set; unset it (or log in to an X11 session) for X11.
```

On Linux, install `libfontconfig-dev libfreetype-dev libwayland-dev
libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev` first.

## What was verified, and what is only documented

| Claim | How it was checked | Status |
|---|---|---|
| Build, tests, clippy on macOS | `cargo build --release`, `cargo test --workspace` (23 tests), `cargo clippy --workspace --all-targets --release`, `cargo fmt --check` | Verified |
| 100,000-line replay renders, stays pinned while streaming, scrolls | The measured runs above, the AX check below (36 visible line elements, the newest last), and the tests `follows_the_newest_line_while_pinned` and `scrolled_back_view_holds_still_while_lines_are_trimmed` | Verified programmatically. **I could not look at the window:** this session has no Screen Recording permission, so the look (column, colors, fonts) is unchecked. See the manual steps |
| Only visible lines are laid out | `uniform_list` renders only the rows in view; the AX tree holds 36 line elements out of 100,000 | Verified |
| Trimming costs nothing to draw | The ring buffer drops the oldest line in place; the view moves its offset by one line instead of re-laying out (tested by mutation: removing the shift makes the test fail) | Verified |
| IME composition | Tests drive the same input-handler calls macOS makes for `setMarkedText` and `insertText` (Japanese "き" marked, then "北" committed; Return while composing doesn't send; UTF-16 offsets with emoji). Typing through the test platform's input handler and Return echo the command, and typing doesn't move the chat | **Verified programmatically only.** No real IME was used; see the manual steps |
| VoiceOver can reach the chat lines and the input | `mudling-desktop --synthetic --ax-check` reads the tree through the `NSAccessibility` methods VoiceOver's server calls, from inside the app, and passes five checks: the chat is an `AXGroup` with subrole `AXApplicationLog` named "Chat"; the visible lines are `AXStaticText`, newest last; focus is an `AXTextField` named "Command"; new lines arrive in an `AXApplicationStatus` live region, as both its value and its title; gpui's AccessKit tree focuses a `TextInput` named "Command" | Verified programmatically, in process. Querying from another process with `AXUIElement` needs the Accessibility permission, which this session doesn't have. Not yet heard with VoiceOver |
| VoiceOver announces new lines | The live region node is `Live::Polite` in our code, and `accesskit_macos` posts `NSAccessibilityAnnouncementRequested` when a live node with a value is added. The AX check confirms the node and its text; gpui's tree dump doesn't include `live`, so the flag itself is checked by reading the code path | Partly verified; needs a listen with VoiceOver |
| NVDA (Windows) and Orca (Linux) | The same AccessKit tree goes to `accesskit_windows` (UI Automation, `LiveRegionChanged`) and `accesskit_unix` (AT-SPI), both enabled at this gpui version. Those two adapters announce a live node's name (its label), while macOS announces its value, so the live region carries the new lines in both; the AX check confirms both | **Unverified**: code path present, manual steps below |

## Why this gpui

The workspace pins `gpui = { package = "gpui-pre", version = "=0.3.10" }`.

- **The official release can't do the job.** `gpui` on crates.io is 0.2.2,
  from October 2025, and has no accessibility support at all.
- **Zed's `main` can.** Since then Zed has built AccessKit into gpui: element
  roles and labels, values, focus, accessibility actions, synthetic child
  nodes, and adapters for macOS, Windows and Linux. That is most of what
  this ticket expected to build.
- **A git dependency on Zed doesn't build on its own.** gpui inside Zed relies
  on the `[patch.crates-io]` entries in Zed's root `Cargo.toml` (forks of
  `async-task` and `calloop`, a git revision of `wgpu`), and patches don't
  carry over to a crate that depends on Zed.
- **`gpui-pre` is a published, exact snapshot.** Jason Lee (`huacnlee`, who
  maintains gpui-component) publishes it weekly. 0.3.10 is a snapshot of
  [zed@08cbb88](https://github.com/zed-industries/zed/commit/08cbb88a152c3751a43e853425a70e9f631a5918)
  (2026-10-25). `Cargo.lock` pins the checksum.

### Do its sources match zed@08cbb88?

I downloaded Zed's source at `08cbb88a` and diffed each `gpui-pre-*`
crate's sources against the Zed crate it names in its metadata (20 crates).
The result:

- **Identical:** 17 crates, including `gpui_macos`, `gpui_windows`,
  `gpui_linux`, `gpui_platform`, `gpui_apple`, `scheduler` and every
  accessibility file in `gpui` (`window/a11y.rs`, its debug module and the
  element code).
- **Changed, each labelled "Modified for gpui-pre" at the top:**
  - `gpui/src/action.rs`: the `actions!` macro uses `$crate::Action` in
    place of `gpui::Action`.
  - `gpui_macros`: the derive macros pass their output through an added
    `gpui_pre_facade_paths.rs`, which rewrites `gpui::` paths when the
    `gpui-kit` facade crate is in use. It only rewrites tokens; it reads no
    files and runs nothing.
  - `gpui_wgpu`: three one-line changes that adapt it to the crates.io
    `wgpu` 30.0.1 API.
- **Dependencies differ where Zed patches them:** `gpui-pre` uses crates.io
  `async-task` 4.7, `calloop` 0.14 and `wgpu` 30.0.1, where Zed uses its own
  forks and a later `wgpu` revision. One example of what that loses: Zed's
  comment says 30.0.1 lacks a fix for a Vulkan surface-drop panic. That
  touches the Windows and Linux renderers, not macOS.

### The long-term pin

| Option | What it costs | Gains |
|---|---|---|
| **Stay on a `gpui-pre` snapshot**, re-diffed on each upgrade | A short diff per upgrade (scripted above). Trust in a third-party publisher. Zed's dependency fixes arrive only when `gpui-pre` adopts them | Builds today; no clone; tracks Zed weekly |
| **Git dependency on Zed at a fixed rev, with Zed's needed `[patch]` entries copied into our root `Cargo.toml`** | A first fetch of Zed's repository (about 550 MB, cached afterwards and in CI). Copying and keeping the 4–5 patches that gpui needs (`async-task`, `calloop`, `wgpu`, `khronos-egl`) in step with each upgrade. Cargo resolves Zed's whole workspace | Exactly Zed's code and dependency fixes; the source is Zed's own repository |
| **Vendor gpui's crates into `vendor/`** | About 20 crates of someone else's code in our tree, and a manual sync per upgrade | Full control; patches for missing accessibility can live in our tree while they go upstream |
| **An official gpui release from Zed** | Waiting: there has been none since 0.2.2 | The ideal end state |

**Recommendation:** for Phase 1, move to the **git dependency at a fixed
Zed rev with the copied patches**. MUDling is public, and its UI toolkit
should come from Zed's own repository with Zed's own dependency fixes, not
through a third party. The patch list is short and changes rarely. Keep the
diff script's approach as the upgrade check, and switch to an official
release as soon as Zed publishes one with AccessKit. Vendoring is the
fallback if we end up carrying our own gpui changes for long.

## What gpui lacks, and the plan for each

Zed's AccessKit work changed this list a lot: the expected "build an
AccessKit adapter onto gpui's window" is already done upstream. What remains:

1. **No official release with accessibility.** Plan: the pin above.
2. **A virtualized list shows screen readers only the visible lines.** The
   chat reads line by line within the window, but VoiceOver can't move past
   the first or last visible line into history, and the list doesn't say how
   long it is. Plan: give the chat `aria_size_of_set` (the scrollback
   length) and each line `aria_position_in_set`; render a few lines beyond
   each edge so a screen reader can step past it; answer AccessKit's
   `ScrollUp`, `ScrollDown` and `ScrollIntoView` actions on the chat node by
   moving the view. That fits the split-screen scrollback of Phase 3, where
   the history pane is the one navigated line by line.
3. **No live-region API on elements.** gpui's `div` has no `aria_live`; we
   set `Live::Polite` on the node through `a11y_synthetic_children`, and
   make each batch of new lines a new node so a repeated line is still
   announced. gpui's tree dump also omits `live`. Plan: contribute
   `aria_live()` and the dump field upstream (both small).
4. **Announcement flooding.** At 50 lines a second every frame announces.
   Each announcement carries at most the newest five lines, but that is
   still too much speech in a fight. Plan: a core-side announcer that
   coalesces lines (for example at most two announcements a second), lets
   plugins mark which lines matter (tells, combat) and gives the player a
   setting. This is product design as much as plumbing, so it belongs with
   the screen-reader user testing of Phase 8.
5. **The command input exposes only its value.** It has a role, a name, a
   placeholder and its text, but no caret or selection, so VoiceOver can't
   read character by character inside the field. Plan: add a `TextRun`
   child with character lengths and the text selection through
   `a11y_synthetic_children`, as gpui's accessibility guide describes for
   custom editors. Phase 3, with the floating command bar.
6. **IME works through gpui's input handler, with no gaps found so far.**
   gpui implements `NSTextInputClient` on macOS, IMM composition on Windows,
   and `text-input-v3` and XIM on Linux. The input is plain: no mouse
   selection yet. Plan: verify with real IMEs on all three (manual steps
   below) and add mouse selection with the command bar.
7. **No public frame-timing hook.** The measuring mode depends on
   `ZED_MEASUREMENTS`, a debug printout. Plan: contribute a per-frame
   callback with wall and CPU time, or use gpui's `profiler` feature, and
   drive the benchmark's `--ui` mode (PRD §13) from it.
8. **Uniform-height rows mean no wrapping.** Long lines are clipped at the
   column's edge. Real MUD lines are short (p99 79 characters, longest 203,
   across the logs), but wrapping is still needed. Plan: wrap in the core at
   the column width in characters (the chat is monospaced), so each visual
   row keeps the same height and the list stays a `uniform_list`. Rewrap
   only the visible rows when the width changes.
9. **Inactive windows are drawn at most about 30 times a second** by
   default. That's right for daily use; the measuring mode turns it off.
10. **The accessibility tree costs nothing until a screen reader connects**,
    so the frame numbers above don't include it. Plan: repeat the measured
    run with VoiceOver on, once the announcer (gap 4) exists.
11. **24-bit ANSI color** (`38;2;r;g;b`) is skipped, not drawn. The 16 and
    256 colors and bold are drawn. Plan: add it when a game needs it.

## Manual checks

### Look and scrolling (macOS)

1. `./target/release/mudling-desktop --replay ~/dev/vyttle/mud-client-spike/logs/session-*.log`
2. The chat should sit in a centered column about 100 characters wide,
   Night (#111A16) with Stone text in dark mode, and Parchment (#F8F7F2)
   with Slate text in light mode. Switch the system appearance and it
   should follow.
3. New lines arrive at 50 a second and the view stays on the newest line.
4. Scroll up: the view stops following and holds still while lines keep
   arriving. Scroll back to the bottom: it follows again.

### IME (macOS)

1. Add an input source: System Settings → Keyboard → Text Input → Edit →
   "+" → Japanese (Romaji), and Chinese (Pinyin – Simplified).
2. Run the app with or without `--synthetic --rate 0`, and click into the
   command input if it isn't focused.
3. Japanese: switch with Ctrl-Space, type `kita`. The text should show
   underlined as "きた" with the candidate window under the field. Press
   Space to convert, then Return to confirm: the underline goes and nothing
   is sent. Press Return again: "北" (or what you chose) appears in the chat.
4. Chinese: type `beijing`, choose 北京 with its number key, then Return.
5. While composing, the chat must not move.

### VoiceOver (macOS)

1. Run `./target/release/mudling-desktop --synthetic --rate 1`, and turn on
   VoiceOver (⌘F5).
2. VoiceOver should announce the window "MUDling" and land in the text field
   "Command". Type "look": it echoes characters, and Return puts "look" in
   the chat.
3. Move into the chat: VO-Shift-Down on "Chat" (a log), then VO-Right
   and VO-Left read one line at a time.
4. Wait: about once a second VoiceOver should say the new line.
5. Optionally, Xcode's Accessibility Inspector shows the same tree: the
   "Chat" group with subrole `AXApplicationLog`, `AXStaticText` lines, and
   the `AXApplicationStatus` group holding the newest lines.
6. Known gap: VO-Right stops at the last visible line (gap 2).

### NVDA (Windows), unverified

1. `cargo build --release`, then `target\release\mudling-desktop.exe --synthetic --rate 1`.
2. Start NVDA. Focus should be the edit field "Command"; typing echoes, and
   Enter puts the command in the chat.
3. Press NVDA+Space for browse mode, or use object navigation
   (NVDA+Numpad 8/2/4/6) to reach the "Chat" log and read its lines.
4. New lines should be spoken as they arrive (UI Automation live region).

### Orca (Linux, X11 and Wayland), unverified

1. Install the libraries above, `cargo build --release`, then
   `./target/release/mudling-desktop --synthetic --rate 1`. Do it once in an
   X11 session and once under Wayland.
2. Start Orca (Super+Alt+S on GNOME). Focus should be the text entry
   "Command"; typing echoes, and Enter puts the command in the chat.
3. Use Orca's flat review (Numpad 7/8/9 for lines) or Tab and arrow keys to
   reach the "Chat" log and read its lines.
4. New lines should be spoken (AT-SPI live region, "polite").

## Code map

- `crates/mudling-core`: `ansi.rs` (SGR parsed once per line into plain text
  and style spans), `scrollback.rs` (the ring buffer and find), `replay.rs`
  (reads MUDPad's session logs: keeps the server text, drops its `[GMCP]`,
  `[MAP]`, `[telnet]`, `[auto-login]`, `[connected]` and `[disconnected]`
  notes and the blank lines they add; player input is never in the logs),
  `stats.rs`.
- `crates/mudling-bench`: the headless append and find benchmark.
- `crates/mudling-desktop`: `chat.rs` (the virtualized chat and its live
  region), `input.rs` (the command input and IME), `shell.rs` (the window's
  layout), `theme.rs` (brand colors and the ANSI palette), `measure.rs` (the
  measuring mode), `macos.rs` (the AX check), `stream.rs` (the replay pace
  for interactive runs).
