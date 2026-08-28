# Grok Build `ask_user_question` source notes

Research date: 2026-08-29. The reference is the official
[`xai-org/grok-build`](https://github.com/xai-org/grok-build) repository at
commit [`bc7f02eddd3d84085849dc19ed216f11c23b0571`](https://github.com/xai-org/grok-build/commit/bc7f02eddd3d84085849dc19ed216f11c23b0571).
These notes describe observable interaction contracts; Lenso keeps its own
portable interaction Capability and rendering implementation.

## Findings

1. The question view is not a centered modal. It occupies the prompt slot at
   the bottom of the agent view. Its renderer fills the prompt surface, draws a
   left `┃` accent, then renders question chrome and option rows
   ([`question_view.rs` 1740–1818](https://github.com/xai-org/grok-build/blob/bc7f02eddd3d84085849dc19ed216f11c23b0571/crates/codegen/xai-grok-pager/src/views/question_view.rs#L1740-L1818)).
2. The non-fullscreen question body is capped at 33% of the terminal height,
   with an eight-row minimum and an 80% upper bound. Description and preview
   budgets shrink before visible option rows are sacrificed
   ([`question_view.rs` 1090–1170](https://github.com/xai-org/grok-build/blob/bc7f02eddd3d84085849dc19ed216f11c23b0571/crates/codegen/xai-grok-pager/src/views/question_view.rs#L1090-L1170)).
3. The focused option has a distinct row background. Single-select choices use
   radio markers, multi-select choices use checkboxes, and option labels and
   descriptions share the same row model
   ([`question_view.rs` 1362–1419](https://github.com/xai-org/grok-build/blob/bc7f02eddd3d84085849dc19ed216f11c23b0571/crates/codegen/xai-grok-pager/src/views/question_view.rs#L1362-L1419)).
4. Freeform input is a sticky final row, outside the scrollable options. It is
   replaced by inline editing while active, so `Other` does not disappear when
   a long option list scrolls
   ([`question_view.rs` 1820–1896](https://github.com/xai-org/grok-build/blob/bc7f02eddd3d84085849dc19ed216f11c23b0571/crates/codegen/xai-grok-pager/src/views/question_view.rs#L1820-L1896)).
5. Keyboard ownership is local to the card: Tab and Shift-Tab walk answers,
   arrows or `h`/`l` switch questions, option shortcuts select directly, `z`
   opens freeform input, and Esc parks the card instead of cancelling the turn
   ([`interactions.rs` 329–583](https://github.com/xai-org/grok-build/blob/bc7f02eddd3d84085849dc19ed216f11c23b0571/crates/codegen/xai-grok-pager/src/app/agent_view/interactions.rs#L329-L583)).
6. Submission sends an accepted extension response back to the blocked tool,
   restores the stashed composer, and destroys the question state. It does not
   append a User prompt to scrollback
   ([`interactions.rs` 1267–1325](https://github.com/xai-org/grok-build/blob/bc7f02eddd3d84085849dc19ed216f11c23b0571/crates/codegen/xai-grok-pager/src/app/agent_view/interactions.rs#L1267-L1325)).

## Lenso contract

Lenso mirrors those surface semantics while retaining
`lenso.agent.user-interaction@2` as the transport-neutral boundary. The TUI
answers the same Generation-pinned interaction; web, channel, or remote
surfaces can render their own controls without adopting terminal state. The
answer remains an `ask_user` Tool result, never a synthetic User message.
