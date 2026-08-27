# Build and add a Tool Plugin in 10 minutes

This is the complete normal extension-author workflow; only the Plugin source
and commands below are part of it.

## 1. Create the Plugin

```sh
lenso plugin new uppercase
cd uppercase
```

The generated project contains one Plugin identity, one Rust/Wasm source file,
and the `lenso.agent.tool-provider@2` Tool contract expected by the Agent
Harness. Edit `src/lib.rs` to implement the behavior.

## 2. Check and run it locally

```sh
lenso plugin check
lenso plugin dev --operation execute \
  --request-json '{"name":"uppercase","arguments_json":"{\"text\":\"hello\"}"}'
```

`dev` uses the same Wasm Component execution path used after installation.

## 3. Package it

```sh
lenso plugin pack
```

`pack` builds the release, creates a non-overwriting `.lenso-plugin` directory,
and reopens the exact bytes it wrote. There is no separate `plugin verify`
step.

## 4. Add it to the Harness

From the Harness project:

```sh
lenso-agent-cli plugins add \
  path/to/uppercase/dist/uppercase-0.1.0.lenso-plugin
lenso-agent-cli plugins status
lenso-agent-cli "Use the text Plugin to uppercase Lenso plugin."
```

Adding a newer release with the same Plugin ID updates it. The previous active
release remains selected if validation or readiness fails.

## 5. Disable, re-enable, or remove it

```sh
lenso-agent-cli plugins disable uppercase
lenso-agent-cli plugins enable uppercase
lenso-agent-cli plugins remove uppercase
```

Disable keeps the selected release available for re-enabling. Remove forgets
the Plugin from this App.
