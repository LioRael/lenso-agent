# Dynamic Model Selection Plugin card

Status: implemented native baseline for Turn-scoped model policies.

## Product job

When a user selects a policy name such as `auto`, choose an appropriate model
for the current Turn. An App can use cheap local rules, ask an admitted model
to classify the work, or distribute Turns across a fixed weighted pool.

## Ownership and deletion boundary

`lenso.agent.model-selection.dynamic` owns policy configuration and the choice
among candidates supplied for one Turn. It provides
`lenso.agent.model-selection@1` and requires `lenso.agent.model@2` only for the
optional LLM-classifier strategy.

Removing the Plugin removes dynamic aliases. The Host, Agent Loop, concrete
`/model` selection, Provider/Model Catalog, Session storage, and Model Plugins
remain intact. A policy never discovers models and cannot return a model that
the Host did not admit.

## Example Plugin configuration

```toml
[[policies]]
id = "auto"
description = "Use the stronger model for complex work"
strategy = "rules"
default_model = "provider/fast"
strong_model = "provider/strong"
min_input_characters = 8000
strong_keywords = ["architecture", "migration", "security review"]

[[policies]]
id = "mixed"
description = "Distribute Turns across an admitted pool"
strategy = "weighted_random"

[[policies.candidates]]
model = "provider/fast"
weight = 8

[[policies.candidates]]
model = "provider/strong"
weight = 2

[[policies]]
id = "judge"
description = "Ask a model whether this Turn needs the stronger model"
strategy = "llm_classifier"
classifier_model = "provider/fast"
default_model = "provider/fast"
strong_model = "provider/strong"
fallback_model = "provider/fast"
instruction = "Classify whether the work requires deep multi-step reasoning."
max_output_tokens = 8
```

Every referenced model must also be admitted by the Model Provider Instance
selected in the immutable Generation. The classifier receives no Tools, uses
temperature zero, and must return exactly `default` or `strong`; any model
error or malformed response uses `fallback_model`.

## Turn sequence

1. A Surface requests `auto` as the model identifier.
2. The Host resolves eligible candidate profiles from the selected Provider.
3. Before `turn_started`, the Agent Loop invokes the selected policy Plugin.
4. The Agent Loop validates the returned model against the Host-issued list.
5. One exact `ResolvedTurnProfile` drives compaction and every model step.
6. `turn_started` stores `model_selection` beside that resolved profile.

The current boundary intentionally does not route across Provider Instances.
That changes credentials, transport, and authority and therefore requires a
Generation-level design.
