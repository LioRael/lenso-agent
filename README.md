# Lenso Agent

**An open-source coding agent for your terminal and browser, built around replaceable Plugins.**

Explore a codebase, plan a change, edit files, and run checks in your workspace.
Start with the included coding Profiles, then choose the models, tools, memory,
and interfaces that fit your workflow.

[Get started](#quick-start) · [Documentation](docs/README.md) ·
[Releases](https://github.com/LioRael/lenso-agent/releases) ·
[Build a Plugin](docs/tutorials/10-minute-tool-provider.md)

## Quick start

### Browser

Run this from your project directory:

```sh
npx @lenso/agent web
```

This starts Agent and Console locally and opens your browser. Requires
**Node.js 22.12+**, with **macOS 15+ on Apple silicon** or **Ubuntu 24.04+ x64
(glibc 2.39+)**. No Rust toolchain or source checkout is needed.

Use `--port 3031` to choose a port or `--no-open` to print the URL without
opening a browser. See the [npm distribution guide](https://github.com/LioRael/lenso-console/blob/main/docs/agent-npm-distribution.md)
for details.

### Terminal

Install with Homebrew:

```sh
brew install LioRael/tap/lenso-agent
lenso-agent-cli auth login
lenso-agent-cli profiles install coding
lenso-agent --profile code
```

Or use npm without a global installation:

```sh
npx @lenso/agent cli auth login
npx @lenso/agent cli profiles install coding
npx @lenso/agent --profile code
```

The npm package also supports `cli` for headless requests and `acp` for editors.
For direct binary downloads, verification, upgrades, and removal, see
[installation](docs/installation.md). Release binaries support Apple silicon on
macOS 15+ and x86-64 Linux with glibc 2.39+ (Ubuntu 24.04+).

Try a request such as:

```text
Explain how this project is organized and where its main entry points are.
```

Or give it a concrete change:

```text
Fix the failing test, run the relevant checks, and summarize the changes.
```

## Work your way

The included Profiles let you choose how the agent works:

| Profile | Use it for |
| --- | --- |
| `code` | Edit files, run configured programs, use Git, and delegate work, with inline approvals. |
| `plan` | Explore the workspace and plan changes with read-only tools. |
| `code-sandbox` | Code with OS-isolated process execution and network access disabled. |

```sh
lenso-agent --profile plan
lenso-agent --profile code-sandbox
```

The sandbox Profile uses Seatbelt on macOS and requires `bwrap` with usable
unprivileged namespaces on Linux. The ordinary `code` Profile limits executable
access but does not provide an OS sandbox. See [Profile configuration](docs/configuration.md#choose-a-session-profile)
for the exact boundaries.

- **Project instructions and Skills.** Coding Profiles load `AGENTS.md` from the
  repository root through your working directory. Type `/` in the terminal to
  browse commands and available Skills.
- **Reviewable edits.** File changes use checkpoints so the agent can inspect
  diffs and accept or restore its edits.
- **Persistent sessions.** Configuration and history live in `~/.lenso/agent/`,
  independently of the project you open. Resume a session when you return.

```sh
lenso-agent-cli sessions list
lenso-agent --profile code --session <id>
```

For a single request from a script or terminal, use the headless CLI:

```sh
lenso-agent-cli --profile plan "Summarize this workspace README."
```

## Make it yours

Plugins let you change more than the tool list: swap the model provider,
session storage, memory, or agent loop, and select different combinations with
named Profiles. Configuration lives in ordinary files under your Agent Home.

Connect external tools through **MCP**, add reusable **Skills**, or write your
own **Tool Plugin**. Start with the [configuration guide](docs/configuration.md)
or the [10-minute Plugin tutorial](docs/tutorials/10-minute-tool-provider.md).
Plugin authoring and package management use the separate
[Lenso CLI](https://github.com/LioRael/lenso-cli).

## Beyond the terminal

| Interface | Getting started |
| --- | --- |
| ACP-compatible editors | [Install the ACP entrypoint and connect an editor](docs/integrations.md#acp-editors). |
| Web and Console | [Run the Agent Web API and configure access](docs/integrations.md#web-api-and-configuration-control). |
| Your own application | [Embed the Rust Agent Host](docs/integrations.md#embed-lenso-agent). |
| Telegram and Discord | [Run the channel Host from source](docs/integrations.md#run-chat-channels). |

## Documentation

- [Install, upgrade, and uninstall](docs/installation.md)
- [Configure Profiles, models, MCP, memory, and Plugins](docs/configuration.md)
- [Connect editors, Web APIs, and chat channels](docs/integrations.md)
- [Build your first Tool Plugin](docs/tutorials/10-minute-tool-provider.md)
- [Add a command to the CLI and terminal UI](docs/tutorials/add-terminal-command-provider.md)
- [Architecture and source development](docs/architecture/host-internals.md)
- [Full documentation map](docs/README.md)

## License

[MIT](LICENSE)
