---
name: develop-checkout
description: Prepare this source checkout and run its existing isolated development checks.
---

From the checkout root, run `.agents/dependencies` with existing tools, or
`.agents/setup` on a Linux x86-64 Delta runner. Source `. .agents/env` in each
new terminal; PATH changes from setup do not persist into later terminals.

Focused source smoke: `bun run build && bun run example`.
This is a focused development check, not the full existing CI/land gate.
No browser UI is owned here; no HTTP preview or wrapper page is needed.
