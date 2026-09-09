# Projects daily workflow source acceptance

The local disposable App used real Auth Account, Password, browser consent,
Organization, Access Control, Projects, Agent Tools, and Projects Web Plugins
with PostgreSQL 18. No business authentication or mutation provider was mocked.

## Verified behavior

- An Issue link requires App login and returns to that same Issue after login.
- Unauthenticated consent offers App login, returns to consent, and requires
  explicit approval. The narrowed grant identifies the authenticated subject.
- A workflow catalog read followed by an exact-revision update changes the
  visible Issue to Done. Refresh shows the new revision and the real activity
  actor. A repeated stale revision returns actionable conflict guidance.
- The protocol suite passes 16 authorization/mutation checks, including private
  Team and Organization isolation, idempotency, no write on denial and parent
  revocation.
- Native connection tests verify private grants, immutable Turn identity,
  cancellation, shutdown, and that an old Turn's 401 cannot invalidate a newer
  connected account. The current rejected grant reports reconnect required.
- Console browser tests cover account presentation, reconnect, safe Issue links,
  and the existing activity disclosure. Unit tests only compute changes when a
  complete pre-read matches the mutation's expected revision.

## Evidence boundaries

The browser acceptance script invokes the actual business Tool protocol directly;
it does not invoke a language model. The previously published 1.11.0 model
acceptance is separate and is not reused as proof of these source changes.
No npm or binary release was performed for this iteration.

The source fixture pins Auth `8d5ab52ff34f9ff3cd487b6e83982fef6e5c72da`,
Projects `909a5482871fb79f04e92c65308bae7a5a466378`, and Projects Web
`6721eddfda1ee3dbad3cc49fe4e95f340f35c85d`. Their merge/release state is separate
from this local evidence.

The current Issue contract has no assignee field. This flow concerns visible
Issues supplied through their App links, not an assigned-to-me inbox.
Account presentation uses the App-issued subject ID; it does not infer a name
or email address. App owners still supply a real login route and session ingress.
