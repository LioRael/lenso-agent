//! Dynamic, turn-scoped Model selection policies.

use std::collections::BTreeSet;

use lenso::prelude::*;
use lenso_capability_agent_model::{
    self as model_contract, CompleteMessageInput, CompleteMessageKind, CompleteMessageRole,
    CompleteOpen, ModelCompleteEvent,
};
use lenso_capability_agent_model_selection::{
    self as selection_contract, SelectError, SelectRequest, SelectResponse,
};
use lenso_kernel::{RuntimeFailure, StreamEvent};
use sha2::{Digest, Sha256};

const MAX_CLASSIFIER_RESPONSE_CHARACTERS: usize = 128;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionConfig {
    policies: Vec<PolicyConfig>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "strategy")]
enum PolicyConfig {
    Rules {
        id: String,
        description: String,
        default_model: String,
        strong_model: String,
        min_input_characters: usize,
        strong_keywords: Vec<String>,
    },
    WeightedRandom {
        id: String,
        description: String,
        candidates: Vec<WeightedCandidate>,
    },
    LlmClassifier {
        id: String,
        description: String,
        classifier_model: String,
        default_model: String,
        strong_model: String,
        fallback_model: String,
        instruction: String,
        max_output_tokens: i64,
    },
}

impl PolicyConfig {
    fn id(&self) -> &str {
        match self {
            Self::Rules { id, .. }
            | Self::WeightedRandom { id, .. }
            | Self::LlmClassifier { id, .. } => id,
        }
    }

    fn description(&self) -> &str {
        match self {
            Self::Rules { description, .. }
            | Self::WeightedRandom { description, .. }
            | Self::LlmClassifier { description, .. } => description,
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WeightedCandidate {
    model: String,
    weight: u32,
}

fn validate_config(config: &SelectionConfig) -> Result<(), RuntimeFailure> {
    if config.policies.is_empty() || config.policies.len() > 32 {
        return Err(invalid_plan(
            "Model Selection must define 1 through 32 policies",
        ));
    }
    let mut ids = BTreeSet::new();
    for policy in &config.policies {
        if !valid_id(policy.id())
            || !ids.insert(policy.id())
            || policy.description().trim().is_empty()
            || policy.description().chars().count() > 256
        {
            return Err(invalid_plan("Model Selection policy metadata is invalid"));
        }
        match policy {
            PolicyConfig::Rules {
                default_model,
                strong_model,
                min_input_characters,
                strong_keywords,
                ..
            } => {
                if !valid_model(default_model)
                    || !valid_model(strong_model)
                    || *min_input_characters == 0
                    || *min_input_characters > 262_144
                    || strong_keywords.len() > 64
                    || strong_keywords
                        .iter()
                        .any(|keyword| keyword.trim().is_empty() || keyword.chars().count() > 128)
                {
                    return Err(invalid_plan("rules Model Selection policy is invalid"));
                }
            }
            PolicyConfig::WeightedRandom { candidates, .. } => {
                let mut models = BTreeSet::new();
                if candidates.is_empty()
                    || candidates.len() > 16
                    || candidates.iter().any(|candidate| {
                        !valid_model(&candidate.model)
                            || candidate.weight == 0
                            || candidate.weight > 1_000_000
                            || !models.insert(candidate.model.as_str())
                    })
                    || candidates
                        .iter()
                        .try_fold(0_u64, |total, candidate| {
                            total.checked_add(u64::from(candidate.weight))
                        })
                        .is_none()
                {
                    return Err(invalid_plan(
                        "weighted-random Model Selection policy is invalid",
                    ));
                }
            }
            PolicyConfig::LlmClassifier {
                classifier_model,
                default_model,
                strong_model,
                fallback_model,
                instruction,
                max_output_tokens,
                ..
            } => {
                if [
                    classifier_model.as_str(),
                    default_model.as_str(),
                    strong_model.as_str(),
                    fallback_model.as_str(),
                ]
                .into_iter()
                .any(|model| !valid_model(model))
                    || instruction.trim().is_empty()
                    || instruction.chars().count() > 8_192
                    || !(1..=256).contains(max_output_tokens)
                {
                    return Err(invalid_plan("LLM Model Selection policy is invalid"));
                }
            }
        }
    }
    Ok(())
}

#[lenso::plugin(
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct DynamicModelSelection {
    #[config]
    config: SelectionConfig,
    model: Port<model_contract::ModelClient>,
}

#[lenso::provides(selection_contract::ModelSelection)]
impl DynamicModelSelection {
    async fn select(
        &self,
        context: Ctx,
        request: SelectRequest,
    ) -> PluginResult<SelectResponse, SelectError> {
        let policy = self
            .config
            .policies
            .iter()
            .find(|policy| policy.id() == request.policy)
            .ok_or_else(|| PluginError::domain(SelectError::UnknownPolicy))?;
        let admitted = request
            .candidates
            .iter()
            .map(|candidate| candidate.model.as_str())
            .collect::<BTreeSet<_>>();
        if admitted.is_empty() {
            return Err(PluginError::domain(SelectError::NoCandidate));
        }
        match policy {
            PolicyConfig::Rules {
                default_model,
                strong_model,
                min_input_characters,
                strong_keywords,
                ..
            } => {
                let normalized = request.input.to_lowercase();
                let strong = request.input.chars().count() >= *min_input_characters
                    || strong_keywords
                        .iter()
                        .any(|keyword| normalized.contains(&keyword.to_lowercase()));
                let selected = if strong { strong_model } else { default_model };
                selected_response(
                    &admitted,
                    selected,
                    "rules",
                    if strong {
                        "strong_rule"
                    } else {
                        "default_rule"
                    },
                )
            }
            PolicyConfig::WeightedRandom { candidates, .. } => {
                let selected = weighted_model(
                    candidates,
                    &admitted,
                    &request.selection_id,
                    &request.policy,
                )
                .ok_or_else(|| PluginError::domain(SelectError::NoCandidate))?;
                selected_response(&admitted, selected, "weighted_random", "weighted_draw")
            }
            PolicyConfig::LlmClassifier {
                classifier_model,
                default_model,
                strong_model,
                fallback_model,
                instruction,
                max_output_tokens,
                ..
            } => {
                if !admitted.contains(classifier_model.as_str()) {
                    return Err(PluginError::domain(SelectError::NoCandidate));
                }
                let result = classify(
                    &self.model,
                    context,
                    classifier_model,
                    instruction,
                    &request.input,
                    *max_output_tokens,
                )
                .await;
                let (selected, reason_code) = match result.as_deref() {
                    Ok("strong") => (strong_model, "llm_strong"),
                    Ok("default") => (default_model, "llm_default"),
                    _ => (fallback_model, "llm_fallback"),
                };
                selected_response(&admitted, selected, "llm_classifier", reason_code)
            }
        }
    }
}

fn weighted_model<'a>(
    candidates: &'a [WeightedCandidate],
    admitted: &BTreeSet<&str>,
    selection_id: &str,
    policy: &str,
) -> Option<&'a str> {
    let eligible = candidates
        .iter()
        .filter(|candidate| admitted.contains(candidate.model.as_str()))
        .collect::<Vec<_>>();
    let total = eligible
        .iter()
        .map(|candidate| u64::from(candidate.weight))
        .sum::<u64>();
    if total == 0 {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(selection_id.as_bytes());
    digest.update([0]);
    digest.update(policy.as_bytes());
    let bytes: [u8; 8] = digest.finalize()[..8]
        .try_into()
        .expect("SHA-256 prefix has eight bytes");
    let mut draw = u64::from_be_bytes(bytes) % total;
    eligible.into_iter().find_map(|candidate| {
        let weight = u64::from(candidate.weight);
        if draw < weight {
            Some(candidate.model.as_str())
        } else {
            draw -= weight;
            None
        }
    })
}

async fn classify(
    model: &model_contract::ModelClient,
    context: Ctx,
    classifier_model: &str,
    instruction: &str,
    input: &str,
    max_output_tokens: i64,
) -> Result<String, ()> {
    let stream = model
        .complete_with_context(
            context,
            CompleteOpen {
                model: classifier_model.to_owned(),
                reasoning_effort: None,
                reasoning_enabled: None,
                reasoning_budget_tokens: None,
                service_tier: None,
                messages: vec![
                    CompleteMessageInput {
                        role: CompleteMessageRole::System,
                        content: format!(
                            "{instruction}\n\nReturn exactly `default` or `strong` and nothing else."
                        ),
                        tool_call_id: None,
                        tool_name: None,
                        arguments_json: None,
                    },
                    CompleteMessageInput {
                        role: CompleteMessageRole::User,
                        content: input.to_owned(),
                        tool_call_id: None,
                        tool_name: None,
                        arguments_json: None,
                    },
                ],
                tools: Vec::new(),
                temperature: 0.0,
                max_output_tokens,
            },
        )
        .await
        .map_err(|_| ())?;
    stream.close_send().await.map_err(|_| ())?;
    let mut output = String::new();
    loop {
        match stream.receive().await.map_err(|_| ())? {
            ModelCompleteEvent::Message(message) => match message.kind {
                CompleteMessageKind::TextDelta => {
                    if output
                        .chars()
                        .count()
                        .saturating_add(message.text.chars().count())
                        > MAX_CLASSIFIER_RESPONSE_CHARACTERS
                    {
                        return Err(());
                    }
                    output.push_str(&message.text);
                }
                CompleteMessageKind::ReasoningSummaryDelta | CompleteMessageKind::Usage => {}
                CompleteMessageKind::ToolCall => return Err(()),
            },
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => return Ok(output.trim().to_lowercase()),
            StreamEvent::Terminal(Err(_)) => return Err(()),
        }
    }
}

fn selected_response(
    admitted: &BTreeSet<&str>,
    model: &str,
    strategy: &str,
    reason_code: &str,
) -> PluginResult<SelectResponse, SelectError> {
    if !admitted.contains(model) {
        return Err(PluginError::domain(SelectError::NoCandidate));
    }
    Ok(SelectResponse {
        model: model.to_owned(),
        strategy: strategy.to_owned(),
        reason_code: reason_code.to_owned(),
    })
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
}

fn valid_model(value: &str) -> bool {
    value.trim() == value && !value.is_empty() && value.len() <= 256
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighted_selection_is_stable_for_one_selection() {
        let admitted = BTreeSet::from(["fast", "strong"]);
        let candidates = vec![
            WeightedCandidate {
                model: "fast".to_owned(),
                weight: 3,
            },
            WeightedCandidate {
                model: "strong".to_owned(),
                weight: 1,
            },
        ];
        let policies = SelectionConfig {
            policies: vec![PolicyConfig::WeightedRandom {
                id: "mixed".to_owned(),
                description: "Mix two models".to_owned(),
                candidates: candidates.clone(),
            }],
        };
        validate_config(&policies).unwrap();
        let first = weighted_model(&candidates, &admitted, "turn-1", "mixed").unwrap();
        let retry = weighted_model(&candidates, &admitted, "turn-1", "mixed").unwrap();
        assert_eq!(first, retry);
    }

    #[test]
    fn rejects_duplicate_policy_ids() {
        let policy = PolicyConfig::Rules {
            id: "auto".to_owned(),
            description: "Select by rules".to_owned(),
            default_model: "fast".to_owned(),
            strong_model: "strong".to_owned(),
            min_input_characters: 100,
            strong_keywords: Vec::new(),
        };
        assert!(
            validate_config(&SelectionConfig {
                policies: vec![policy.clone(), policy],
            })
            .is_err()
        );
    }
}
