//! Deterministic Prompt contribution aggregate Module.

use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

use lenso_capability_agent_prompt::{
    AssembleRequest, AssembleResponse, AssembleResponseContributionsItem,
    AssembleResponseContributionsItemKind, PromptEndpoint, PromptProvider,
};
use lenso_capability_agent_prompt_provider as provider_contract;
use lenso_kernel::{
    ActivateContext, InvocationContext, ModuleFuture, ModuleLifecycle, NativeRequestEndpoint,
    RuntimeFailure,
};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};
use sha2::{Digest, Sha256};

/// Runtime package identity selected by App Composition.
pub const PACKAGE_ID: &str = "lenso.agent.prompt";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptConfig {
    max_contributions: usize,
    max_total_bytes: usize,
}

/// Native factory for deterministic Prompt aggregation.
#[derive(Clone, Debug, Default)]
pub struct PromptFactory;

impl NativeModuleFactory for PromptFactory {
    fn package_id(&self) -> &'static str {
        PACKAGE_ID
    }

    fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        if context.entrypoint() != "default" {
            return Err(invalid_plan("unsupported Prompt aggregate entrypoint"));
        }
        let config = serde_json::from_str::<PromptConfig>(context.configuration())
            .map_err(|error| invalid_plan(format!("invalid Prompt configuration: {error}")))?;
        if !(1..=256).contains(&config.max_contributions)
            || !(1..=262_144).contains(&config.max_total_bytes)
        {
            return Err(invalid_plan("Prompt aggregate limits are invalid"));
        }
        let state = Rc::new(RefCell::new(None));
        let endpoint = Rc::new(PromptEndpoint::new(AggregatePrompt {
            state: state.clone(),
        })) as Rc<dyn NativeRequestEndpoint>;
        Ok(NativeModuleInstance::with_lifecycle(
            vec![endpoint],
            PromptLifecycle { config, state },
        ))
    }
}

#[derive(Clone, Debug)]
struct AggregatePrompt {
    state: Rc<RefCell<Option<AssembleResponse>>>,
}

impl PromptProvider for AggregatePrompt {
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

#[derive(Debug)]
struct PromptLifecycle {
    config: PromptConfig,
    state: Rc<RefCell<Option<AssembleResponse>>>,
}

impl ModuleLifecycle for PromptLifecycle {
    fn activate(&self, context: ActivateContext) -> ModuleFuture {
        let handles = match context
            .dependencies()
            .many::<provider_contract::PromptProvider>()
        {
            Ok(handles) => handles,
            Err(error) => return Box::pin(futures::future::ready(Err(error))),
        };
        let config = self.config.clone();
        let state = self.state.clone();
        Box::pin(async move {
            let mut provider_contributions = Vec::with_capacity(handles.len());
            for (index, handle) in handles.into_iter().enumerate() {
                let response = handle
                    .invoke(
                        provider_contract::CONTRIBUTE_OPERATION,
                        provider_contract::ContributeRequest {},
                    )
                    .await?
                    .map_err(|error| RuntimeFailure::ModuleFailure {
                        detail: format!(
                            "Prompt Provider {index} rejected its contributions: {error:?}"
                        ),
                    })?;
                provider_contributions.push(response.contributions);
            }
            state.replace(Some(assemble_contributions(
                provider_contributions,
                &config,
            )?));
            Ok(())
        })
    }
}

fn assemble_contributions(
    providers: Vec<Vec<provider_contract::ContributeResponseContributionsItem>>,
    config: &PromptConfig,
) -> Result<AssembleResponse, RuntimeFailure> {
    let mut ids = BTreeSet::new();
    let mut contents = Vec::new();
    let mut manifest = Vec::new();
    let mut total_bytes = 0_usize;
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
                max_total_bytes: 64,
            },
        )
        .unwrap();
        assert_eq!(response.content, "First\n\nSecond");
        assert_eq!(response.contributions[0].id, "first");
        assert_eq!(response.contributions[1].id, "second");
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
                max_total_bytes: 64,
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
                max_total_bytes: 10,
            },
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }
}
