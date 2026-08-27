# Plugin clean-room V1 evidence

Date: 2026-08-27

## Result

The end-to-end public workflow is release-complete. A fresh installation of
`lenso-cli 0.4.7` from crates.io generated a
`lenso.agent.tool-provider@2` Plugin with `catalog` and `execute`, and that
Plugin completed the entire Harness lifecycle using registry dependencies
only. No locally built CLI or sibling path dependency participated in this
proof.

## Release checks

| Artifact | Verified release |
| --- | --- |
| crates.io `lenso-cli` | `0.4.7` |
| npm `@lenso/cli` | `0.14.0` |
| crates.io `lenso` | `0.4.3` |
| crates.io `lenso-guest-sdk` | `0.1.3` |
| crates.io `lenso-plugin-bundle` | `0.1.3` |

The npm and Cargo packages have independent version lines and are not reported
as one shared version. The npm tarball matched registry integrity
`sha512-KtdS56rNYf7JE4lhFqWYLry9xb0UPAIyJWrc391oanx1Rqo6yJRIdYHtQPhXV+9RT8zcSR1QFE/PTm0Sm5km1Q==`;
its vendored executable reported `lenso 0.4.7` and generated the same Tool
Provider contract without forbidden author vocabulary.

## Public CLI reproduction

The CLI was installed into an empty temporary prefix:

```sh
cargo install lenso-cli --version 0.4.7 --locked --root "$TOOLCHAIN"
git init "$PROOF_ROOT"
"$TOOLCHAIN/bin/lenso" plugin new uppercase --repo-root "$PROOF_ROOT"
cd "$PROOF_ROOT/uppercase"
lenso plugin check
lenso plugin dev --operation execute \
  --request-json '{"name":"uppercase","arguments_json":"{\"text\":\"hello\"}"}' \
  --json
lenso plugin pack --json
```

The generated repository contained only `Cargo.toml`, `Cargo.lock`, `README.md`,
`src/lib.rs`, and `wit/world.wit`. The author-vocabulary audit returned no
Module, Manifest-template, Plan, Receipt, Store, Controller, Supervisor, or
Generation terms. `cargo metadata --locked` showed `lenso-guest-sdk 0.1.3`
with the crates.io registry source and no sibling path or Git dependency.

## Public CLI lifecycle

The released scaffold emits the Harness Tool contract directly. After editing
the generated Tool to uppercase its input:

```text
plugin check: uppercase@0.1.0 passed
plugin dev: HELLO LENSO
plugin pack manifest: sha256:1c8dd4d5b85060adb02765729fcd5a804e67cb1966c09735df34d6e306716c74
plugin pack artifact: sha256:d47eac42cc4a3760f968546a177aee3df0f45f8db6bce2cc0249c025edf7b5fd
Harness add: uppercase@0.1.0 enabled
Harness Tool result: LENSO PLUGIN
```

The source and version were then changed without invoking a separate verify
command:

```text
plugin check: uppercase@0.1.1 passed
plugin dev: v2: HELLO LENSO
plugin pack manifest: sha256:9e2e99e46d4d8f565336c66956ae97c5d5aca5109550aa4d6a6647244d5b0f98
plugin pack artifact: sha256:37e58e217d8d3c2acb5613ba3ce23f4915b07e51e55dee42d50b5b16946c6d61
Harness add: uppercase@0.1.1 enabled
Harness Tool result: v2: LENSO PLUGIN
```

Changing only the package version again from `0.1.1` to `0.1.2` and running
`plugin check` synchronized the root package entry in `Cargo.lock` offline and
passed. No extra Cargo lock-maintenance command was required.

Truncating one byte from `plugin.wasm` caused admission to fail with
`digest mismatch for plugin.wasm`. `uppercase@0.1.1` remained enabled and
continued returning the V2 result. Disable retained the release, enable
restored it, remove produced `No plugins.`, and the base App still completed a
Turn with `README summary: # Lenso Agent Harness`.

This closes the public clean-room gate for the four-command authoring workflow:
`plugin new → check → dev → pack`. Integrity is checked by `pack` and checked
again at Harness admission; no separate `plugin verify` command is required.
