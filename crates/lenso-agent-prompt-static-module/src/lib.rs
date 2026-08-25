//! Static Prompt and Skill contribution Module.

use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

use futures::future::ready;
use lenso::prelude::*;
use lenso_capability_agent_prompt_provider::{
    self as prompt_provider_contract, ContributeRequest, ContributeResponse,
    ContributeResponseContributionsItem, ContributeResponseContributionsItemKind,
    PromptProviderProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};

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

fn validate_config(config: &StaticPromptConfig) -> Result<(), RuntimeFailure> {
    validate_and_convert(config.clone()).map(|_| ())
}

#[lenso::module(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct StaticPrompt {
    #[config]
    config: StaticPromptConfig,
    contributions: Rc<RefCell<Option<Vec<ContributeResponseContributionsItem>>>>,
}

impl Lifecycle for StaticPrompt {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        self.contributions
            .replace(Some(validate_and_convert(self.config.clone())?));
        Ok(())
    }
}

#[lenso::provides(prompt_provider_contract::PromptProvider)]
impl PromptProviderProvider for StaticPrompt {
    fn contribute(
        &self,
        _context: InvocationContext,
        _request: ContributeRequest,
    ) -> lenso_kernel::NativeRequestFuture<lenso_capability_agent_prompt_provider::PromptProvider>
    {
        let result = self
            .contributions
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::Unavailable {
                capability: prompt_provider_contract::CAPABILITY_ID,
            })
            .map(|contributions| ContributeResponse { contributions });
        Box::pin(ready(result.map(Ok)))
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
