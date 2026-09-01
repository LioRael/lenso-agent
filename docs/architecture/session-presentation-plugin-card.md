# Session Presentation Plugin card

Status: linked native baseline

## Product boundary

Session Presentation owns best-effort display metadata for one completed Turn:
the stable automatic Session title and latest-turn preview. It does not own
durable Session events, user-edited titles, model context compaction, Memory,
or the Agent Loop outcome. Removing every provider removes automatic display
metadata while Turns continue normally.

## Contract and implementations

- **Provides:** `lenso.agent.session-presentation@1` (`project`).
- **Deterministic implementation:** `lenso.agent.session-presentation`; local
  whitespace normalization and bounded extraction, with no requirements.
- **Model implementation:** `lenso.agent.session-presentation.model`; requires
  exactly one Plan-bound `lenso.agent.model@2` provider and requests no Tools.
- **Root Slot:** optional, replaceable `session-presentation`.
- **State and lifecycle:** stateless; no lifecycle hook or background task.
- **Failure:** invalid input is a Domain Error. Model rejection, malformed
  structured output, Tool calls, or limit violations fail the projection. A
  provider/runtime failure remains classified as Runtime Failure. The Agent
  Loop treats every projection failure as non-terminal for the completed Turn.

## Configuration

The deterministic implementation owns only input/title/preview limits. The
model implementation owns the requested model ID, system instruction,
temperature, output-token bound, input bound, and title/preview bounds. These
values live in `plugins/lenso.agent.session-presentation.model/<instance>.toml`;
a Profile only selects that configured Instance.

The requested model is still authorized by the bound Model provider. A Model
provider rejects an unavailable model ID. The built-in OpenAI-compatible
Provider accepts its primary `model` plus an explicit bounded `allowed_models`
list. The direct Codex Provider instead admits its frozen discovered catalog;
its optional `include_models`/`exclude_models` fields affect ordinary selector
visibility only. Either path allows one Profile to use a cheaper presentation
model without changing the Agent Loop model. Session Presentation never reads
credentials or calls a provider transport directly.

## Authority and deletion

The Session Adapter retains final authority for manual title metadata and
independent title revisions. The Agent Loop rejects any automatic projection
that changes an existing automatic title. Disable or remove the selected
Session Presentation Instance and resolve again to prove the Agent remains
runnable without presentation metadata.
