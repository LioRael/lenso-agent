# ADR 0079: Update official Prompts with the Host

## Status

Accepted.

## Context

ADR 0056 installed the complete official coding and planning Prompt bytes as
ordinary Plugin Instance configuration in Agent Home. Exact-file preservation
protected user changes, but it also froze an unmodified official Prompt until
the user manually replaced the file. Updating the Harness executable therefore
did not give new Sessions the improved official instruction shipped by that
version.

The opposite policy, blindly overwriting visible Plugin configuration on Host
startup, would erase intentional App differences. Reassembling the instruction
on every Turn would also violate ADR 0043: a Session must retain the exact
System Instruction installed when it was created.

## Decision

The Host Catalog owns the current configuration defaults for the three
official optional Prompt Instances selected by `code`, `code-sandbox`, and
`plan`. Their visible files in Agent Home are empty enabling entries. The
ordinary configuration overlay rules remain authoritative:

- an empty official Instance uses the Prompt bytes shipped in the current Host
  binary;
- explicit local contribution content overrides those defaults and opts that
  Instance out of automatic official Prompt updates; and
- deleting the Instance or selecting another Profile removes the contribution.

Each Harness release may update the official contribution content and version
in the immutable generated Host Catalog. After the binary is updated, a new
App Generation resolves those bytes and every newly created Session installs
the new System Instruction. Existing Sessions retain their previously
installed instruction exactly as required by ADR 0043.

When an official named Profile is first selected, the Host recognizes the
byte-exact Prompt files written by the pre-0064 installer and atomically
replaces only those files with empty enabling entries before Plugin Root
snapshotting. Unknown content, non-regular files, and symlinks remain untouched
as local customization. The explicit Profile installer performs the same
migration before its normal exact-content preflight.

This migration is intentionally narrow. Other Profile and Plugin configuration
continues to be visible App-owned state and is not silently rewritten by a Host
upgrade.

## Consequences

- an ordinary Harness binary update improves the official System Instruction
  for new Sessions without another install command;
- the current Prompt bytes and contribution versions remain inspectable in the
  generated Host Catalog and durable Session provenance;
- local Prompt customization remains possible through the existing Plugin
  configuration overlay instead of a second preference mechanism;
- official `code` and `code-sandbox` share one byte-identical coding core while
  retaining distinct execution contributions; and
- updating a running binary does not mutate the instruction of an existing
  Session or introduce per-Turn Prompt mutation.

## Proof

Tests must prove that empty official Prompt Instance files resolve the current
Host-owned contributions, coding and sandbox modes share the same coding core,
legacy exact official files migrate automatically, customized files remain
unchanged, unrelated Profiles do not trigger migration, all three official
Profiles still resolve, and resumed Sessions retain their installed System
Instruction.
