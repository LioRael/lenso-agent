# Grok Build TUI Official Source Notes

> Research date: 2026-08-26. This document uses only official public xAI/SpaceXAI repositories and documentation. It does not infer implementation details from screenshots or memory.

## Conclusion

The complete Rust source for the Grok Build TUI is public, so it can be reproduced directly from source without reverse-engineering an npm package or source maps. The official repository is [`xai-org/grok-build`](https://github.com/xai-org/grok-build). Its README explicitly states that the repository contains the Rust source for the `grok` CLI/TUI and agent runtime and is periodically synchronized from the internal monorepo. The main TUI crate is `xai-grok-pager`, and the foundational rendering layer is `xai-grok-pager-render` ([README 31–35](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/README.md#L31-L35), [README 95–105](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/README.md#L95-L105)).

These notes pin the following source snapshot. Subsequent implementation work should not reference a moving `main` branch:

- GitHub commit: [`77cd7eb675ba911c225c3aaeeece3a20cbccc426`](https://github.com/xai-org/grok-build/commit/77cd7eb675ba911c225c3aaeeece3a20cbccc426), commit title `Synced from monorepo`, dated 2026-08-25 21:15:49 UTC.
- Corresponding internal monorepo revision: [`SOURCE_REV`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/SOURCE_REV) = `28439e8a8712c363321cf6ff0c2d70cd058d2a7d`.
- `xai-grok-pager` crate version: `1.0.10` ([`Cargo.toml` 1–6](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/Cargo.toml#L1-L6)).
- At the time of research, the official repository had no Git tag that precisely maps `1.0.10` to a released binary. These notes therefore establish the behavior of this public synchronized snapshot; they do not claim byte-for-byte identity with any locally installed `grok` binary.
- The official reference screenshot is at [README 27](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/README.md#L27), and the official online documentation entry point is at [README 85–93](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/README.md#L85-L93).

First-party code is Apache-2.0 ([README 127–139](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/README.md#L127-L139)). Direct ports or substantial excerpts must retain the applicable license and NOTICE requirements; this document extracts only structure and behavior.

## Source Map

| Concern | Official source |
|---|---|
| Overall state and root input/rendering | [`src/app/app_view.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/app_view.rs#L1-L5) |
| Main-screen vertical layout | [`src/views/agent.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/agent.rs#L80-L179) |
| Main-screen render orchestration | [`src/app/agent_view/render.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/render.rs#L1365-L1426) |
| Conversation block model | [`src/scrollback/block.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/block.rs#L366-L397) |
| User/Agent conversation rows | [`blocks/user.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/user.rs#L197-L243), [`blocks/agent.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/agent.rs#L19-L58) |
| Tool block categories and Execute | [`blocks/tool/mod.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/mod.rs#L154-L187), [`tool/execute.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/execute.rs#L654-L770) |
| Composer | [`src/views/prompt_widget/mod.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/prompt_widget/mod.rs#L3046-L3189) |
| Header right-side status | [`src/views/agent_status.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/agent_status.rs#L41-L135) |
| Turn status | [`src/views/turn_status.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/turn_status.rs#L1-L13) |
| Bottom shortcut bar | [`src/views/shortcuts_bar.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/shortcuts_bar.rs#L211-L321) |
| Keyboard navigation | [`docs/user-guide/03-keyboard-shortcuts.md`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/docs/user-guide/03-keyboard-shortcuts.md#L27-L100) |
| Mouse-scroll normalization | [`src/input/mouse.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/input/mouse.rs#L1-L19) |
| GrokNight colors | [`xai-grok-pager-render/src/theme/groknight.rs`](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager-render/src/theme/groknight.rs#L16-L149) |

## 1. The Overall Layout Is Not Three Boxes for “Header + Messages + Input”

The root view stacks the screen in the following order. Only scrollback uses `Constraint::Min(5)` to absorb the remaining height; everything else is a fixed-height row shown when needed ([`agent.rs` 230–297](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/agent.rs#L230-L297)):

```text
top outer padding
status bar: 1 row
[tasks / catalog / todo]
gap
scrollback: min 5 rows, consumes remainder
[/btw / queue / turn status / banner / plugin CTA / follow-ups]
prompt gap
[voice recording]
composer
[custom status line]
shortcuts bar: 1 row
bottom outer padding
```

Default outer padding is one row vertically and two columns on each side, with two columns of inner padding on each side of a block ([`appearance/config.rs` 196–220](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/appearance/config.rs#L196-L220)). Compact mode is automatic at 20 rows or fewer. At 16 rows or fewer, optional rows such as CTA and follow-ups are hidden, while scrollback retains at least five rows ([`agent.rs` 80–100](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/agent.rs#L80-L100)). The composer is capped at half the terminal height instead of pushing away the main content without limit ([`render.rs` 1122–1130](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/render.rs#L1122-L1130)).

When a scrollbar or timeline rail is enabled, the actual scrollback text area narrows so text is not rendered beneath it ([`agent.rs` 399–419](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/agent.rs#L399-L419)). This is crucial to the current Lenso TUI scrolling fix: the scrollable region must own an independent viewport height and content width rather than clipping strings against the entire screen.

## 2. Header / Status Bar

An active session has no permanent oversized `Grok Build` logo at the top. The source header is one row of environment and runtime context:

- The left side shows, in order, the Git branch (or `detached`), a `worktree` marker, optional `sandbox:<profile>`, and the cwd with home abbreviated to `~` ([`render.rs` 1620–1685](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/render.rs#L1620-L1685)).
- The cwd reserves room for and is truncated against right-side status; clicking it copies the complete path ([`render.rs` 1686–1716](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/render.rs#L1686-L1716), [`mouse.rs` 252–255](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/mouse.rs#L252-L255)).
- The right side contains dynamic background-task, plan, goal, MCP-initialization, workspace-mode, and context-usage items. `AgentStatusBar` right-aligns them and separates them with ` │ ` ([`render.rs` 1525–1619](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/render.rs#L1525-L1619), [`agent_status.rs` 41–135](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/agent_status.rs#L41-L135)).

The reproduced top area should therefore be a low-contrast context row, not a bordered application title bar.

## 3. Conversation Rows

Scrollback is not a `Vec<String>`; it is a collection of typed blocks with independent measurement, folding, selection, and streaming-update semantics. The official enum includes at least UserPrompt, AgentMessage, ToolCall, Thinking, System, SessionEvent, BgTask, Subagent, Workflow, Btw, ContextInfo, and CreditLimit ([`block.rs` 366–397](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/block.rs#L366-L397)).

Concrete visual rules:

- A user message uses a full-row background band. A regular prompt has an arrow prefix, bash uses `$`, and cron uses another arrow ([`user.rs` 197–243](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/user.rs#L197-L243), [`user.rs` 246–457](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/user.rs#L246-L457)). A folded long-user-message preview is at most three rows and hides its prefix/accent ([`user.rs` 461–518](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/user.rs#L461-L518)).
- An Agent message owns incremental Markdown state: it pushes chunks as they arrive, then performs complete Markdown/media/Mermaid finalization at the end instead of rebuilding the entire transcript for every token ([`agent.rs` 19–58](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/agent.rs#L19-L58)). Agent messages have no background, extra accent, or vertical padding and cannot be folded ([`agent.rs` 170–231](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/agent.rs#L170-L231)).
- A shared renderer ultimately adds bullets/accents, selection state, fold mode, and related presentation to each block's output; each message type does not hand-build the complete row itself ([`block.rs` 462–482](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/block.rs#L462-L482)).

## 4. Tool Cards and Streaming Command Output

Grok tool cards are not generic rectangular boxes. They are divided into Execute, Read, Edit, ListDir, Search, WebFetch, WebSearch, IntegrationSearch, UseTool, MemorySearch, Skill, Other, and Lifecycle ([`tool/mod.rs` 154–187](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/mod.rs#L154-L187)), and normalized into verb groups such as `Read/Reading files`, `Search`, `Listed`, `Fetched`, `Ran`, `Edited`, and `Called` ([`tool/mod.rs` 79–152](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/mod.rs#L79-L152)). The visual emphasis is “a one-line semantic title plus expandable content,” not a four-sided border around every tool call.

Key Execute-block state and behavior:

- It owns command, description, output, error, start time, elapsed time, and bash mode. Streaming stdout is appended through `push_output` ([`execute.rs` 1–82](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/execute.rs#L1-L82)).
- Its header is `$ command` or `Run <description>`. With a description, collapsed mode shows only the title and reveals the command only when expanded ([`execute.rs` 132–185](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/execute.rs#L132-L185), [`execute.rs` 291–410](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/execute.rs#L291-L410)).
- Stdout is parsed as ANSI terminal output and wrapped before head-and-tail truncation, with `… +N lines` in the middle. Output uses a darker surface background ([`execute.rs` 513–627](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/execute.rs#L513-L627)).
- Error/running/success accents are red, animated, and green, respectively ([`execute.rs` 695–708](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/execute.rs#L695-L708)).
- Agent-initiated tools are collapsed by default and do not auto-expand at start or completion. A user `!` bash invocation is truncated and shown live while running, then expanded on completion ([`execute.rs` 731–769](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/execute.rs#L731-L769)).

This directly determines the minimum information Lenso's streaming Tool progress contract must carry: stable call id, tool kind, title/description, lifecycle, incremental output chunks, terminal state/error, and elapsed time, while allowing the UI to update the same block in place. If the UI can receive only a “final tool result,” it cannot reproduce the running accent, live stdout, head-and-tail truncation, or preservation of the user's fold selection after completion.

## 5. Composer

The composer is a multiline input panel with rounded box-drawing borders rather than a single underlined input: `╭────╮` at the top, `│` on the sides, and `╰──── model · flags ────╯` at the bottom. Its first content row uses `❯ `, search uses `? `, and bash can override it with `! ` ([`prompt_widget/mod.rs` 3116–3189](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/prompt_widget/mod.rs#L3116-L3189), [`prompt_widget/mod.rs` 3367–3416](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/prompt_widget/mod.rs#L3367-L3416)).

- Empty and unfocused, its placeholder is `Build anything` ([`prompt_widget/mod.rs` 3349–3365](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/prompt_widget/mod.rs#L3349-L3365)).
- The bottom border embeds `model_name · flag1 · flag2` on the left and may show multiline on the right. A border caption can show the session title right-aligned in the top border ([`prompt_widget/mod.rs` 3136–3155](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/prompt_widget/mod.rs#L3136-L3155), [`prompt_widget/mod.rs` 3528–3608](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/prompt_widget/mod.rs#L3528-L3608)).
- When unfocused, foreground, border, and info line blend toward the background rather than merely changing prefix color ([`prompt_widget/mod.rs` 3418–3467](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/prompt_widget/mod.rs#L3418-L3467)).
- Slash/file/completion dropdowns are drawn above the composer ([`render.rs` 3086–3230](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/render.rs#L3086-L3230)).

## 6. Footer and Runtime Status

A one-row turn status may appear above the composer: spinner/activity/phase timer on the left and turn timer, tokens, and `[stop]` on the right. It is hidden when idle ([`turn_status.rs` 1–13](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/turn_status.rs#L1-L13)). It is not a permanent footer.

Below the composer comes an optional custom status line, followed by a one-row shortcuts bar (layout at [`agent.rs` 386–398](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/agent.rs#L386-L398)). Shortcuts change with the current pane, overlay, or confirmation state. Keys use an emphasized style, descriptions are gray, entries are separated by `  │  `, and content is clipped when width is insufficient ([`shortcuts_bar.rs` 211–321](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/views/shortcuts_bar.rs#L211-L321)). Static help copy should therefore not be hard-coded into an invariant footer.

## 7. Scrolling, Keyboard, and Mouse

### Keyboard

The official implementation provides both simple and vim keymaps ([`keyboard-shortcuts.md` 1–23](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/docs/user-guide/03-keyboard-shortcuts.md#L1-L23)):

- Simple: arrow keys move by row; `Shift+↑/↓` moves between turns.
- Vim: `j/k` moves by row, `H/L` by turn, `J/K` by response, and `g/G` to top/bottom.
- Shared: `Ctrl-K/J` moves by row, `PageUp/PageDown` by page, and `Ctrl-U/D` by half-page. PageUp/PageDown still scroll the transcript while the prompt is focused unless a dropdown takes ownership ([`keyboard-shortcuts.md` 27–51](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/docs/user-guide/03-keyboard-shortcuts.md#L27-L51)).
- `h/l/e/E/Ctrl-E` controls folding. `Tab` or `Space` switches focus between scrollback and prompt; Esc itself is not a focus toggle ([`keyboard-shortcuts.md` 55–100](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/docs/user-guide/03-keyboard-shortcuts.md#L55-L100)).
- With scrollback focused in simple mode, typing a letter automatically returns focus to the prompt. Enter can open a link, inline edit, or subagent ([`panes.rs` 11–129](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/panes.rs#L11-L129)).

### Follow and Viewport

Scroll state includes real `scroll_offset`, `viewport_height`, `total_height`, and follow mode. `goto_top` disables follow and `goto_bottom` enables it; half-page movement uses half the viewport height ([`nav.rs` 517–576](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/state/nav.rs#L517-L576)).

A new turn supports a “page flip”: it first pins the user prompt to the top of the viewport so subsequent content grows below it, then repins to the bottom only after content overflows ([`nav.rs` 598–620](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/state/nav.rs#L598-L620), [`nav.rs` 1191–1214](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/state/nav.rs#L1191-L1214)). Follow mode pins to the tail during each render without crudely overwriting a user's selection on a middle block ([`nav.rs` 1146–1181](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/state/nav.rs#L1146-L1181)).

After manually leaving the bottom while content remains below, a clickable `▼` appears between scrollback and the composer. When an earlier response exists above, the sticky area may show `▲` ([`render.rs` 1973–2029](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/render.rs#L1973-L2029), [`mouse.rs` 257–266](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/mouse.rs#L257-L266)).

### Mouse / Trackpad

A wheel event is not naively treated as one row. Source groups events into streams separated by an 80 ms gap and redraws at a 16 ms cadence. One wheel notch defaults to three rows, with sub-row accumulation and speed-band acceleration for trackpads ([`input/mouse.rs` 1–19](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/input/mouse.rs#L1-L19), [`input/mouse.rs` 53–98](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/input/mouse.rs#L53-L98)). Each pane routes wheel input independently; scrollback, prompt textarea, dropdowns, and panels do not share one scroll target ([`panes.rs` 495–687](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/panes.rs#L495-L687)).

The right scrollbar supports click and drag, using the inverse of the thumb renderer's mapping to translate screen y precisely into top, bottom, or offset ([`mouse.rs` 1200–1235](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/mouse.rs#L1200-L1235)).

## 8. Theme and Visual Density

The default GrokNight theme is not pure black: base background `#141414`, light surface `#242424`, dark surface `#1c1c1c`, and primary/secondary text `#e1e1e1`/`#c8c8c8`. Thinking/running uses a magenta-leaning accent, success green, error red, skill blue, and plan gold; normal/active prompt borders are `#323237`/`#505058` (all tokens at [`groknight.rs` 16–149](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager-render/src/theme/groknight.rs#L16-L149)).

Reproduction should begin with these semantic tokens and then assign them to blocks, not with many unrelated hard-coded RGB values. Grok's density comes mainly from a borderless transcript, low-contrast context, surfaces used only for the user band/command output/composer, and accents rather than whole-block highlights for state changes.

## 9. Direct Implementation Order for Lenso's Next Layer

In descending order of impact on visual and behavioral fidelity:

1. Turn the main area into an independently measured scrollback viewport that owns `total_height/viewport_height/scroll_offset/follow_mode`; implement PageUp/Down, wheel input, top/bottom navigation, and resize clamping.
2. Promote the transcript from uniform text rows to typed blocks, beginning with at least User, Agent, and Execute Tool, each with cached wrapped height.
3. Update a single Execute block in place by stable call id for Tool progress, incrementally appending ANSI-aware stdout; implement running/success/error accents and collapsed/truncated/expanded modes.
4. Rebuild header, turn status, composer, and shortcuts according to the official vertical stack instead of continuing to tune colors on the existing three large panels.
5. Implement follow/page-flip: top-align the user prompt after sending, follow the tail after content fills the viewport, disable follow when the user scrolls manually, and show `▼` to return to the bottom.
6. Finally add sticky response navigation, scrollbar dragging, slash/file dropdowns, selection/copy, and other advanced details.

This order fixes structural gaps first. Adjusting only borders, icons, and copy may make a local screenshot look similar, but it cannot reproduce Grok Build's live output, scrolling feel, or information density.
