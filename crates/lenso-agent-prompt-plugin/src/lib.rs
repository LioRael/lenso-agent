//! Deterministic Prompt contribution aggregate Plugin.

use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

use lenso::prelude::*;
use lenso_capability_agent_prompt::{
    self as prompt_contract, AssembleRequest, AssembleResponse, AssembleResponseContributionsItem,
    AssembleResponseContributionsItemKind, PromptProvider,
};
use lenso_capability_agent_prompt_provider as provider_contract;
use lenso_kernel::{InvocationContext, RuntimeFailure};
use sha2::{Digest, Sha256};

const BASE_INSTRUCTION_ID: &str = "harness.base";
const BASE_INSTRUCTION_VERSION: &str = "1.0.1";
const BASE_INSTRUCTION: &str = "You are Lenso Agent. Follow explicit user instructions and use only the capabilities supplied by the current App Profile.";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptConfig {
    max_contributions: usize,
    max_total_bytes: usize,
}

fn validate_config(config: &PromptConfig) -> Result<(), RuntimeFailure> {
    if !(1..=256).contains(&config.max_contributions)
        || !(1..=262_144).contains(&config.max_total_bytes)
    {
        return Err(invalid_plan("Prompt aggregate limits are invalid"));
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    configuration_defaults = "config.defaults.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct PromptPlugin {
    #[config]
    config: PromptConfig,
    providers: ManyPort<provider_contract::PromptProviderClient>,
    state: Rc<RefCell<Option<AssembleResponse>>>,
}

#[lenso::provides(prompt_contract::Prompt)]
impl PromptProvider for PromptPlugin {
    fn assemble(
        &self,
        _context: InvocationContext,
        _request: AssembleRequest,
    ) -> lenso_kernel::NativeRequestFuture<lenso_capability_agent_prompt::Prompt> {
        let result = self
            .state
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::Unavailable {
                capability: lenso_capability_agent_prompt::CAPABILITY_ID,
            });
        Box::pin(futures::future::ready(result.map(Ok)))
    }
}

impl Lifecycle for PromptPlugin {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let mut provider_contributions = Vec::with_capacity(self.providers.len());
        for (index, provider) in self.providers.iter().enumerate() {
            let response = provider
                .contribute(provider_contract::ContributeRequest {})
                .await
                .map_err(|error| match error {
                    provider_contract::PromptProviderInvocationError::Domain(error) => {
                        RuntimeFailure::PluginFailure {
                            detail: format!(
                                "Prompt Provider {index} rejected its contributions: {error:?}"
                            ),
                        }
                    }
                    provider_contract::PromptProviderInvocationError::Runtime(error) => error,
                })?;
            provider_contributions.push(response.contributions);
        }
        self.state.replace(Some(assemble_contributions(
            provider_contributions,
            &self.config,
        )?));
        Ok(())
    }
}

fn assemble_contributions(
    providers: Vec<Vec<provider_contract::ContributeResponseContributionsItem>>,
    config: &PromptConfig,
) -> Result<AssembleResponse, RuntimeFailure> {
    let mut ids = BTreeSet::new();
    ids.insert(BASE_INSTRUCTION_ID.to_owned());
    let mut contents = vec![BASE_INSTRUCTION.to_owned()];
    let mut manifest = vec![AssembleResponseContributionsItem {
        id: BASE_INSTRUCTION_ID.to_owned(),
        version: BASE_INSTRUCTION_VERSION.to_owned(),
        kind: AssembleResponseContributionsItemKind::Instruction,
        digest: format!("{:x}", Sha256::digest(BASE_INSTRUCTION.as_bytes())),
    }];
    let mut total_bytes = BASE_INSTRUCTION.len();
    if manifest.len() > config.max_contributions || total_bytes > config.max_total_bytes {
        return Err(invalid_plan(
            "Prompt aggregate limits do not admit the required base instruction",
        ));
    }
    for contribution in providers.into_iter().flatten() {
        if manifest.len() == config.max_contributions {
            return Err(invalid_plan("Prompt contribution count limit exceeded"));
        }
        if !ids.insert(contribution.id.clone()) {
            return Err(invalid_plan(format!(
                "duplicate Prompt contribution id `{}`",
                contribution.id
            )));
        }
        let separator_bytes = usize::from(!contents.is_empty()) * 2;
        total_bytes = total_bytes
            .saturating_add(separator_bytes)
            .saturating_add(contribution.content.len());
        if total_bytes > config.max_total_bytes {
            return Err(invalid_plan("Prompt total byte limit exceeded"));
        }
        manifest.push(AssembleResponseContributionsItem {
            id: contribution.id,
            version: contribution.version,
            kind: match contribution.kind {
                provider_contract::ContributeResponseContributionsItemKind::Instruction => {
                    AssembleResponseContributionsItemKind::Instruction
                }
                provider_contract::ContributeResponseContributionsItemKind::Skill => {
                    AssembleResponseContributionsItemKind::Skill
                }
            },
            digest: format!("{:x}", Sha256::digest(contribution.content.as_bytes())),
        });
        contents.push(contribution.content);
    }
    Ok(AssembleResponse {
        content: contents.join("\n\n"),
        contributions: manifest,
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
    use provider_contract::ContributeResponseContributionsItemKind;

    fn contribution(
        id: &str,
        content: &str,
    ) -> provider_contract::ContributeResponseContributionsItem {
        provider_contract::ContributeResponseContributionsItem {
            id: id.to_owned(),
            version: "1.0.0".to_owned(),
            kind: ContributeResponseContributionsItemKind::Skill,
            content: content.to_owned(),
        }
    }

    #[test]
    fn preserves_provider_and_local_order_with_digests() {
        let response = assemble_contributions(
            vec![
                vec![contribution("first", "First")],
                vec![contribution("second", "Second")],
            ],
            &PromptConfig {
                max_contributions: 4,
                max_total_bytes: 512,
            },
        )
        .unwrap();
        assert_eq!(
            response.content,
            format!("{BASE_INSTRUCTION}\n\nFirst\n\nSecond")
        );
        assert_eq!(response.contributions[0].id, BASE_INSTRUCTION_ID);
        assert_eq!(response.contributions[1].id, "first");
        assert_eq!(response.contributions[2].id, "second");
        assert_eq!(response.contributions[0].digest.len(), 64);
    }

    #[test]
    fn rejects_cross_provider_id_collisions() {
        let error = assemble_contributions(
            vec![
                vec![contribution("same", "One")],
                vec![contribution("same", "Two")],
            ],
            &PromptConfig {
                max_contributions: 4,
                max_total_bytes: 512,
            },
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }

    #[test]
    fn rejects_provider_collision_with_required_base_instruction() {
        let error = assemble_contributions(
            vec![vec![contribution(BASE_INSTRUCTION_ID, "Override")]],
            &PromptConfig {
                max_contributions: 4,
                max_total_bytes: 512,
            },
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }

    #[test]
    fn aggregate_byte_limit_includes_separators() {
        let error = assemble_contributions(
            vec![
                vec![contribution("first", "12345")],
                vec![contribution("second", "6789")],
            ],
            &PromptConfig {
                max_contributions: 4,
                max_total_bytes: BASE_INSTRUCTION.len() + 10,
            },
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }
}
