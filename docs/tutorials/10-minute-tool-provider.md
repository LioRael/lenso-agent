# Build and run a Tool Provider in 10 minutes

This tutorial starts from the checked-in `uppercase` Tool so the
whole path runs without credentials or network APIs.

## 1. Inspect the typed Tool

Open `crates/lenso-agent-text-tools-module/src/lib.rs`. The argument type owns
its JSON Schema. `#[tool_provider]` derives `catalog`, typed JSON decoding, and
dispatch from the method marked `#[tool(...)]`:

```rust
#[tool_provider]
impl TextTools {
    #[tool(name = "uppercase", description = "Convert bounded text to uppercase.")]
    fn uppercase(arguments: UppercaseArguments) -> Result<ExecuteResponse, ExecuteError> {
        // Product behavior only.
    }
}
```

Invalid JSON becomes the portable `InvalidArguments` Domain Error; the Module
never hand-writes an endpoint or Provider factory.

Change the description or the `uppercase` function, keeping the bounded input
and output checks.

## 2. Check the contracts and App

From the repository root:

```sh
./scripts/generate-contracts.sh
./scripts/check-contracts.sh
lenso app check --definition lenso.app.json
jq empty examples/plugins/text-tools/lenso-plugin.json
```

The workspace command discovers Capability packages through Cargo metadata, so
adding a contract does not require editing a shell-script crate list.

## 3. Resolve the immutable Plan

```sh
lenso app resolve \
  --definition lenso.app.json \
  --output .lenso/resolved-plan.json
```

Review the root source App Definition for base intent and the Plugin Manifest
for the optional contribution. The generated Plan is Host state; do not edit it
by hand.

## 4. Run it

```sh
cargo run -p lenso-agent-cli -- plugins enable text-tools
cargo run -p lenso-agent-cli -- \
  "Use uppercase on: Lenso modules are replaceable."
```

The normal run resolves the root App plus the persisted Plugin Active Set;
`--plan` is reserved for exact replay.

## 5. Prove removal

```sh
./scripts/check-removal.sh
```

The proof removes optional Tool Provider instances from temporary App
Definitions and checks that the remaining graph still resolves. A removable
Module should leave no required binding, task, state meaning, or configuration
behind.
