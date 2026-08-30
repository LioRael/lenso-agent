# Install, upgrade, and remove Lenso Agent

## Supported release targets

The first binary release supports:

- Apple silicon on macOS 15 or later (`darwin-aarch64`); and
- x86-64 Linux (`linux-x86_64`).

Windows, Intel macOS, and ARM64 Linux remain source-build targets until their
native release jobs and clean-room acceptance gates exist.

## Install

Download the installer from the exact versioned release, inspect it, and run
it without elevated privileges:

```sh
curl --fail --location \
  https://github.com/LioRael/lenso-agent/releases/download/v0.1.0/install.sh \
  --output /tmp/lenso-agent-install.sh
less /tmp/lenso-agent-install.sh
sh /tmp/lenso-agent-install.sh --version 0.1.0
```

The default destination is `~/.local/bin`. Set `LENSO_AGENT_INSTALL_DIR` or
pass `--install-dir` with an absolute path to choose another location. The
installer downloads each independent archive and checksum, verifies SHA-256,
extracts into a temporary directory, and replaces the destination binary only
after verification succeeds.

The default selection installs:

- `lenso-agent`, the interactive terminal product; and
- `lenso-agent-cli`, authentication, Profile management, diagnostics, Session
  operations, and the headless surface.

Install the independent ACP entrypoint when an editor needs it:

```sh
sh /tmp/lenso-agent-install.sh --version 0.1.0 --component acp
```

## First run

```sh
lenso-agent-cli auth login
lenso-agent-cli profiles install coding
lenso-agent-cli doctor
lenso-agent --profile code
```

`doctor --json` exposes the same non-secret checks for support automation. It
reports release version, platform, Agent Home, authentication presence,
installed entrypoints, and coding dependencies. It never prints credentials.

## Verify release provenance

Every archive has a sibling `.sha256` record and the Release includes one
`SHA256SUMS` index. GitHub also publishes build-provenance attestations:

```sh
gh attestation verify \
  lenso-agent-v0.1.0-darwin-aarch64.tar.gz \
  --repo LioRael/lenso-agent
```

The initial macOS prerelease is not platform-signed. Verify its SHA-256 and
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
