# ADR 0104: Edit named Profiles through SQLite drafts

Status: Accepted

## Context

Agent-wide Tool policy is a permission ceiling, not a named Profile editor. Writing an active Profile file on every edit would trigger reconciliation before the user explicitly applies it.

## Decision

The Web Host's existing SQLite configuration authority owns named Profile drafts. The existing Host Profile resolver remains the authoring boundary: it selects Plugin Root instances and creates immutable resolved Plans. No product state is introduced into Kernel.

A Profile document adds excluded instances, an optional Tool allowlist, additive instructions, and an optional model default. Unknown document fields survive editing. An omitted Tool allowlist inherits caller authority; an empty list disables all Tools. Agent Loop intersects the Profile ceiling with the caller Run Scope and Plan-bound catalog. A Profile never grants permissions denied by Agent-wide policy.

Built-in and externally authored file Profiles are read-only in Console and can be duplicated. Custom drafts use revision-checked writes in SQLite. Save stages and resolves the candidate without changing its live file. Apply materializes the saved revision through a write-ahead journal and enters the existing Ready-Gated reconciliation path. An interrupted materialization rolls forward its explicit apply intent. An unexplained external file edit fails closed.

Running turns retain their Generation and Session history. A failed Ready Gate leaves the previous active Generation in service; the candidate remains desired and the saved draft is preserved. Active and saved revisions are reported independently. Saving a newer draft does not mutate an active materialization.

Console provides a Profile selector and an editor with collapsible capability groups. Sidebar remains the only page navigation. Bulk operations affect the filtered set, preserve hidden selections, and never install or remove providers. Agent-wide permissions remain a separate advanced section. Missing catalog information must not erase saved choices.

## Consequences

Editing currently requires SQLite authority. Existing file Profile selection remains compatible. Model and provider configuration remain owned by their Plugins; the Profile only selects instances and supplies existing Loop defaults or additive Prompt contributions. Credentials are never copied into a Profile document.
