# ADR 0105: Turn-scoped approval modes

- Status: accepted
- Date: 2026-09-09

Profiles define an approval default: request approval, AI-assisted approval, or full access. A trusted interactive Surface may override it for one Turn. The Surface captures the mode and user request in an immutable invocation extension; Tool arguments cannot choose approval authority. Existing denied Tools and Run Scope remain ceilings.

The interactive approval Plugin owns approval decisions. For AI-assisted approval it invokes the bound Model separately, without Tools, using a fixed review instruction and explicitly delimited untrusted arguments. Only a valid affirmative result for an authorized low-risk operation permits execution. Timeout, malformed output, missing context, or uncertainty falls back to the user. This is model-assisted review, not a security sandbox. Decision reasons remain in Tool Hook results.

Asking the user does not require a preliminary approval. Full access skips approval prompts, not denied capabilities. Profile changes affect new Generations; Surface changes affect new Turns. The Kernel owns neither approval modes nor product-specific risk logic.
