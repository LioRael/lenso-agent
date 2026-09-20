use std::sync::atomic::{AtomicBool, Ordering};

use external_task_board as task_board;
use lenso::prelude::*;

static STOPPED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, serde::Deserialize, PluginConfig)]
#[serde(deny_unknown_fields)]
struct TaskBoardConfig {
    prefix: String,
}

#[lenso::plugin(lifecycle, validate = validate_config)]
#[derive(Clone, Debug)]
struct TaskBoardProvider {
    #[config]
    config: TaskBoardConfig,
}

fn validate_config(config: &TaskBoardConfig) -> Result<(), RuntimeFailure> {
    if config.prefix.trim().is_empty() || config.prefix.len() > 64 {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Task Board prefix must be a non-empty bounded string".to_owned(),
        });
    }
    Ok(())
}

impl Lifecycle for TaskBoardProvider {
    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        STOPPED.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[lenso::provides(task_board::TaskBoard)]
impl TaskBoardProvider {
    async fn execute(
        &self,
        context: Ctx,
        request: task_board::ExecuteRequest,
    ) -> PluginResult<task_board::ExecuteResponse, task_board::ExecuteError> {
        if context.is_cancelled() {
            return Err(PluginError::runtime(RuntimeFailure::Cancelled {
                request_id: context.request_id(),
            }));
        }
        if request.text == "reject" {
            return Err(PluginError::domain(task_board::ExecuteError::Unavailable));
        }
        Ok(task_board::ExecuteResponse {
            text: format!("{}: {}", self.config.prefix, request.text),
        })
    }
}

pub fn link() {
    link_plugin();
}

pub fn reset_lifecycle_observation() {
    STOPPED.store(false, Ordering::SeqCst);
}

pub fn was_stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}
