# TUI Gap Analysis: Rust (`rusty-claude-cli`) vs TypeScript (`claude-code`)

## Executive Summary

The TypeScript Claude Code CLI uses a **custom fork of Ink** (React for terminals) with a full-screen alternate-screen layout, double-buffered cell rendering, Yoga-based flexbox, virtual scrolling, and 60fps animation. The Rust implementation uses **inline scrolling output** with crossterm, rustyline, and manual ANSI escape codes. The gap is substantial — the TS version is essentially a terminal GUI application, while the Rust version is a rich REPL.

This document catalogs every significant feature delta and proposes a concrete technical path for each.

---

## 0. Visual Comparison (Screenshots)

### Claude Code TS (v2.1.62)

The TS CLI runs in **alternate screen mode** — the entire terminal is its canvas:

```
┌─ Claude Code v2.1.62 ─────────────────────────┬─ Tips for getting started ─────┐
│                                                │ Run /init to create a ...      │
│         Welcome back Jinjing!                  ├─ Recent activity ──────────────┤
│            [pixel avatar]                      │ No recent activity             │
│  Opus 4.6 · Claude Max · org                   │                                │
│              ~/code                            │                                │
└────────────────────────────────────────────────┴────────────────────────────────┘

› hi                          ← user prompt: chevron + bold + highlighted background

● Hi! How can I help you?     ← response: bullet prefix, clean spacing

› list the files              ← second turn

● Read 1 file (ctrl+o to expand)   ← COLLAPSIBLE tool result
● Here are the files...

› /k█                         ← slash command autocomplete dropdown visible
  /keybindings    Open or create your keybindings...
  /git-clean...   Discard all local changes...
                                    [scrollbar visible on right edge]
```

**Key visual traits:**
- Orange/amber dashed borders on welcome panel (two-column flexbox layout)
- `›` chevron prompt with bold text on a subtle background highlight
- `●` green bullet for assistant messages (green = completed tool, changes color)
- Tool results are **collapsible** — "Read 1 file (ctrl+o to expand)"
- Slash command autocomplete is an **inline dropdown** overlaying the UI
- Scrollbar indicator on the right edge (red/blue segments)
- Fixed input area at bottom — always visible regardless of output length

### Rust `scode` CLI

The Rust CLI runs in **inline scrollback mode** — output appends to the terminal:

```
jinjingzhou@... % cargo run --bin scode --
Endpoint        https://api.anthropic.com
Permissions     danger-full-access
Branch          main
Workspace       dirty · 1 files · 1 unstaged
Directory       /Users/.../sudo-code/rust
Session         session-1777093886431-0
Auto-save       .scode/sessions/.../session-....jsonl

Type /help for commands · /status for live context · ...
> hi
Hi there! 👋 How can I help you today? ...

> what's in the dir
🦀 Thinking...
╭─ bash ─╮
│ $ ls -la
╰─────────╯

✓ bash

total 240
drwxr-xr-x@ 18 jinjingzhou staff    576 Apr 23 22:26 .
drwxr-xr-x@ 13 jinjingzhou staff    416 Apr 24 11:52 ..
...
[FULL 20+ line ls -la output dumped inline — not collapsible]

[markdown table rendered inline]

✓ ✨ Done
> /█
```

**Key visual traits:**
- Plain key-value startup header (no borders, no layout)
- `>` simple prompt, no background highlight
- Emoji-heavy responses (👋) — no structured bullet/glyph system
- Tool calls in box-drawing borders (`╭─ bash ─╮`) — this is good
- Tool output is **fully dumped** — `ls -la` shows all 20+ lines with no collapse
- Markdown tables render well (box-drawing with column alignment)
- `✓ ✨ Done` completion — no persistent status line
- No scrollbar, no fixed footer, no alternate screen
- Slash completion works but via rustyline inline, not as an overlay dropdown

### Side-by-Side Delta Summary

| Element | TS | Rust | Visual Impact |
|---------|:--:|:----:|---------------|
| Welcome banner | Two-column bordered panel with avatar | Plain key-value text | High — first impression |
| User prompt | `›` chevron, bold, background highlight | `>` plain text | Medium |
| Response prefix | `●` colored bullet | None (inline text) | Medium |
| Tool result display | Collapsible with "ctrl+o to expand" | Full dump, no collapse | **High** — biggest readability issue |
| Slash autocomplete | Overlay dropdown with descriptions | Inline rustyline completion | Medium |
| Screen mode | Alternate screen, fixed footer | Inline scrollback | **High** — input scrolls off screen |
| Status line | Persistent bottom bar | None after startup | High |
| Scrollbar | Visual indicator on right edge | None (terminal native) | Low |
| Borders/chrome | Orange dashed borders, themed | Box-drawing characters | Low — Rust borders look fine |

---

## 1. Architecture Gap

| Aspect | TypeScript (Claude Code) | Rust (sudo-code) | Gap |
|--------|--------------------------|-------------------|-----|
| **Rendering engine** | Custom Ink fork with React reconciler, Yoga flexbox layout, cell-based double-buffered screen | Direct `crossterm` writes + raw ANSI escapes | **Critical** — no layout engine, no composable component model |
| **Layout model** | Flexbox via Yoga (row/column, grow/shrink, padding, margin, gap, absolute positioning) | None — sequential `write!()` to stdout | **Critical** |
| **Screen mode** | Alternate screen buffer (DEC 1049) with mouse tracking | Inline/scrollback mode only | **Major** |
| **Render loop** | 60fps throttled (`FRAME_INTERVAL_MS=16`), diff-based blit (only changed cells written) | Synchronous writes on each event | **Major** |
| **Component model** | React components with hooks, context, memoization (React Compiler) | Procedural functions writing to `impl Write` | **Major** |
| **State management** | React state + context providers (Theme, AppState, Stats, FpsMetrics, ScrollChrome) | Mutable fields on `LiveCli` struct | **Moderate** |
| **Code structure** | `REPL.tsx` (~5,000 lines) + ~50 component files | `main.rs` (~3,600 lines) monolith | Both need refactoring |

### Assessment

The TS version has a **full terminal GUI framework** under the hood. Replicating this in Rust doesn't require porting Ink — it requires choosing the right Rust-native approach. Options:

1. **`ratatui`** — Immediate-mode TUI framework with layout, widgets, and double-buffered rendering. Closest equivalent to Ink's capabilities.
2. **Custom crossterm-based rendering** — Continue current approach but add layout primitives. Lower dependency risk but much more work.
3. **Hybrid** — Use `ratatui` for full-screen mode, keep current inline rendering for basic/non-TTY mode.

**Recommendation**: Option 3 (hybrid). The current inline rendering is fine for piped/scripted use. Add `ratatui` behind a `--tui` / `--fullscreen` flag for the interactive experience.

---

## 2. Feature-by-Feature Gap Table

### 2.1 Layout & Screen Management

| Feature | TS | Rust | Priority |
|---------|:--:|:----:|----------|
| Alternate screen buffer | ✅ `AlternateScreen` component (DEC 1049) | ❌ | P1 |
| Fixed header (sticky prompt) | ✅ `StickyTracker` in `VirtualMessageList` | ❌ | P2 |
| Fixed footer (input/spinner pinned) | ✅ `FullscreenLayout.bottom` slot | ❌ | P1 |
| Scrollable message area | ✅ `ScrollBox` with imperative API (`scrollTo`, `scrollBy`, `isSticky`) | ❌ Inline scrollback only | P1 |
| Terminal resize handling | ✅ Yoga re-layout on resize | ❌ No `terminal::size()` awareness | P2 |
| "N new messages" pill | ✅ `useUnseenDivider` + pill overlay | ❌ | P3 |
| Mouse support (click, drag, scroll) | ✅ SGR mode 1003, text selection, hover states | ❌ | P3 |
| Virtual scrolling (viewport culling) | ✅ `VirtualMessageList` with height caching | ❌ | P3 |

### 2.2 Status Bar / HUD

| Feature | TS | Rust | Priority |
|---------|:--:|:----:|----------|
| Persistent status line | ✅ `StatusLine` component with hook system | ❌ | P1 |
| Model name display | ✅ In status line | ✅ Startup banner only | P1 |
| Token usage (live) | ✅ Input/output tokens, % context used | ❌ | P1 |
| Cost tracking | ✅ Total cost, duration, lines +/- | ❌ | P2 |
| Rate limit utilization | ✅ 5-hour and 7-day utilization | ❌ | P3 |
| Git branch indicator | ✅ In status line | ✅ Startup banner only | P2 |
| Permission mode display | ✅ In status line | ✅ Startup banner only | P2 |
| Vim mode indicator | ✅ In status line | ❌ | P3 |
| Customizable via hooks | ✅ User-defined `statusLine` hook command | ❌ | P3 |

### 2.3 Spinner / Progress Indicators

| Feature | TS | Rust | Priority |
|---------|:--:|:----:|----------|
| Basic spinner animation | ✅ `SpinnerGlyph` at 50ms | ✅ Braille dots at ~60ms | ✅ Parity |
| Mode-aware spinner text | ✅ `SpinnerMode`: thinking/responding/tool_use | ⚠️ Single "Thinking…" message | P1 |
| Shimmer/glimmer effect | ✅ `GlimmerMessage` + `ShimmerChar` with position-based color interpolation | ❌ | P2 |
| Stall detection (3s timeout) | ✅ `useStalledAnimation` — color fades to red | ❌ | P2 |
| Thinking duration display | ✅ Shows elapsed time after thinking completes, 2s minimum display | ❌ | P2 |
| Token counter animation | ✅ Animated count-up of tokens in spinner row | ❌ | P2 |
| Teammate agent tree | ✅ `TeammateSpinnerTree` — tree of sub-agent statuses | ❌ | P3 |
| Tool execution indicator | ✅ `ToolUseLoader` — blinking circle (●) with green/red completion | ⚠️ Box-drawing borders only | P2 |

### 2.4 Interactive Components

| Feature | TS | Rust | Priority |
|---------|:--:|:----:|----------|
| Single-select menu | ✅ `CustomSelect` with keyboard nav, numeric shortcuts, scroll | ❌ Y/N text prompt only | P1 |
| Multi-select menu | ✅ `SelectMulti` with checkboxes | ❌ | P2 |
| Input within select options | ✅ Inline text input in select items (e.g., feedback on deny) | ❌ | P2 |
| Fuzzy search picker | ✅ `FuzzyPicker` component | ❌ | P2 |
| Confirmation dialogs | ✅ `Dialog` component with Enter/Esc keybindings | ❌ | P1 |
| Tabbed interface | ✅ `Tabs` component with keyboard nav | ❌ | P3 |
| Progress bar | ✅ `ProgressBar` component | ❌ | P2 |
| Keyboard shortcut hints | ✅ `KeyboardShortcutHint`, `Byline` | ❌ | P2 |

### 2.5 Permission Prompts

| Feature | TS | Rust | Priority |
|---------|:--:|:----:|----------|
| Per-tool permission UI | ✅ Specialized components: `BashPermissionRequest`, `FileEditPermissionRequest`, `FileWritePermissionRequest`, etc. | ❌ Generic text-based Y/N for all tools | P1 |
| Rich context display | ✅ Shows command preview, file path, diff preview in styled boxes | ⚠️ Plain text tool name + input JSON | P1 |
| Feedback on deny | ✅ Tab to expand inline text input for deny reason | ❌ | P2 |
| Permission dialog styling | ✅ `PermissionDialog` with themed borders, dividers | ❌ | P1 |
| Batch permission options | ✅ "Allow for session", "Always allow" options in select | ❌ | P2 |

### 2.6 Message Rendering

| Feature | TS | Rust | Priority |
|---------|:--:|:----:|----------|
| Markdown rendering | ✅ `Markdown` component with highlight.js | ✅ `TerminalRenderer` with syntect + pulldown-cmark | ✅ Parity |
| Streaming markdown | ✅ React state updates → re-render | ✅ `MarkdownStreamState` with safe boundaries | ✅ Parity |
| Syntax highlighting in stream | ✅ highlight.js in `Markdown` component | ⚠️ Only in code blocks, not during streaming | P2 |
| Code block borders | ✅ Themed box borders | ✅ `╭─ lang` / `╰─` borders | ✅ Parity |
| Tables | ✅ | ✅ Unicode box-drawing tables | ✅ Parity |
| Nested lists | ✅ | ✅ | ✅ Parity |
| Thinking/reasoning display | ✅ Distinct visual treatment, shimmer rainbow highlight for ultrathink | ⚠️ "▶ Thinking (N chars hidden)" summary only | P2 |
| Message connectors | ✅ `⎿` tree connector glyph for response blocks | ❌ | P3 |
| Search in conversation | ✅ Regex search with highlighting (`setSearchQuery`, `nextMatch`, `prevMatch`) | ❌ | P3 |
| Message selection cursor | ✅ Navigate messages with keyboard for actions | ❌ | P3 |

### 2.7 Tool Call Visualization

| Feature | TS | Rust | Priority |
|---------|:--:|:----:|----------|
| Tool call box borders | ✅ Themed borders | ✅ `╭─ name ─╮` borders | ✅ Parity |
| Tool-specific formatting | ✅ Per-tool renderers | ✅ Per-tool formatters (bash, read, write, edit, glob, grep) | ✅ Parity |
| Collapsible tool output | ✅ Truncation with expand | ❌ Full output always shown | P1 |
| Diff-aware edit display | ✅ Colored unified diff | ⚠️ Shows old/new strings but no colored diff | P2 |
| Tool timeline summary | ✅ Multi-tool turn summary | ❌ | P2 |
| Tool execution status icon | ✅ Blinking ● → green/red ● | ✅ ✓/✗ icons | ✅ Approximate parity |

### 2.8 Color & Theming

| Feature | TS | Rust | Priority |
|---------|:--:|:----:|----------|
| Named themes | ✅ 6 themes: dark, light, dark-ansi, light-ansi, dark-daltonized, light-daltonized | ❌ Single hardcoded `ColorTheme::default()` | P2 |
| Auto dark/light detection | ✅ OSC 11 background color query | ❌ | P2 |
| Color format support | ✅ rgb(), #hex, ansi256(), ansi:name | ⚠️ crossterm `Color` enum (limited) | P2 |
| Theme context system | ✅ `ThemeProvider` + `useTheme()` + semantic color keys (90+ fields) | ❌ | P2 |
| Live theme preview | ✅ `/theme` picker with instant preview | ❌ | P3 |
| Color-blind accessible themes | ✅ daltonized variants | ❌ | P3 |
| ANSI capability detection | ✅ Falls back 16→256→truecolor | ❌ Assumes 256-color | P3 |

### 2.9 Input & Editing

| Feature | TS | Rust | Priority |
|---------|:--:|:----:|----------|
| Line editing | ✅ Custom input with React | ✅ rustyline with emacs bindings | ✅ Parity |
| Slash command completion | ✅ | ✅ `SlashCommandHelper` | ✅ Parity |
| Multiline input | ✅ | ✅ Ctrl+J / Shift+Enter | ✅ Parity |
| File path completion | ✅ | ❌ Slash commands only | P2 |
| Vim mode | ✅ Full vim keybindings | ❌ Emacs only | P3 |
| Paste handling | ✅ Bracketed paste + image paste | ⚠️ Basic paste via rustyline | P3 |
| CJK IME support | ✅ `useDeclaredCursor` for IME positioning | ❌ | P3 |

---

## 3. Deep-Dive: Critical Gaps

### 3.1 The Alternate Screen Gap

**What the TS version does**: Enters DEC private mode 1049 (alternate screen buffer). The entire terminal becomes a canvas. The layout is:

```
┌─────────────────────────────────┐
│ [Sticky prompt header - if      │ ← Appears when scrolled up
│  user scrolls up past prompt]   │
├─────────────────────────────────┤
│                                 │
│ Scrollable message area         │ ← ScrollBox with virtual scrolling
│ (messages, tool output, etc.)   │
│                                 │
├─────────────────────────────────┤
│ Spinner / Tool status           │ ← Fixed bottom slot
│ ─── prompt divider ──────────── │
│ > user input                    │ ← Input area (fixed)
├─────────────────────────────────┤
│ model │ tokens │ cost │ branch  │ ← Status line (fixed)
└─────────────────────────────────┘
```

**What the Rust version does**: Everything writes sequentially to stdout. Scrollback is the terminal emulator's job. There is no pinned header/footer — when the model generates long output, the prompt scrolls off-screen.

**Why it matters**: The fixed footer with spinner + input is the single most impactful UX feature. Users always see what's happening and can always type.

**Technical approach in Rust**:
- Use `ratatui` with `crossterm` backend
- `Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(1), Constraint::Length(3), Constraint::Length(1)])` for the 3-zone split
- `ratatui::widgets::Paragraph` with `scroll` for the message area
- Custom widget for the status line
- `crossterm::terminal::enable_raw_mode()` + `EnterAlternateScreen`

### 3.2 The Interactive Select Gap

**What the TS version does**: `CustomSelect` renders a list of options with:
- Highlighted current option (background color change)
- Arrow key navigation (Up/Down/Home/End)
- Number keys for quick selection (1-9)
- Scrolling when options exceed visible area
- Inline text input within options (e.g., "Deny with feedback: [___]")

Permission prompts use this to offer: Allow once / Allow for session / Always allow / Deny / Deny with feedback.

**What the Rust version does**: Prints text and reads a single line (`y/N`).

**Technical approach in Rust**:
- **Option A**: Use `dialoguer` crate — provides `Select`, `MultiSelect`, `Confirm` out of the box. Clean API, battle-tested.
- **Option B**: Use `ratatui` `List` widget with custom keybinding handler. More control, integrates with the full-screen layout.
- **Option C**: Build a minimal select component with `crossterm` raw mode. Most control, most work.

**Recommendation**: Option B if going with ratatui for full-screen mode. Option A as an interim solution.

### 3.3 The Spinner Intelligence Gap

**What the TS version does**:
1. **Mode tracking**: Spinner changes appearance based on what the model is doing (thinking vs responding vs tool_use)
2. **Stall detection**: If no tokens arrive for 3+ seconds, the spinner color smoothly interpolates toward red over 2 seconds
3. **Shimmer effect**: The spinner label text has a traveling glimmer highlight (per-character color interpolation based on position and time)
4. **Token counter**: An animated count-up of output tokens
5. **Thinking duration**: After thinking completes, shows "Thought for Xs" with a 2-second minimum display time

**What the Rust version does**: A braille spinner with a static label ("Thinking…"). Switches to "Done" or "Failed" on completion.

**Technical approach in Rust**:
```rust
enum SpinnerMode {
    Thinking { start: Instant },
    Responding { tokens: usize, last_token_at: Instant },
    ToolUse { tool_name: String },
}

impl Spinner {
    fn update_mode(&mut self, mode: SpinnerMode) { ... }
    fn detect_stall(&self) -> f32 {
        // Returns 0.0..1.0 stall intensity based on time since last token
        let elapsed = self.last_token_at.elapsed();
        if elapsed < Duration::from_secs(3) { return 0.0; }
        ((elapsed.as_secs_f32() - 3.0) / 2.0).min(1.0)
    }
}
```

### 3.4 The Theme System Gap

**What the TS version does**: 90+ semantic color fields organized into categories (brand, UI chrome, semantic, diff, agent, rainbow). Six built-in themes. Auto-detection via OSC 11. Live preview in `/theme` command.

**What the Rust version does**: A single `ColorTheme` struct with 11 fields using `crossterm::style::Color`.

**Technical approach in Rust**:
```rust
pub struct Theme {
    // Brand
    pub claude: Color,
    pub permission: Color,
    // UI chrome
    pub prompt_border: Color,
    pub text: Color,
    pub subtle: Color,
    // Semantic
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    // Diff
    pub diff_added: Color,
    pub diff_removed: Color,
    // ... ~30 fields for practical parity (not all 90+)
}

pub fn theme(name: &str) -> Theme {
    match name {
        "dark" => Theme::dark(),
        "light" => Theme::light(),
        _ => Theme::dark(),
    }
}
```

---

## 4. Implementation Recommendations

### Should we use `ratatui`?

**Yes, for the full-screen mode.** Here's the breakdown:

| Approach | Pros | Cons |
|----------|------|------|
| **Continue custom ANSI** | No new deps, full control | Must build layout engine, scrolling, input handling, widget system from scratch |
| **`ratatui` full-screen** | Mature layout system, 100+ widgets, double-buffered rendering, active ecosystem | Learning curve, opinionated immediate-mode API, adds ~500KB to binary |
| **`ratatui` hybrid** | Best of both — inline mode for scripts/pipes, full-screen for interactive | Two code paths to maintain |

**Recommendation**: **Hybrid approach (`ratatui` + existing inline renderer)**

- Keep the current `render.rs` / `crossterm` pipeline for `--no-tui` / piped output
- Add `ratatui` behind a `tui` feature flag for the interactive experience
- The inline mode stays lean and fast; the full-screen mode gets the polish

### How to achieve the "live" feel of Ink in Rust?

Ink achieves its live feel through:
1. **60fps render loop** — Throttled re-renders at 16ms intervals
2. **Diff-based screen updates** — Only changed cells are written
3. **React reconciler** — Only re-renders changed subtrees

In Rust with `ratatui`:
1. **Event loop with tick rate** — Use `crossterm::event::poll()` with a 50ms timeout for animation
2. **Double-buffered terminal** — `ratatui::Terminal` already does diff-based rendering
3. **State-driven rendering** — The `draw()` closure re-renders the full UI; ratatui diffs against the previous frame

```rust
loop {
    // Draw the current state
    terminal.draw(|frame| {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),      // Messages (scrollable)
                Constraint::Length(3),    // Input area
                Constraint::Length(1),    // Status line
            ])
            .split(frame.area());

        render_messages(frame, chunks[0], &app.messages, app.scroll);
        render_input(frame, chunks[1], &app.input);
        render_status_line(frame, chunks[2], &app.status);
    })?;

    // Handle events with timeout for animation tick
    if crossterm::event::poll(Duration::from_millis(50))? {
        match crossterm::event::read()? {
            Event::Key(key) => handle_key(&mut app, key),
            Event::Resize(w, h) => { /* handled by ratatui */ },
            _ => {}
        }
    }

    // Tick animations (spinner, token counter, stall detection)
    app.spinner.tick();
    app.status.update_elapsed();

    // Check for streaming events from the API
    while let Ok(event) = api_rx.try_recv() {
        app.handle_stream_event(event);
    }
}
```

### Crate Recommendations

| Purpose | Crate | Notes |
|---------|-------|-------|
| Full-screen TUI | `ratatui` 0.29+ | Layout, widgets, double-buffered rendering |
| Terminal backend | `crossterm` 0.28 (already used) | Raw mode, events, colors |
| Interactive select (interim) | `dialoguer` 0.11 | Drop-in select/confirm before ratatui |
| Syntax highlighting | `syntect` 5 (already used) | Keep existing |
| Markdown parsing | `pulldown-cmark` 0.13 (already used) | Keep existing |
| Line editing | `rustyline` 15 (already used) | Keep for non-fullscreen mode |
| Async channels | `tokio::sync::mpsc` or `crossbeam-channel` | For API stream → TUI event bridge |

---

## 5. Phased Implementation Plan

### Phase 0: Foundation (1–2 days)

1. **Extract `main.rs` monolith** → `app.rs`, `format.rs`, `session_mgr.rs`
2. **Create `tui/` module** with `mod.rs`
3. **Add `ratatui` dependency** behind `full-tui` feature flag
4. **Add `dialoguer` dependency** for immediate interactive improvements

### Phase 1: Interactive Improvements — No Ratatui (2–3 days)

1. **Replace Y/N permission prompt** with `dialoguer::Select` offering: Allow / Allow for session / Deny
2. **Add spinner modes** (thinking/responding/tool_use) with mode-specific labels
3. **Add stall detection** to spinner (color shift after 3s of no tokens)
4. **Bottom-pinned status line** using crossterm cursor positioning (works without alternate screen)

### Phase 2: Full-Screen Mode with Ratatui (1–2 weeks)

1. **Alternate screen** with `crossterm::terminal::EnterAlternateScreen`
2. **3-zone layout**: scrollable messages / input+spinner / status line
3. **Scrollable message list** with PgUp/PgDn, sticky-to-bottom behavior
4. **Async event bridge**: API streaming events → `mpsc::channel` → ratatui event loop
5. **Raw mode input** replacing rustyline for full-screen mode

### Phase 3: Polish (1 week)

1. **Theme system** with dark/light themes (expand `ColorTheme` to ~30 fields)
2. **Collapsible tool output** (truncate at 15 lines, `e` to expand)
3. **Colored diff display** for edit_file results
4. **Terminal resize handling**
5. **"N new messages" indicator** when scrolled up

### Phase 4: Stretch Goals (ongoing)

1. Mouse support (scroll, click-to-expand)
2. Virtual scrolling for long conversations
3. Fuzzy picker for session selection
4. Vim mode keybindings
5. OSC 11 auto dark/light detection
6. Search in conversation

---

## 6. Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| `ratatui` fights with `rustyline` in raw mode | High | Use separate input widget in full-screen mode; keep rustyline for inline mode only |
| Streaming + ratatui event loop complexity | High | Use `mpsc::channel` to decouple; API thread sends events, TUI thread renders |
| Binary size increase (~500KB from ratatui) | Low | Feature-gated; only included when `full-tui` is enabled |
| Two rendering paths to maintain | Medium | Share formatting logic (markdown rendering, tool formatters); only layout differs |
| Breaking the working inline REPL | High | Phase 1 improvements work within the existing inline architecture; Phase 2 is additive |

---

## 7. Quick Wins (Can Ship Today)

These require no new dependencies or architectural changes:

1. **Spinner mode labels**: Change "Thinking…" to context-aware labels ("Thinking…" / "Responding…" / "Running bash…")
2. **Elapsed time in spinner**: Show seconds elapsed during thinking
3. **Permission prompt context**: Show what the tool will do (command preview for bash, file path for read/write) in the permission prompt text
4. **Remove 8ms stream delay**: Already identified in existing enhancement plan
5. **Terminal width awareness**: Use `crossterm::terminal::size()` for table column calculations

---

*Generated: 2026-04-24 | Source comparison: claude-code TS (v2.1.88) vs sudo-code Rust (main branch)*
*Reference TS source: `/Users/jinjingzhou/code/joezhoujinjing/claude-code-source-code/`*
