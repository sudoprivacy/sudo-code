# TUI Gap Analysis: Rust (`scode`) vs TypeScript (`claude`)

*Source-level comparison. Every gap below was verified by reading the actual source code in both repositories.*

**TS source**: `/Users/jinjingzhou/code/joezhoujinjing/claude-code-source-code/` (v2.1.88)
**Rust source**: `rust/crates/rusty-claude-cli/src/` (this repo)

---

## 1. Welcome / Startup Screen

### What TS does

**Two-column bordered panel** with pixel avatar, model info, tips, and recent activity.

- `src/components/LogoV2/LogoV2.tsx:47` — Main component. Detects terminal width via `useTerminalSize()`, picks "horizontal" (≥70 cols) or "compact" layout.
- `src/components/LogoV2/LogoV2.tsx:331-436` — Horizontal layout: outer `<Box borderStyle="round" borderColor="claude">` with `flexDirection="row"` inner box containing left panel + vertical divider + right feed column.
- `src/components/LogoV2/LogoV2.tsx:251` — Border title: `Claude Code v{version}` rendered in the box's top border.
- `src/components/LogoV2/Clawd.tsx:73` — Pixel avatar using Unicode half-block characters (`▛███▜`, `▝▜`, `▛▘`). Four poses: default, arms-up, look-left, look-right.
- `src/components/LogoV2/feedConfigs.tsx:70` — "Tips for getting started" feed with onboarding steps + checkmarks.
- `src/components/LogoV2/feedConfigs.tsx:21` — "Recent activity" feed showing user's recent prompts with timestamps.
- `src/components/LogoV2/Feed.tsx:51-107` — Individual feed panel: bold colored title, truncated lines, optional dimmed footer.
- `src/components/LogoV2/FeedColumn.tsx:11-54` — Stacks multiple feeds vertically with `<Divider>` between them.
- `src/utils/logoV2Utils.ts:17-22` — Layout constants: `MAX_LEFT_WIDTH=50`, `BORDER_PADDING=4`, `DIVIDER_WIDTH=1`.

### What Rust does

**Plain text key-value list** with ASCII art logo.

- `main.rs:4558-4600` — `startup_banner()` method. Builds a format string with:
  - ASCII art: `███████╗██╗...` in ANSI color 117 (light blue), "Code" in color 208 (orange).
  - Key-value pairs (Model, Permissions, Branch, Workspace, Directory, Session, Auto-save) with dim labels (`\x1b[2m`).
  - Help line at bottom: "Type /help for commands..."
- `main.rs:3759` — `println!("{}", cli.startup_banner());` — emitted once at REPL start.

### Gap

| Aspect | TS | Rust |
|--------|:--:|:----:|
| Bordered layout | ✅ `<Box borderStyle="round">` with flexbox | ❌ Plain text |
| Two-column layout | ✅ `flexDirection="row"` with divider | ❌ Single column |
| Pixel avatar | ✅ `Clawd` component with poses | ❌ ASCII art logo |
| Tips/onboarding | ✅ Feed with checkable steps | ❌ Static help line |
| Recent activity | ✅ Timestamped prompt history | ❌ Nothing |
| Responsive layout | ✅ Switches to compact at <70 cols | ❌ Fixed layout |
| Version in border | ✅ Border title | ❌ Not shown |

---

## 2. User Prompt Display

### What TS does

**Chevron prefix with background highlight.**

- `src/components/PromptInput/PromptInputModeIndicator.tsx:54` — Renders `figures.pointer` (❯) as the prompt character: `<Text color={color} dimColor={isLoading}>{figures.pointer} </Text>`.
- `src/components/messages/UserPromptMessage.tsx:76-78` — User message box gets `backgroundColor="userMessageBackground"` — a subtle dark background highlight distinguishing user input from assistant output.
- `src/components/messages/HighlightedThinkingText.tsx:91,145` — The `›` chevron in message history uses `figures.pointer` with color based on selection state.

### What Rust does

**Plain `> ` prefix via rustyline.**

- `input.rs:108` — `LineEditor::new("> ", candidates)` — prompt is a static string.
- `main.rs:3758` — `let mut editor = input::LineEditor::new("> ", ...)` — creates the editor with `> ` prompt.
- No background highlight, no colored chevron, no visual distinction of user messages in scrollback.

### Gap

| Aspect | TS | Rust |
|--------|:--:|:----:|
| Prompt glyph | ❯ (`figures.pointer`) | `>` |
| Background highlight | ✅ `userMessageBackground` color | ❌ |
| Color varies by mode | ✅ Agent/bash mode changes color | ❌ |
| User messages in history | ✅ Highlighted background | ❌ No visual distinction |

---

## 3. Assistant Response Display

### What TS does

**Colored bullet prefix with response connector glyph.**

- `src/constants/figures.ts:4` — `BLACK_CIRCLE = env.platform === 'darwin' ? '⏺' : '●'` — platform-specific bullet.
- `src/components/ToolUseLoader.tsx:19-20` — Bullet color state machine:
  - `isUnresolved` → undefined (dim, blinking via `useBlink()`)
  - `isError` → `"error"` (red)
  - success → `"success"` (green)
- `src/components/messages/AssistantTextMessage.tsx:231-232` — Text messages get `BLACK_CIRCLE` with `color="text"`.
- `src/components/MessageResponse.tsx:22-23,37` — Response connector: `<Text dimColor={true}>{"  "}⎿  </Text>` — a dimmed `⎿` glyph left-aligned, non-selectable via `<NoSelect fromLeftEdge={true}>`.

### What Rust does

**No prefix, no visual structure.**

- `main.rs:7948-7952` — Streaming text is written directly: `write!(out, "{rendered}")?; out.flush()?;`.
- No bullet, no connector glyph, no visual distinction between assistant text and tool output.
- Tool completion uses `✓` / `✗` icons (`main.rs:8464-8490`), but these mark tool results, not the assistant's text blocks.

### Gap

| Aspect | TS | Rust |
|--------|:--:|:----:|
| Response bullet | ✅ ⏺/● with color state machine | ❌ None |
| Connector glyph | ✅ `⎿` dimmed for nested responses | ❌ None |
| Visual structure | ✅ Clear hierarchy (bullet → connector → content) | ❌ Flat text stream |

---

## 4. Tool Result Display & Collapsibility

### What TS does

**Collapsed by default with "ctrl+o to expand".**

- `src/components/CtrlOToExpand.tsx:29-45` — Renders `<KeyboardShortcutHint shortcut={expandShortcut} action="expand" parens={true} />` in dimmed text.
- `src/components/CtrlOToExpand.tsx:47-50` — Chalk-based string version: `chalk.dim(\`(${shortcut} to expand)\`)`.
- The keybinding is resolved via `useShortcutDisplay("app:toggleTranscript", "Global", "ctrl+o")` — customizable.
- Tool results show a **one-line summary** (e.g., "Read 1 file") with the expand hint. Full output is hidden unless expanded.
- `src/components/ToolUseLoader.tsx:11-41` — Status indicator: blinking ⏺ during execution → green ⏺ on success → red ⏺ on error.

### What Rust does

**Full output dumped inline, truncated only at generous limits.**

- `main.rs:8492-8497` — Truncation constants:
  ```
  READ_DISPLAY_MAX_LINES = 80
  READ_DISPLAY_MAX_CHARS = 6,000
  TOOL_OUTPUT_DISPLAY_MAX_LINES = 60
  TOOL_OUTPUT_DISPLAY_MAX_CHARS = 4,000
  ```
- `main.rs:8553-8590` — `format_bash_result()`: Builds a vector of lines starting with `✓ bash`, then appends the full stdout (truncated at 60 lines / 4000 chars) and stderr (same limits, colored red via `\x1b[38;5;203m`). All lines joined and returned as one string.
- `main.rs:9067-9070` — Emission path: `format_tool_result()` → `stream_markdown()` → `write!(stdout)`. No state tracking, no collapse/expand. The output is written once and cannot be hidden after the fact.
- `main.rs:8464-8490` — `format_tool_result()` dispatches per tool name: `format_bash_result`, `format_read_result`, `format_write_result`, `format_edit_result`, `format_glob_result`, `format_grep_result`, or `format_generic_tool_result`. Each dumps its output inline.
- Tool completion icon: `\x1b[1;32m✓\x1b[0m` (bold green) or `\x1b[1;31m✗\x1b[0m` (bold red), always static.

### Gap

This is the **single biggest readability gap** visible in the screenshots. A `ls -la` dumping 20+ lines of raw output vs a clean "Read 1 file (ctrl+o to expand)" one-liner.

| Aspect | TS | Rust |
|--------|:--:|:----:|
| Default display | One-line summary | Full output dump |
| Expand/collapse | ✅ ctrl+o toggle | ❌ |
| Truncation | Aggressive (collapsed by default) | Generous (60-80 lines) |
| Status animation | ✅ Blinking ⏺ → colored ⏺ | ✓/✗ static icons |

---

## 5. Spinner & Progress

### What TS does

**Multi-mode spinner with stall detection, shimmer, and token counting.**

- `src/components/Spinner/SpinnerAnimationRow.tsx` — Runs on `useAnimationFrame(50)` (50ms tick). Owns all time-derived values: frame index, glimmer position, token counter animation, elapsed time, stalled intensity, thinking shimmer.
- `src/components/Spinner/SpinnerGlyph.tsx` — The animated glyph character.
- `src/components/Spinner/GlimmerMessage.tsx` + `ShimmerChar.tsx` — Text shimmer effect: per-character color interpolation based on position and time.
- `src/components/Spinner/useStalledAnimation.ts` — When no tokens arrive for 3+ seconds, spinner color smoothly interpolates toward red over 2 seconds.
- `SpinnerMode` types: `'thinking'`, `'responding'`, `'tool_use'`.
- Shows elapsed time after thinking completes, with 2-second minimum display to avoid jank.
- `src/components/Spinner/TeammateSpinnerTree.tsx` — Tree of running sub-agent statuses.

### What Rust does

**Spinner is designed for animation but never actually animates.**

- `render.rs:48-116` — `Spinner` struct with 10 braille frames and `SavePosition`/`RestorePosition` cursor control — designed for in-place animation.
- **But**: `main.rs:4638-4648` — `tick()` is called **once** before the blocking `runtime.run_turn()` call. There is no animation loop. The spinner shows a single static frame (`⠋ 🦀 Thinking...`).
- Streaming happens inside `run_turn()` via `AnthropicRuntimeClient::stream_message()` (line 7880+). Tool calls are prefixed with `\n` (line 7981: `writeln!(out, "\n{}", format_tool_call_start(...))`), so they write below the spinner line, leaving "🦀 Thinking..." visible above.
- Text deltas write directly below via `write!(out, "{rendered}")` (line 7948-7950).
- After `run_turn()` returns, `finish()` (line 4653) prints `✔ ✨ Done` at the current cursor position (below all streaming output), or `fail()` (line 4670) prints `✘ ❌ Request failed`.
- **Net effect**: "🦀 Thinking..." appears once, stays frozen, streaming output appears below it, then "✔ ✨ Done" appears at the end. No continuous animation, no mode switching, no stall detection.

### Gap

| Aspect | TS | Rust |
|--------|:--:|:----:|
| Animation | ✅ 50ms continuous animation | ❌ Single frozen frame |
| Modes | ✅ thinking/responding/tool_use | ❌ Single label |
| Updates during streaming | ✅ Token count, elapsed time | ❌ Frozen until turn ends |
| Stall detection | ✅ Color fades to red after 3s | ❌ |
| Shimmer/glimmer | ✅ Per-character color wave | ❌ |
| Elapsed time | ✅ Shown after thinking | ❌ |
| Token counter | ✅ Animated count-up | ❌ |
| Sub-agent tree | ✅ TeammateSpinnerTree | ❌ |

---

## 6. Slash Command Autocomplete

### What TS does

**Overlay dropdown with descriptions, max 5 items visible.**

- `src/components/PromptInput/PromptInputFooterSuggestions.tsx:17` — `OVERLAY_MAX_ITEMS = 5`.
- `src/components/PromptInput/PromptInputFooterSuggestions.tsx:8-15` — `SuggestionItem` type: `{id, displayText, tag, description, metadata, color}`.
- `src/components/PromptInput/PromptInputFooterSuggestions.tsx:36-126` — `SuggestionItemRow`: renders command name + description, truncated to terminal width.
- `src/components/PromptInput/PromptInputFooter.tsx:124-129` — In fullscreen mode, suggestion data is portaled to `useSetPromptOverlay()` which renders them as a dropdown above the input area.
- Icons per type: `+` for files, `◇` for MCP resources, `*` for agents.

### What Rust does

**Inline rustyline completion.**

- `input.rs:23-99` — `SlashCommandHelper` implements rustyline's `Completer` trait.
- `input.rs:58-79` — `complete()` method: filters `self.completions` by prefix match. Returns `Vec<Pair>` where each `Pair` has `display` and `replacement` fields — both set to the command string itself. No description field.
- `input.rs:109-110` — Config: `CompletionType::List` — rustyline renders matches below the prompt in a flat list.
- `input.rs:200-211` — `slash_command_prefix()`: Only triggers completion when cursor is at end of line and input starts with `/`.
- `input.rs:213-220` — `normalize_completions()`: Deduplicates and filters to `/`-prefixed strings only.
- No descriptions, no overlay positioning, no icons, no scrollable list, no type indicators.

### Gap

| Aspect | TS | Rust |
|--------|:--:|:----:|
| Presentation | Overlay dropdown above input | Inline list below prompt |
| Descriptions | ✅ Shown next to command name | ❌ Name only |
| Max visible items | 5 (scrollable) | All matches dumped |
| Per-type icons | ✅ +/◇/* | ❌ |
| Truncation | ✅ Smart path/description truncation | ❌ |

---

## 7. Screen Mode & Layout

### What TS does

**Full-screen alternate screen with fixed footer.**

- `src/ink/components/AlternateScreen.tsx` — Enters DEC private mode 1049. Enables SGR mouse tracking (mode 1003). Constrains `Box height` to terminal rows.
- `src/components/FullscreenLayout.tsx:31-67` — Layout props: `scrollable` (messages), `bottom` (spinner + prompt, pinned), `overlay`, `bottomFloat`, `modal`.
- `src/components/FullscreenLayout.tsx:361` — `ScrollBox` with `flexGrow={1}` for message area.
- `src/components/FullscreenLayout.tsx:413-414` — Bottom slot: `flexShrink={0}`, `maxHeight="50%"` — never scrolls away.
- `src/ink/components/ScrollBox.tsx:82-133` — Imperative scroll API: `scrollTo()`, `scrollBy()`, `scrollToBottom()`, `isSticky()`.
- Enabled via `CLAUDE_CODE_NO_FLICKER=1` env var (default ON for internal builds).

Layout structure:
```
AlternateScreen (mouseTracking)
  └── FullscreenLayout (flexDirection="column")
        ├── StickyPromptHeader (appears when scrolled up)
        ├── ScrollBox (flexGrow=1, messages + overlays)
        ├── NewMessagesPill (floating, "N new messages")
        └── Bottom (flexShrink=0, maxHeight=50%)
              ├── SuggestionsOverlay
              ├── DialogOverlay
              ├── SpinnerWithVerb
              └── PromptInput (chevron + text input)
```

### What Rust does

**Inline scrollback, no layout control.**

- `main.rs:3737-3813` — `run_repl()`: A simple loop:
  1. `editor.read_line()` (line 3767) — rustyline handles prompt display and input
  2. `SlashCommand::parse()` (line 3777) — check for slash commands
  3. `cli.run_turn(&trimmed)` (line 3802) — blocking call that streams output to stdout
  4. Back to step 1
- `main.rs:4638-4678` — `run_turn()`: Creates `Spinner`, ticks once, calls `runtime.run_turn()` (blocking), then finishes spinner.
- Inside `runtime.run_turn()`, `stream_message()` (line 7880+) handles SSE events and writes rendered markdown/tool output directly to stdout via `write!(out, ...)`.
- `main.rs:7883-7887` — Output destination: `if self.emit_output { &mut stdout } else { &mut sink }` — either stdout or /dev/null, nothing in between.
- No alternate screen, no fixed regions, no scroll management, no cursor positioning for layout. When the model generates long output, the prompt scrolls off-screen.

### Gap

| Aspect | TS | Rust |
|--------|:--:|:----:|
| Alternate screen | ✅ DEC 1049 | ❌ |
| Fixed footer | ✅ Input always visible | ❌ Input scrolls away |
| Scrollable messages | ✅ ScrollBox with API | ❌ Terminal scrollback |
| Sticky header | ✅ Shows prompt when scrolled | ❌ |
| Mouse tracking | ✅ Click, drag, scroll | ❌ |

---

## 8. Permission Prompts

### What TS does

**Per-tool rich UI with select menu.**

- `src/components/permissions/PermissionPrompt.tsx` — Wraps `<Select>` with permission-specific options: Allow / Allow for session / Always allow / Deny / Deny with feedback.
- `src/components/permissions/BashPermissionRequest.tsx`, `FileEditPermissionRequest.tsx`, `FileWritePermissionRequest.tsx`, etc. — Specialized per-tool UIs showing command preview, file path, diff preview in styled boxes.
- `src/components/CustomSelect/select.tsx` — Keyboard-navigable list with Up/Down/Home/End, numeric shortcuts (1-9), scrolling.
- Tab to expand inline text input for deny feedback.

### What Rust does

**Generic Y/N text prompt for all tools.**

- `main.rs:7683-7730` — `CliPermissionPrompter` struct with single method `decide()`.
- `main.rs:7698-7706` — Output is plain `println!()` calls:
  ```rust
  println!("Permission approval required");
  println!("  Tool             {}", request.tool_name);
  println!("  Current mode     {}", self.current_mode.as_str());
  println!("  Required mode    {}", request.required_mode.as_str());
  // ... reason, input
  print!("Approve this tool call? [y/N]: ");
  ```
- `main.rs:7711-7714` — Reads one line from stdin via `io::stdin().read_line()`, accepts "y"/"yes" (case-insensitive). Everything else is a deny.
- `main.rs:7717-7721` — Deny reason is hardcoded: `"tool '{name}' denied by user approval prompt"`. No user feedback captured.
- The `request.input` field dumps the raw JSON tool input — not a human-readable summary.
- No select menu, no per-tool formatting, no "allow for session" option.

### Gap

| Aspect | TS | Rust |
|--------|:--:|:----:|
| UI type | Select menu with options | Y/N text input |
| Per-tool formatting | ✅ Specialized components | ❌ Generic for all |
| Batch options | ✅ Allow once/session/always | ❌ Binary y/n |
| Deny feedback | ✅ Inline text input | ❌ |
| Context display | ✅ Command preview, diff preview | ⚠️ Raw tool name + input JSON |

---

## 9. Status Line

### What TS does

**Persistent bottom bar with live data.**

- `src/components/StatusLine.tsx` — Hook-based system. Builds `StatusLineCommandInput` containing: model info, workspace, cost, context window usage (input/output tokens, % used), rate limits (5-hour/7-day), vim mode, agent info.
- Content generated by executing user's configured `statusLine` hook command.
- Rendered via `<Ansi>` component for raw ANSI output.

### What Rust does

**Nothing persistent.** Status info shown only in startup banner (`main.rs:4558-4600`) and via `/status` command (`main.rs:5986-6017`).

---

## 10. Color & Theming

### What TS does

**90+ semantic color fields, 6 themes, auto-detection.**

- `src/utils/theme.ts` — Theme type with 90+ fields: brand (claude, permission), UI chrome (promptBorder, text, subtle), semantic (success, error, warning), diff (added, removed, word-level), agent (8 colors), rainbow (7 + shimmer variants).
- 6 built-in themes: dark, light, dark-ansi, light-ansi, dark-daltonized, light-daltonized.
- Color formats: `rgb(R,G,B)`, `#RRGGBB`, `ansi256(N)`, `ansi:colorName`.
- `src/components/design-system/ThemeProvider.tsx` — React context. Auto-detection via OSC 11 background color query. Live preview in `/theme` picker.

### What Rust does

**11 hardcoded color fields.**

- `render.rs:14-45` — `ColorTheme` with 11 fields: heading (Cyan), emphasis (Magenta), strong (Yellow), inline_code (Green), link (Blue), quote (DarkGrey), table_border (DarkCyan), code_block_border (DarkGrey), spinner_active (Blue), spinner_done (Green), spinner_failed (Red).
- `render.rs:30-44` — `ColorTheme::default()` — only one theme, no switching, no detection.
- `main.rs:4577-4583` — Banner uses raw ANSI codes (`\x1b[38;5;117m`) mixed with crossterm — inconsistent styling approach.

---

## Summary: Gaps Ranked by Functional Impact

Ranked by how much they affect usability — animation/visual polish gaps excluded per decision.

| # | Gap | TS Source | Rust Source | Impact |
|---|-----|-----------|-------------|--------|
| 1 | **Tool output not collapsible** | `CtrlOToExpand.tsx:29-50` | `main.rs:8553-8585` (full dump) | **Critical** — floods screen, kills readability |
| 2 | **No fixed footer** — input/spinner scroll away during long output | `FullscreenLayout.tsx:413` (bottom slot) | `main.rs:3737` (inline loop) | **Critical** — lose context mid-turn |
| 3 | **Y/N permission prompt** — no select menu, no "allow for session", raw JSON context | `PermissionPrompt.tsx` + `CustomSelect` | `main.rs:7693-7730` (stdin readline) | **High** — friction on every permission check |
| 4 | **No status line** — model/tokens/cost not visible during session | `StatusLine.tsx` | Not implemented | **High** — users must run /status manually |
| 5 | **Spinner shows no useful info** — frozen frame, no mode, no elapsed time | `SpinnerAnimationRow.tsx` (modes + time) | `render.rs:60` (single tick) | **Medium** — user doesn't know what's happening |
| 6 | **No response structure** — no bullet prefix, no visual hierarchy between assistant text and tool output | `ToolUseLoader.tsx:19` + `MessageResponse.tsx:22` | `main.rs:7948` (raw write) | **Medium** — harder to scan conversation |
| 7 | **Plain startup banner** — no bordered layout, no tips/activity feeds | `LogoV2.tsx:331-436` (2-col layout) | `main.rs:4558-4600` (key-value) | **Low** — cosmetic, seen once |
| 8 | **Inline autocomplete** — no descriptions next to command names | `PromptInputFooterSuggestions.tsx` | `input.rs:23-99` (rustyline) | **Low** — functional, just less informative |

**Explicitly excluded** (not worth bridging):
- Shimmer/glimmer text effects
- Stall detection color interpolation
- Continuous spinner animation
- Mouse tracking / text selection
- Virtual scrolling / viewport culling
- Live theme preview

---

*Generated: 2026-04-24 | This report is for discussion — let's decide which gaps to bridge.*
