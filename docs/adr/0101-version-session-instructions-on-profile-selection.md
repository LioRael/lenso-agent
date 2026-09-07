# ADR-0101: Version Session instructions on Profile selection

Status: Accepted

## Context

ADR-0043 freezes the Session instruction, while ADR-0084 changes the next Turn's
Generation and Tool authority on Profile selection. A Session started in Plan
therefore keeps its read-only instruction after its Tools switch to Code.

## Decision

Preserve the Session and its original installed instruction. The Host attaches
the selected authoring Profile to each immutable Turn lease, separately from
the model-selection profile. The Loop records that identity with the instruction.
Resuming with the same Profile retains the exact recorded bytes even after a
Host upgrade, configuration publication, or restart. A different Profile
assembles and appends a new `system_instruction_revised` event before the Turn.
The event contains the complete instruction, its Profile, and the previous
instruction digest. Readers validate the chain and use its latest version.
The original install remains unique and is never overwritten.

Legacy records lack a Profile. Reviewed official Plan, Code and sandbox Prompt
contribution IDs identify those historical modes. Other legacy instructions are
bound to the current Profile without changing their bytes on first resume.
This binding is explicitly recorded; it does not guess custom Profile names.
Subsequent mode changes follow the same version rule. Invalid or broken chains
fail closed. An interrupted Turn may leave a committed instruction revision;
resuming uses that version and does not append it again.

The Session Capability adds an event kind, and new instruction records carry
Profile metadata. Old readers may reject these records; binary downgrade
requires a pre-upgrade backup.
This supersedes ADR-0043's exactly-one-active-instruction rule only for explicit
Profile identity changes and legacy identity binding. Prompt edits within the
same Profile still apply only to new Sessions.

## Scope and proof

No Kernel policy, automatic transport replay, or implicit Tool grant is added.
Tests cover Plan/Code/Plan in one Session, restart, same-Profile freeze, legacy
binding, instruction-chain rejection, and alignment of instruction content and
model request provenance. Coding preset admission separately checks the current
Host inventory before it advertises or imports the official preset.
