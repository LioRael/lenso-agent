# Grok Build Message Block Gap Audit

> Research date: 2026-08-26. Grok behavior is pinned to official
> [`xai-org/grok-build` commit `77cd7eb675ba911c225c3aaeeece3a20cbccc426`](https://github.com/xai-org/grok-build/commit/77cd7eb675ba911c225c3aaeeece3a20cbccc426).
> This audit compares that first-party source with the current uncommitted Lenso
> worktree. It describes current behavior and remaining gaps; it does not propose
> copying xAI implementation code verbatim.

## Conclusion

The largest remaining mismatch is structural, not a palette mismatch.

Grok represents the transcript as typed entries. `UserPrompt`, `Thinking`,
`AgentMessage`, `ToolCall`, `System`, and session/lifecycle events each own
their content and display behavior, while a shared entry renderer applies
spacing, background, accent rail, timestamps, selection, and folding. The
official block enum makes that separation explicit
([`block.rs` 366–397](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/block.rs#L366-L397)).

Lenso currently has only `TranscriptEntry::Message { speaker, text }` and
`TranscriptEntry::Tool`, with `Speaker::{User, Agent, System, Error}`
(`apps/lenso-agent-cli/src/tui.rs` 97–103, 178–182). User and Agent messages
are rendered by one hand-written `render_message_entry` branch, while only Tool
entries participate in selection/folding/hit testing (`tui.rs` 1746–1811).
That model cannot reproduce Grok's sent-message band, received-message
streaming lifecycle, or thoughts by continuing to tune individual colors.

Most importantly, thoughts are absent before rendering. Lenso's Model Stream
has only `text_delta`, `tool_call`, and `usage`, and the Agent Turn Stream has
only `text_delta` plus Tool lifecycle/progress kinds
(`crates/lenso-capability-agent-model/src/contract.rs` 57–83;
`crates/lenso-capability-agent/src/generated.rs` 41–93). The TUI therefore has
no truthful event from which to create or update a Thinking block.

## What Grok Does After Submit

The observable flow is:

```text
UserPrompt entry
    -> pre-created running Thinking entry ("Thinking…")
    -> AgentThoughtChunk updates the same Thinking entry
    -> first AgentMessageChunk or ToolCall finishes Thinking
    -> AgentMessageChunk updates one streaming AgentMessage entry
    -> turn completion finalizes Markdown and running state
```

This is not inferred from screenshots. `AcpUpdateTracker` independently tracks
the current Agent message and Thinking entry, plus elapsed thinking time
([`tracker.rs` 328–350](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/acp/tracker.rs#L328-L350)). It dispatches
`AgentMessageChunk` and `AgentThoughtChunk` separately
([`tracker.rs` 935–1001](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/acp/tracker.rs#L935-L1001)).
It can pre-create an empty running Thinking entry so feedback appears before
the first reasoning token, removes that entry if no thought ever arrived, and
finishes it with elapsed time otherwise
([`tracker.rs` 1059–1092](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/acp/tracker.rs#L1059-L1092)). The first Agent text chunk closes Thinking and starts or updates one streaming Agent entry; a Tool call also closes Thinking before creating its Tool block
([`tracker.rs` 1113–1197](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/acp/tracker.rs#L1113-L1197)).

Lenso currently appends a User `Message`, opens the Agent Stream, and changes
the UI phase to Active (`tui.rs` 1300–1364). Every `text_delta` is concatenated
into the last Agent string (`tui.rs` 462–475, 1367–1403). There is no separate
running response entry, no block completion callback, and no Thinking entry.

## Sent User Message

### Official Grok behavior

- The prompt is a dedicated block with variants for normal, bash, cron,
  interjection, and recognized skill-token ranges. Slash/skill tokens receive
  their own semantic color
  ([`user.rs` 81–193](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/user.rs#L81-L193)).
- It paints a full-width elevated background band. The prefix and body have
  distinct styles, and the selected state changes the band treatment
  ([`user.rs` 197–267](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/user.rs#L197-L267)).
- Prefix selection is semantic: normal uses the prompt arrow, bash uses `$`,
  and cron uses its own mark. Continuation rows align under the body, and word
  wrapping is calculated after prefix width is reserved
  ([`user.rs` 269–377](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/user.rs#L269-L377)).
- Long prompts default to a folded preview of at most three visual lines and
  can expand in place
  ([`user.rs` 461–534](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/user.rs#L461-L534)).
- User and Agent message timestamps are optionally overlaid at the right; the
  renderer reserves width before wrapping, and hover expands the timestamp
  from a short to full format
  ([`entry_renderer.rs` 361–383](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/wrappers/entry_renderer.rs#L361-L383),
  [`entry_renderer.rs` 877–906](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/wrappers/entry_renderer.rs#L877-L906)).

### Current Lenso gap

Lenso's sent message now has a `❯ ` prefix and a full-row RGB background, but
`surface_line` pads only the current rendered line width (`tui.rs` 1783–1795,
1997–2013). It does not own a block identity, selected state, display mode,
creation time, recognized command-token ranges, or an exact wrap model. A long
prompt therefore cannot fold like Grok, and wrapped continuation rows are an
effect of the outer `Paragraph` rather than prompt-aware layout. Prompt anchors
support sticky/page-flip behavior, but they are not interactive message blocks.

## Received Agent Message

### Official Grok behavior

`AgentMessageBlock` owns a `MarkdownContent` instance. A new live response starts
empty, chunks are pushed into that same renderer, and `finish()` performs a full
correctness render plus final image/video/Mermaid discovery
([`agent.rs` 10–84](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/agent.rs#L10-L84)). The final Agent block deliberately has no accent rail, no extra vertical padding, and is not foldable; the response reads as quiet Markdown rather than a chat bubble
([`agent.rs` 170–231](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/agent.rs#L170-L231)).

The shared `MarkdownContent` preserves source and rendered views, supports raw
mode, exposes hyperlinks, and caches word-wrapped styled lines
([`markdown_content.rs` 45–68](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/markdown_content.rs#L45-L68),
[`markdown_content.rs` 175–249](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/markdown_content.rs#L175-L249)). It incrementally re-wraps only the unfrozen tail while streaming instead of rebuilding the full response on every chunk
([`markdown_content.rs` 298–389](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/markdown_content.rs#L298-L389)).

### Current Lenso gap

Lenso stores raw response text in a `String`, then calls the small local
`markdown::lines` parser over the complete string during transcript rendering
(`tui.rs` 462–475, 1797–1801; `tui/markdown.rs` 8–118). That parser handles a
limited set of headings, bullets, quotes, fences, inline code, and bold, but it
does not retain incremental Markdown state, source maps, hyperlinks, tables,
syntax-aware code rendering, media, raw/pretty mode, or finalization. Styling
every returned line with `SECONDARY_TEXT` at the outer message branch can also
flatten the hierarchy that the Markdown spans attempt to establish.

This explains why an accepted response still looks unlike Grok even when its
base foreground color is close: Grok renders a semantic Markdown document in a
typed entry; Lenso re-parses a plain string into a short list of line patterns.

## Thoughts / Thinking

### Official Grok behavior

Thinking is a first-class streaming Markdown block with three display modes:

- running/truncated is the default and shows the recent reasoning tail;
- collapsed shows `Thinking…`, `Thought`, or `Thought for Xs`;
- expanded shows the full reasoning document.

The block records both server elapsed time and a local live timer, and finalizes
the streaming Markdown on completion
([`thinking.rs` 73–177](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/thinking.rs#L73-L177)). Its collapsed label and duration styling are independent from the body
([`thinking.rs` 220–265](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/thinking.rs#L220-L265)). Truncated mode renders an ellipsis plus the last configured number of wrapped Markdown lines, while expanded mode renders all content
([`thinking.rs` 310–418](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/thinking.rs#L310-L418)).

While running, folding cycles between truncated and expanded so current
reasoning never becomes a misleading inert one-line completion. Once finished,
the block auto-collapses; it remains selectable/groupable and can be expanded
again. Running accent and bullet animation are separate from completed gray
presentation
([`thinking.rs` 428–529](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/blocks/thinking.rs#L428-L529)).

### Current Lenso gap

There is no Thinking variant in `TranscriptEntry`, no Thinking display state,
and no `RunTurnResponseKind` for a thought start/delta/completion. The gap begins
one layer earlier: source-first Agent Model Capability v1.1 has no reasoning
message kind (`crates/lenso-capability-agent-model/src/contract.rs` 57–83).
The Agent Loop can therefore forward only Model text, Tool calls, and usage
(`crates/lenso-agent-loop-module/src/lib.rs` 673–735).

The Codex Direct Module already requests `reasoning.summary = "auto"`, but its
SSE decoder only translates `response.output_text.delta`, function-call
completion, and terminal usage (`crates/lenso-agent-model-openai-codex-direct-module/src/lib.rs`
197–208, 515–575). Thus even a Provider response that contains a displayable
reasoning summary is not represented in the portable Model Stream and cannot
reach the Agent or TUI.

The future contract should explicitly carry only Provider-designated,
display-safe reasoning summary/progress. The TUI should not infer thoughts from
ordinary Agent text or treat hidden chain-of-thought as a UI payload.

## Shared Entry Chrome and Interaction

Grok's shared `EntryRenderer` owns a stable accent/padding/content/right-padding
layout, background filling across the whole visible entry, completion flash,
and timestamp reservation
([`entry_renderer.rs` 656–769](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/wrappers/entry_renderer.rs#L656-L769)). It applies cached block output, per-line semantic backgrounds, timestamp overlays, and running/collapsed bullet treatment in one pass
([`entry_renderer.rs` 805–966](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/wrappers/entry_renderer.rs#L805-L966)).

Each scrollback entry carries an ID, running/pending state, display mode,
created time, finished time, and render caches
([`entry.rs` 72–138](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/entry.rs#L72-L138)). The visible output publishes selection metadata and OSC 8 link overlays
([`selection.rs` 52–90](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/scrollback/selection.rs#L52-L90)). Mouse release can open a link, select text, or select an entry
([`mouse.rs` 778–901](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/mouse.rs#L778-L901)); single click selects, double click folds, and prompt double-click also scrolls it to the top
([`selection.rs` 935–1018](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/selection.rs#L935-L1018),
[`selection.rs` 1077–1103](https://github.com/xai-org/grok-build/blob/77cd7eb675ba911c225c3aaeeece3a20cbccc426/crates/codegen/xai-grok-pager/src/app/agent_view/selection.rs#L1077-L1103)).

Lenso's current `selected_block`, hit targets, and fold operations apply only to
Tool entries (`tui.rs` 377–405, 801–815, 1813–1937). User and Agent messages
have no IDs or hit targets, so they cannot be selected, folded, copied as a
block, opened in raw mode, or used as hyperlink targets. There is also no
shared renderer guaranteeing that all block types share the same chrome and
spacing rules. This is why local fixes to a User row often create new spacing
or alignment discrepancies beside Agent and Tool rows.

## Gap Matrix

| Area | Current Lenso | Official Grok source behavior | Remaining work |
|---|---|---|---|
| Transcript model | `Message(Speaker, String)` or `ToolCard` | Typed block enum plus per-entry state | Replace speaker switch with typed entries and stable IDs |
| Sent message | Full-row band and `❯`, no state | Semantic prefix, skill ranges, selection, timestamp, three-line folding | Dedicated `UserPrompt` block and prompt-aware wrapping |
| Received message | Full-string lightweight Markdown parse | Incremental Markdown state, finalization, links/media/raw view | Dedicated streaming `AgentMessage` block |
| Thoughts | Not represented | Pre-created running block, streamed content, elapsed duration, auto-collapse | Evolve Model and Agent stream contracts, then add `Thinking` block |
| Shared chrome | Hand-built per branch | One renderer owns padding, background, accents, timestamp, completion flash | Introduce entry renderer before further pixel tuning |
| Interaction | Tool cards and global scroll controls only | All selectable blocks, text ranges, links, fold gestures, block copy | Generalize hit testing and selection beyond Tools |
| Streaming cost | Rebuild rendered transcript from accumulated strings | Incremental Markdown and wrap caches, final correctness pass | Cache block render output and invalidate only the changing entry |

## Recommended Implementation Order

1. **Evolve the source-first stream contracts.** Add a display-safe reasoning
   summary/progress kind to the Agent Model Stream, forward it through the Agent
   Loop, and add corresponding Agent Turn Stream events. Since both enums are
   closed, follow the existing Capability versioning rules rather than editing
   generated Rust or Schemas by hand.
2. **Replace `Speaker + String` with typed transcript entries.** Start with
   `UserPrompt`, `Thinking`, `AgentMessage`, `ToolCall`, `SystemEvent`, and
   `ErrorEvent`. Give every entry an ID, running state, display mode,
   `created_at`, optional `finished_at`, and render-cache generation.
3. **Build one shared entry renderer.** Centralize horizontal chrome, vertical
   gap/padding, background fill, accent/bullet animation, timestamp reservation,
   selection treatment, and hit-test metadata. Do this before more color and
   spacing adjustments.
4. **Implement the turn transition state machine.** Submit immediately creates
   the User block and a provisional running Thinking block. Thought deltas update
   it in place; first Agent text or Tool activity closes it; terminal completion
   finalizes the active Markdown block. Remove provisional Thinking if it stayed
   empty.
5. **Implement block-specific renderers.** Match User wrapping/folding and
   Agent/Thinking Markdown behavior, including finalization and elapsed time.
   Preserve the current Tool progress work by moving Tool cards into the same
   entry model rather than rewriting their execution contract again.
6. **Generalize interaction.** Publish message/text/link hit targets, then add
   selection, copy, raw/pretty toggle, click-to-open links, and double-click
   folding. Keep terminal surface behavior inside the removable TUI Shell.
7. **Lock visual behavior with buffer snapshots.** Cover one submitted prompt,
   running empty Thinking, streamed Thinking, completed `Thought for Xs`,
   streaming Agent Markdown, long folded prompt, timestamps on/off, hover,
   narrow width, and mixed Thinking/Tool/Agent sequences.

This order keeps the accepted Lenso boundary intact: Model Providers own
provider protocol and displayable reasoning extraction, the Agent Loop owns
Turn coordination and portable event forwarding, and the TUI Shell owns
terminal presentation and interaction. No terminal concept or Agent-specific
registry needs to enter Kernel.
