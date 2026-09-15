# Member Agent implementation status

This branch adds provider-side prerequisites. It does **not** enable the Console
member Agent entry or change the running local preview.

Implemented and tested:

- Signed actor verification and durable ownership for SQLite Session operations.
- Legacy local sessions are not automatically assigned; local inspection cannot
  read owned sessions. The schema migration fences older binaries on startup.
- Private per-owner artifact storage, including known-handle negative tests.
- Private pending questions and answer routing, including matching IDs across users.
- Auth assertion retention across an immutable Host turn lease and native provider
  invocation; denied requests do not poison the running provider.
- The operator business grant cannot be captured by a Console member turn.

Still required before enabling member Agent:

1. Auth-owned member delegation from Console ingress to the receiving Agent,
   including audience, expiry, revocation and cancellation behavior. Console's
   current HTTP proxy forwards an operator credential, not this identity.
2. Per-owner admission and retrieval for running turns, SSE, cancellation and
   task state in Agent Web. A Session filter cannot authorize those endpoints.
3. Member-safe tool execution and credentials. Native filesystem/process tools,
   MCP connections and the current business connection cannot inherit operator
   authority. Projects tools must act as the signed-in Console user.
4. Frontend capability-driven Agent entry and controls, plus two-user browser
   tests through the actual Console/Auth/Agent/Projects composition.

Do not mark this branch as a complete multi-user Agent implementation. Do not
replace these remaining boundaries with a browser subject header, shared Home,
shared business grant or frontend-only filtering.
