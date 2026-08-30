# Build and add a Tool Plugin in 10 minutes

This is the complete normal extension-author workflow; only the Plugin source
and commands below are part of it.

## 1. Create the Plugin

```sh
lenso plugin new uppercase
cd uppercase
```

The generated project contains one Plugin identity, one Rust/Wasm source file,
and the `lenso.agent.tool-provider@2` Tool contract expected by Lenso Agent.
Edit `src/lib.rs` to implement the behavior.

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

## 4. Add it to Lenso Agent

From the Lenso Agent repository:

```sh
lenso plugins add \
  path/to/uppercase/dist/uppercase-0.1.0.lenso-plugin
lenso plugins list
lenso-agent-cli "Use the text Plugin to uppercase Lenso plugin."
```

Adding a newer release with the same Plugin ID replaces the package directory
only after the received bytes pass validation. The Host derives a candidate App
from the new Plugin Root and keeps the current Generation if readiness fails.

## 5. Disable, re-enable, or remove it

```sh
lenso plugins disable dev.example.uppercase
lenso plugins enable dev.example.uppercase
lenso plugins remove dev.example.uppercase
```

Disable keeps the Plugin package and configuration available for re-enabling.
Remove deletes that Plugin's entry from this App's `plugins/` directory.
