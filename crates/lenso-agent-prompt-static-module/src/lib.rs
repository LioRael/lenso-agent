//! Static Prompt and Skill contribution Module.

use std::{collections::BTreeSet, rc::Rc};

use futures::future::ready;
use lenso_capability_agent_prompt_provider::{
    ContributeRequest, ContributeResponse, ContributeResponseContributionsItem,
    ContributeResponseContributionsItemKind, PromptProviderEndpoint, PromptProviderProvider,
};
use lenso_kernel::{InvocationContext, NativeRequestEndpoint, RuntimeFailure};
use lenso_native_adapter::{NativeModuleFactoryContext, NativeModuleInstance};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticPromptConfig {
    contributions: Vec<StaticContribution>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticContribution {
    id: String,
    version: String,
    kind: StaticContributionKind,
    content: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum StaticContributionKind {
    Instruction,
    Skill,
}

/// Instantiates one explicitly configured Prompt contribution Instance.
#[lenso_native_adapter::module(
    descriptor = r#"{"provided_capabilities":[{"capability_id":"lenso.agent.prompt-provider@1","descriptor_version":"1.0.0","operations":["contribute"],"operation_kinds":{},"default_admission":{"queue_capacity":1,"max_concurrency":1},"operation_admissions":{},"event_admission":null,"cross_lane_transfer":false}],"required_capabilities":[]}"#,
    configuration_schema = "config.schema.json"
)]
fn instantiate(
    context: NativeModuleFactoryContext<'_>,
) -> Result<NativeModuleInstance, RuntimeFailure> {
    if context.entrypoint() != "default" {
        return Err(invalid_plan("unsupported static Prompt entrypoint"));
    }
    let config = serde_json::from_str::<StaticPromptConfig>(context.configuration())
        .map_err(|error| invalid_plan(format!("invalid static Prompt configuration: {error}")))?;
    let contributions = validate_and_convert(config)?;
    let endpoint = Rc::new(PromptProviderEndpoint::new(StaticPrompt { contributions }))
        as Rc<dyn NativeRequestEndpoint>;
    Ok(NativeModuleInstance::new(vec![endpoint]))
}

#[derive(Clone, Debug)]
struct StaticPrompt {
    contributions: Vec<ContributeResponseContributionsItem>,
}

impl PromptProviderProvider for StaticPrompt {
    fn contribute(
        &self,
        _context: InvocationContext,
        _request: ContributeRequest,
    ) -> lenso_kernel::NativeRequestFuture<lenso_capability_agent_prompt_provider::PromptProvider>
    {
        Box::pin(ready(Ok(Ok(ContributeResponse {
            contributions: self.contributions.clone(),
        }))))
    }
}

fn validate_and_convert(
    config: StaticPromptConfig,
) -> Result<Vec<ContributeResponseContributionsItem>, RuntimeFailure> {
    if config.contributions.len() > 64 {
        return Err(invalid_plan("static Prompt exceeds 64 contributions"));
    }
    let mut ids = BTreeSet::new();
    config
        .contributions
        .into_iter()
        .map(|contribution| {
            if !valid_id(&contribution.id)
                || contribution.version.trim().is_empty()
                || contribution.version.len() > 64
                || contribution.content.trim().is_empty()
                || contribution.content.len() > 65_536
                || !ids.insert(contribution.id.clone())
            {
                return Err(invalid_plan(format!(
                    "invalid or duplicate static Prompt contribution `{}`",
                    contribution.id
                )));
            }
            Ok(ContributeResponseContributionsItem {
                id: contribution.id,
                version: contribution.version,
                kind: match contribution.kind {
                    StaticContributionKind::Instruction => {
                        ContributeResponseContributionsItemKind::Instruction
                    }
                    StaticContributionKind::Skill => ContributeResponseContributionsItemKind::Skill,
                },
                content: contribution.content,
            })
        })
        .collect()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'/' | b'-'))
        })
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
    fn rejects_duplicate_contribution_ids() {
        let contribution = StaticContribution {
            id: "review.rust".to_owned(),
            version: "1.0.0".to_owned(),
            kind: StaticContributionKind::Skill,
            content: "Review Rust code.".to_owned(),
        };
        let error = validate_and_convert(StaticPromptConfig {
            contributions: vec![contribution.clone(), contribution],
        })
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }
}
