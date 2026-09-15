# Install, upgrade, and remove Lenso Agent

## Homebrew

Install the terminal UI, management CLI, and ACP entrypoint:

```sh
brew install LioRael/tap/lenso-agent
```

Homebrew checks the published archive digests and installs ripgrep. Use
`brew upgrade lenso-agent` to upgrade and `brew uninstall lenso-agent` to remove
these binaries. Agent Home is preserved. Continue with [First run](#first-run).

## npm / npx

Run the terminal from your project without a global install:

```sh
npx @lenso/agent auth login
npx @lenso/agent profiles install coding
npx @lenso/agent --profile code
```

For a persistent installation, `npm install -g @lenso/agent` exposes the same
launcher as `lenso-agent`; use `lenso-agent doctor` for diagnostics and
`lenso-agent acp` for an editor. Choose one global installation method so npm
and Homebrew do not compete for the `lenso-agent` command.

Use `npx @lenso/agent web` to start Agent and Console in your browser. The npm
package requires Node.js 22.12+ and supports macOS 15+ on Apple silicon or
Ubuntu 24.04+ x64 (glibc 2.39+). It opens the local UI; use `--no-open` to print
the URL instead. See the [npm distribution guide](https://github.com/LioRael/lenso-console/blob/main/docs/agent-npm-distribution.md).

The npm launcher version follows Console; `npx @lenso/agent tui --version`
reports its pinned native Agent version. Native binaries remain independently
versioned. Use `npm update -g @lenso/agent` or `npm uninstall -g @lenso/agent`
for a global npm install; both preserve durable Agent state.

## Supported release targets

The `0.1.11` prerelease provides binaries for:

- Apple silicon on macOS 15 or later (`darwin-aarch64`); and
- x86-64 Linux with glibc 2.39+ (`linux-x86_64`, Ubuntu 24.04+).

Windows, Intel macOS, and ARM64 Linux remain source-build targets until their
native release jobs and clean-room acceptance gates exist.

## Install

Download the installer from the exact versioned release, inspect it, and run
it without elevated privileges:

```sh
curl --fail --location \
  https://github.com/LioRael/lenso-agent/releases/download/v0.1.11/install.sh \
  --output /tmp/lenso-agent-install.sh
less /tmp/lenso-agent-install.sh
sh /tmp/lenso-agent-install.sh --version 0.1.11
```

The default destination is `~/.local/bin`. Set `LENSO_AGENT_INSTALL_DIR` or
pass `--install-dir` with an absolute path to choose another location. The
installer downloads each independent archive and checksum, verifies SHA-256,
extracts into a temporary directory, and replaces the destination binary only
after verification succeeds.

The default selection installs:

- `lenso-agent`, the interactive terminal product; and
- `lenso-agent-cli`, the companion executable used by `lenso-agent` for authentication, Profile management, diagnostics, Session
  operations, and the headless surface;
- `lenso-agent-web`, the standalone Agent Web API; and
- `lenso-agent-console-web`, the Console Agent Web API.

Install the independent ACP entrypoint when an editor needs it:

```sh
sh /tmp/lenso-agent-install.sh --version 0.1.11 --component acp
```

## First run

```sh
lenso-agent auth login
lenso-agent profiles install coding
lenso-agent doctor
lenso-agent --profile code
```

Install coding Profiles before attaching the Agent Home to SQLite or a remote
configuration authority. The offline installer refuses Homes carrying the
managed-configuration guard (and legacy default SQLite stores) before changing
files. Stopping the Host does not relinquish its durable authority. Use a fresh
Home for local coding Profiles or the owning authority's supported publication
operations. For a running SQLite-managed Home, use
[the live Profile import command](integrations.md#web-api-and-configuration-control).


`doctor --json` exposes the same non-secret checks for support automation. It
reports release version, platform, Agent Home, authentication presence,
installed entrypoints, and coding dependencies. It never prints credentials.

## Verify release provenance

Every archive has a sibling `.sha256` record and the Release includes one
`SHA256SUMS` index. GitHub also publishes build-provenance attestations:

```sh
gh attestation verify \
  lenso-agent-v0.1.11-darwin-aarch64.tar.gz \
  --repo LioRael/lenso-agent
```

The macOS prerelease is not platform-signed. Verify its SHA-256 and
GitHub build-provenance attestation before use.

## Upgrade and rollback

Download the installer from the new exact release and run it with the new
version. Archives are verified before any installed binary is replaced, so a
download or checksum failure leaves the previous binary intact. Agent Home is
not part of the binary transaction and remains at `~/.lenso/agent` or the
absolute `LENSO_AGENT_HOME` override.

For binary rollback, rerun the installer from the previous exact version. New
Host releases preserve existing Session, Memory, Profile, and Plugin Root data;
runtime schemas continue to fail closed when a release cannot read them.

## Uninstall

Remove the default product binaries while retaining Agent Home:

```sh
sh /tmp/lenso-agent-install.sh --uninstall
```

Remove ACP separately if it was installed:

```sh
sh /tmp/lenso-agent-install.sh --component acp --uninstall
```

The installer prints the preserved Agent Home path. Delete durable state only
with the separate, explicit `--purge-agent-home` option. That removes Sessions,
Memory, credentials, Profiles, Plugins, and runtime history and cannot be
undone.

## Unified terminal command

`lenso-agent` opens the terminal UI. Use `lenso-agent run <prompt>` for an
explicit headless task, and `lenso-agent auth`, `profiles`, `sessions`, `models`,
`contexts`, `approvals`, or `doctor` for management. Put options after the
subcommand, for example `lenso-agent run --profile plan "Review this project"`.
A prompt equal to a command name remains a prompt under `run`; use `run --
"--leading-dash prompt"` for literal text beginning with a dash.

The native entrypoint starts only the selected surface and locates its companion
executable beside itself, never through PATH. Keep `lenso-agent` and
`lenso-agent-cli` from the same release in the same directory. Homebrew, npm,
and the installer's `--component agent` selection install both. `acp` additionally
requires the ACP component. Missing companions produce an installation error.

Existing `lenso-agent-cli <prompt>` scripts and management commands remain
supported, as do the explicit `lenso-agent cli` and `lenso-agent tui` entrypoints.
The separate `lenso` command continues to own Plugin authoring and management.
