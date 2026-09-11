# Editors, Web APIs, embedding, and chat channels

[Back to the README](../README.md) · [Documentation map](README.md)

Use Lenso Agent from another interface or embed it in a Host. Start with
[installation](installation.md) for the terminal workflow. Commands using
`cargo run` require a source checkout.

## ACP editors

After downloading the installer in the [installation guide](installation.md),
install the ACP component and expose a Profile to an editor over stdio:

```sh
sh /tmp/lenso-agent-install.sh --version 0.1.10 --component acp
lenso-agent-acp --profile code
```

The ACP process implements stable protocol v1. The editor's session `cwd` must
match the process Workspace. Text and resource links, streaming Agent and Tool
updates, cancellation, and one-shot permission requests are supported; the
entrypoint does not advertise client-provided MCP servers, additional roots,
session loading, or rich media yet.

Zed users can add the installed binary as a custom External Agent while the
ACP Registry submission is awaiting admission:

```json
{
  "agent_servers": {
    "lenso": {
      "type": "custom",
      "command": "lenso-agent-acp",
      "args": []
    }
  }
}
```

Zed's supported publication path is now the ACP Registry; its older Agent
Server extension format is deprecated. VS Code does not currently expose a
public native ACP Agent registration API, so Lenso does not present an MCP
configuration or a third-party ACP client as first-party VS Code packaging.
Release and Registry verification details live in [`packaging/acp-registry/`](../packaging/acp-registry/).

## Web API and configuration control

For the complete browser UI, run `npx @lenso/agent web` from your workspace.
See [npm installation and requirements](installation.md#npm--npx).
The standalone API setup below is for custom clients and Host administration.

Start the standalone Lenso Agent Web surface with a durable, administrator-controlled
Tool allowlist:

```sh
LENSO_AGENT_CONTROL_TOKEN=replace-with-a-local-control-token \
  lenso-agent-web \
  --listen 127.0.0.1:8788 \
  --tool-policy ~/.lenso/agent/console-agent-tool-policy.json
```

The control route accepts only the matching bearer token. Updates validate
against the active Plan-bound Tool catalog, use an expected revision, persist
before activation, and affect only Turns admitted after the update.

Loopback listeners preserve token-free local Agent requests. A standalone
listener on any non-loopback address fails startup unless
`LENSO_AGENT_WEB_TOKEN` is non-empty; every Agent data-plane request must then
send that value as its bearer token. Keep TLS in front of remote deployments.
Embedded surfaces default to a disabled data plane: the embedding Host must
explicitly select `Local`, `Bearer`, or `HostAuthorized`, and may select `Local`
only after it has pinned the transport to loopback.

Plugin configuration control can additionally select an explicit durable
authority. Proposals remain read-only, while publication uses revision CAS,
materializes the reviewed Plugin Root, and records durable history:

```sh
LENSO_AGENT_CONTROL_TOKEN=replace-with-a-local-control-token \
  lenso-agent-web \
  --listen 127.0.0.1:8788 \
  --plugin-control \
  --plugin-configuration-store ~/.lenso/agent/plugin-configuration.sqlite3
```

The store path must be absolute. Omitting it preserves direct local Plugin Root
authority. This SQLite adapter is a single-Host persistence boundary, not a
remote configuration service or distributed rollout coordinator.

A running SQLite-managed Agent can import the official coding Profiles without
restarting or writing behind the authority's back. Use the same control token as
the running Web Host (the URL includes the Agent API prefix):

```sh
lenso-agent-cli profiles import coding \
  --url http://127.0.0.1:8788/api/console/v1/agent
```

The command reads `LENSO_AGENT_CONTROL_TOKEN`, obtains the current revision and
process identity, and imports `plan`, `code`, and `code-sandbox`. Customized files
are preserved by rejecting the import; repeating an unchanged import is safe.
Existing enabled Plugins remain enabled. Import makes Profiles available; select
Plan or Auto in Console, or use the authorized `POST /control/profile` endpoint,
to activate one through the Ready Gate. Failed activation preserves the previous
mode. Imported Profiles survive restart and support `--profile plan` or
`--profile code`. Remote/injected authorities do not support this import, and
`profiles install coding` remains blocked in managed Homes.

Package installation uses a separate Host-owned trust boundary. Register each
reviewed Bundle under an opaque catalog entry ID; Console Agent can list that
ID and request a reviewed install, but cannot supply a path, URL, or package
bytes:

```sh
LENSO_AGENT_CONTROL_TOKEN=replace-with-a-local-control-token \
  lenso-agent-web \
  --listen 127.0.0.1:8788 \
  --plugin-control \
  --trusted-plugin-bundle uppercase=/opt/lenso/plugins/uppercase
```

The option is repeatable and requires an absolute Bundle path. Installation,
enablement, and configuration remain distinct changes. Removal moves the
installed package into the managed trash area; it does not purge Plugin data.

A Host can instead select one remote configuration resource. The resource
identity is explicit and the bearer token is read separately from the command
line so it is not exposed in process arguments:

First prepare a dedicated service root with the exact Host Catalog from a
compatible Host build, then start the single-resource service:

```sh
LENSO_PLUGIN_CONFIGURATION_SERVICE_READ_TOKEN=replace-with-a-read-token \
LENSO_PLUGIN_CONFIGURATION_SERVICE_WRITE_TOKEN=replace-with-a-write-token \
  cargo run -p lenso-agent-web \
  --no-default-features \
  --bin lenso-plugin-configuration-service -- \
  --listen 127.0.0.1:8790 \
  --root /var/lib/lenso-config/agent-production \
  --database /var/lib/lenso-config/agent-production/configuration.sqlite3 \
  --app agent \
  --environment production
```

The service root is its own durable desired-state materialization and must not
be shared with a running Host. The read credential permits inspection,
history, and change watching. The distinct write credential additionally
permits proposal, publication, and rollback operations. Put TLS in front of
non-loopback deployments; credentials and TLS secret distribution remain
deployment responsibilities.

Then configure a Console-capable Host with the service write credential, or a
read-only Host with the read credential:

```sh
LENSO_AGENT_CONTROL_TOKEN=replace-with-a-local-control-token \
LENSO_PLUGIN_CONFIGURATION_REMOTE_TOKEN=replace-with-a-service-token \
  lenso-agent-web \
  --listen 127.0.0.1:8788 \
  --plugin-control \
  --plugin-configuration-remote https://config.example.com/ \
  --plugin-configuration-app agent \
  --plugin-configuration-environment production
```

The adapter addresses
`/v1/apps/{app}/environments/{environment}` beneath the service URL. HTTPS is
required except for loopback development. Before the first App Generation it
recovers ordered remote changes from the visible semantic Plugin Root revision.
It then long-polls the same bounded change feed and materializes each transition
through the local Plugin Root, existing reconciler, and Ready Gate. Missing or
reordered history fails closed; there is no whole-root overwrite fallback. The
remote service remains desired-state CAS and audit authority, while each Host
continues to resolve the immutable Plan and Generation it actually runs.

An embedding Host composes the library Surface with its own explicitly linked
Plugin inventory. `lenso-agent-web` does not select Console, workspace, process,
or other product behavior for the Host:

```rust,ignore
let mut config = AgentWebConfig::new(my_product_plugins::link);
config.agent_home = Some(agent_home);
config.access = AgentWebAccess::HostAuthorized;
config.control = AgentWebControl::HostAuthorized;

let surface = AgentWebSurface::start(config).await?;
let app = Router::new().merge(surface.router());
```

Cross-repository Hosts consume `lenso-agent-web` and their selected Plugin
inventory from the same exact Lenso Agent Git revision. Local path dependencies are
only a coordinated-development aid and are not a delivery boundary.

## Embed Lenso Agent

An application declares only the Plugins compiled into its Host Build, its
process-owned surface, and the Profile to run:

```rust
let host = AgentHost::builder()
    .plugins(lenso_agent_default_plugins::link)
    .surface(TuiSurface::terminal())
    .build()?;

let mut app = host.run(Profile::named("code")).await?;
// The TUI owns its event loop; `app` supplies Generation-pinned Agent Turns.
app.shutdown().await?;
```

A headless binary swaps only the surface:

```rust
let host = AgentHost::builder()
    .plugins(lenso_agent_default_plugins::link)
    .surface(HeadlessSurface::stdio())
    .build()?;

let mut app = host.run(Profile::Default).await?;
```

`lenso::host::HostBuilder` is the lower framework seam that owns durable
Generation recovery, Controller execution, fenced routes, and shutdown. Agent
Profiles, Turns, sessions, and TUI or channel loops remain owned by Lenso Agent.

## Run chat channels

From a source checkout, copy the [example configuration](../lenso.channels.example.toml),
select exact chat or channel allowlists, and
keep tokens in environment variables:

```sh
mkdir -p ~/.lenso/agent
cp lenso.channels.example.toml ~/.lenso/agent/channels.toml
export TELEGRAM_BOT_TOKEN='<bot-token>'
export DISCORD_BOT_TOKEN='<bot-token>'
cargo run -p lenso-agent-channel
```

Delete either `[telegram]` or `[discord]` from the file to run only one
transport. The shared Host runs one Agent Turn at a time and bounds pending
work across channels.

The distributions remain independent. Release archives contain one executable
and its exact surface Plugin Catalog. The default product installer selects the
interactive `lenso-agent` and management `lenso-agent-cli` archives together;
The default installer also includes the standalone and Console Web APIs. ACP
is opt-in; the Channel surface runs from source.

