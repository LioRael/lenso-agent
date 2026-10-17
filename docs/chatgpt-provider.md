# ChatGPT provider extraction and Studio migration

## Baseline and ownership

This candidate starts at Lenso Agent `3e6aaa785480910ee35ad8a9cfda386527c063d0`,
the exact official commit pinned in Studio's `vendor/agent/README.md`.
The authorized local Agent repository is
`/Users/leosouthey/Projects/framework/lenso-agent`, remote
`https://github.com/LioRael/lenso-agent`, clean at `df397455bec2a8ea5ef224542d46f4b53e97fc70`
when inspected. Its cached `origin/main` matched that older commit; this was not
treated as proof of current remote main. The newer exact commit was fetched
into this isolated checkout, without updating the local source repository.

Provider implementation and initial offline tests were extracted from Studio
`581b6a9df9cacd5532a026f6a11c0c7b92b80ac0` using Git blobs, not mutable working files.
Only the source provider/tests were extracted. No Studio working file or service
was changed; no credential, `.env`, database or user profile was copied.
The Studio owner's subsequent uncommitted `provider.secrets()` change was read
separately and preserved: verified access tokens enter a private Set, exposed
only through a detached server-side snapshot for dynamic redaction. No applicable AGENTS.md was present in Agent or its parent paths.
Agent's only repository skill is `.agents/skills/land`; it applies only to an
explicit landing request, and was not invoked. Local application authoring
skills do not govern this optional Agent package entry.

## Small interface boundary

| Reusable Agent module | Host / Studio responsibility |
| --- | --- |
| `createChatGPTProvider({owner, registrationId, modelId, getAccess, toolNamespace?, fetch?})` | Authenticate owner and select account/model explicitly |
| `ChatGPTAuthAdapter.getAccess(owner, {registrationId})` contract | Implement current authorization, identity/scope checks and refresh |
| Responses endpoint, payload restrictions and completion handling | OAuth listener, browser flow, JWT validation, private storage and locks |
| Explicit token passed through Pi to guarded transport | Stop old executors on switch, reauthorization and disconnect |
| Namespace formatting for host-selected tools | Creative context, prompts, business permissions, UI and human adoption |

There is no new provider registry, account manager, generic OAuth framework,
database migration, or changed Agent core API. The optional entry is not
re-exported by the default entry. `fetch` is a trusted injection seam for tests;
it is not a browser-configurable destination. The payload helper is not a
permission check; the caller must use Agent's existing tool gateway.

## Current official protocol check

Reviewed live official documentation on 2026-10-17 UTC. The
[overview](https://developers.openai.com/siwc/token-sharing-open-source)
documents open-source and locally hosted apps; paid or remotely hosted offerings
have a separate interest path. The stated local personal, noncharging deployment
fits the documented local flow; this does not establish eligibility for future
distribution models. Host identifiers persist across sign-ins and contain no PII.

[Registration](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)
uses browser consent, PKCE, state, nonce, loopback callback and the issued client
ID. The host must keep identity authentication separate from permission to use
the plan. [Token reference](https://developers.openai.com/siwc/token-sharing-open-source/token-reference)
specifies `https://api.openai.com/v1` as audience and replacement refresh tokens.
Those validation and lifecycle rules remain the existing host adapter's job.

[Models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)
requires the selected account's catalog, explicit OAuth bearer access and public
`/v1/responses`; success needs `response.completed`. The provider rejects incomplete
streams and retains Pi's failed/interrupted handling. It disables automatic HTTP
retries and redirects. It does not use ChatGPT backend endpoints or ambient keys.

[Preview restrictions](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)
require stateless streamed requests, full context, developer messages and namespaced
function/custom tools. Unsupported fields are removed; hosted tool types are
rejected by this adapter. This extraction preserves the Studio behavior rather
than adding broader input or tool support.

## Studio migration, after its inference verification stabilizes

1. Keep the currently running Studio and existing vendor package unchanged.
   Review this candidate and rebuild/package it locally. Do not install registry
   `@lenso/agent@latest`: that can identify the unrelated native CLI distribution.
2. In a separate Studio candidate, update its vendor archive, recorded commit/hash
   and lockfile together. Preserve all existing Agent exports and runtime behavior.
3. Replace only the local provider body with a thin compatibility wrapper:

   ```ts
   import { createChatGPTProvider as createAgentChatGPTProvider,
     type ChatGPTProviderOptions } from "@lenso/agent/chatgpt";
   import type { AssistantProvider } from "./assistant";

   export function createChatGPTProvider(options: ChatGPTProviderOptions): AssistantProvider {
     return { ...createAgentChatGPTProvider({ ...options, toolNamespace: "studio" }),
       label: `ChatGPT plan · ${options.modelId}`, testOnly: false };
   }
   ```

4. Keep `chatgpt-connections.ts`, `profile-lock.ts`, `chatgpt-assistant.ts`, author
   isolation, lifecycle cancellation, and draft/adoption code owned by Studio.
   Existing `getAccess(owner, {registrationId})` is structurally compatible;
   no persisted credential or database migration is needed.
5. Run Studio's focused provider/connection tests and isolated real PostgreSQL
   assistant test against the new package. Its namespace must still be `studio`.
   Reconcile any provider fixes made by the Studio owner after `581b6a9` before
   integration. This task neither modifies nor claims to validate those later fixes.
6. Switch the running process only as a separate deliberate host action. Keep
   the prior archive/lockfile revision as rollback; do not copy or reset user data.

This task uses synthetic fixture strings and injected HTTP/SSE only. It performs
no real authorization, token issuance, refresh, model call or service restart.
The fixture suite covers completion/failure/disconnect/cancellation, no retries,
selection changes, and two owners traversing the actual Agent tool loop. It does
not establish real account eligibility, remaining quota or successful live inference.

A [minimal provider replacement patch](studio-provider.patch) is supplied against
the Studio owner's working provider with `secrets()`. It is review material only,
not applied to Studio. It preserves the local payload and access-type exports.
Check the patch against the owner's latest code after updating the candidate
package; retain subsequent fixes before applying it.
