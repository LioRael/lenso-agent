# Installing marketplace plugins with Console Agent

Ask Console Agent to find and install an exact plugin release. Marketplace is a
public directory and detail website; it never connects to your Agent or receives
its control credentials.

The Console tool flow is:

1. `list_available_plugins` with an explicit `agent_id` and optional search query.
2. `check_plugin_install` with the returned `catalog_entry_id` and target revision.
3. Review the exact candidate in Console Agent and approve installation through
   the existing Agent approval flow.
4. `apply_plugin_install` with that entry, base revision and proposal digest.
5. `get_plugin_installation` with the same target and proposal digest. Only
   `succeeded` proves runtime activation. `waiting_for_ready` is pending;
   `failed` or `recovery_required` requires inspection.

Repeating apply for an already submitted signed-catalog proposal returns its
retained operation. It does not install twice. A changed target/revision or expired
review requires a new check. Updates preserve existing instances/configuration.
First installation uses plugin defaults; a plugin requiring additional mandatory
configuration may fail candidate validation and needs an authoring/configuration
workflow before it can run. Tool access remains a separate Agent policy grant.

## Official Marketplace defaults

Release builds can embed the official Marketplace source and public verification
metadata. Users ask Console Agent to find and install a plugin; they do not create
a policy file, manage keys or maintain checkpoints. Existing verification and
installation receipts remain internal to the target. This does not introduce a
new key-management UI or a separate rollback service.

Release maintainers supply the public JSON policy through the repository variable
`LENSO_OFFICIAL_MARKETPLACE_POLICY`. The release workflow embeds it at compilation;
it is not a runtime environment override and must never contain a private key.
Use the reviewed official endpoint, public verification key and artifact origins,
not a proof deployment. An unset variable leaves development distributions without
a default remote catalog; it must not be described as an enabled official market.
The official source still needs deployment and release acceptance before rollout.

## Existing operator override

For existing private deployments, the operator configures
`.lenso/marketplace-policy.json` below the selected Agent authority Home. These values are deployment trust inputs; never obtain an
Agent credential or choose signing keys from a publisher description/model reply.
For example:

```json
{
  "name": "Development Agent",
  "catalogs": [{
    "catalog_id": "my-catalog",
    "key_id": "release-key",
    "public_key_hex": "<64 hexadecimal characters supplied by the operator>",
    "snapshot_url": "https://plugins.example.com/api/marketplace/v1/snapshot"
  }],
  "artifact_origins": ["https://artifacts.example.com"]
}
```

Catalog metadata must be signed, current and monotonic. The target stores its
checkpoint, verified artifacts, proposals and operation journal under
`.lenso/marketplace/`. HTTPS is required except for an explicitly configured
loopback metadata endpoint. Artifact downloads always require admitted HTTPS
origins and public destinations. Redirects and ambient HTTP proxies are disabled.
Removing this override restores the distribution default, if one was embedded.
Without either source, existing Host-trusted Bundle entries remain available.
This does not uninstall running plugins.

A failed candidate keeps the previous active Generation, but does not restore the
desired filesystem publication automatically. Inspect the receipt and review a
working replacement or removal before restarting. Retained receipts can recover
success from active revision after restart; unknown state is never retried by a
read. Native and Process plugins run as trusted code, not sandboxed code.

## Reproduce acceptance

The Cargo source pins identify the immutable development prerequisites; a clean
checkout requires no local sibling paths. They must be replaced by released
minimum versions before registry publication.

Build the Process proof archives with
`apps/lenso-agent-web/tests/fixtures/marketplace-proof/build-variants.py`, then run:

```sh
LENSO_MARKETPLACE_ARCHIVE=/path/to/proof-0.1.0.lenso-plugin \
LENSO_MARKETPLACE_UPDATE_ARCHIVE=/path/to/proof-0.2.0.lenso-plugin \
LENSO_MARKETPLACE_FAILURE_ARCHIVE=/path/to/proof-0.3.0.lenso-plugin \
cargo test --locked -p lenso-agent-web --lib \
  console_tools_install_signed_release_and_observe_runtime -- --ignored
```

This invokes real Console tools against a private Agent, with explicit test-only
approval and tool grants. It reads signed metadata over HTTP and uses preverified
archive cache fixtures. CLI HTTPS tests separately cover real transport, TLS,
redirect rejection and immutable identity. Neither test establishes public
artifact hosting or registry publication.

## Public Echo acceptance

The opt-in `Remote Marketplace acceptance` workflow runs on macOS arm64 against
`publisher-cloudflare-proof`. It uses the committed public test policy and exact
reviewed Echo 0.1.1 archive digest. It needs no publisher credentials, signing key,
model account or local network configuration. This is test deployment acceptance,
not an official Marketplace default or an Agent distribution release.

The test starts with an empty temporary Agent Home, fetches current signed metadata
and downloads the archive through the real target-owned HTTPS transport. It calls
Console tools to check, install and observe activation, verifies duplicate apply
is idempotent, and calls Echo. A second OS process reopens the same Home and checks
the retained installation operation and Echo execution. Only Cargo compilation
artifacts are cached; the test never seeds the Agent artifact cache.

The workflow uploads a JSON receipt after both processes succeed. Expired metadata,
changed archive identity or unavailable hosting fails acceptance. Renew the test
publication through its existing publisher authority before rerunning an expired
catalog; do not disable validity checks. Run the workflow manually after changes to
installation or deployment. Its narrow PR trigger qualifies edits to the acceptance
itself without making every product PR depend on the test deployment.
