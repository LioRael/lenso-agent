use std::sync::atomic::{AtomicBool, Ordering};

use external_task_board as task_board;
use external_task_board_audit as audit;
use external_task_board_report as report;
use lenso::prelude::*;

static STOPPED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, serde::Deserialize, PluginConfig)]
#[serde(deny_unknown_fields)]
struct ReportConfig {
    label: String,
}

#[lenso::plugin(lifecycle, validate = validate_config)]
#[derive(Clone, Debug)]
struct TaskBoardReport {
    #[config]
    config: ReportConfig,
    task_board: Port<task_board::TaskBoardClient>,
}

fn validate_config(config: &ReportConfig) -> Result<(), RuntimeFailure> {
    if config.label.trim().is_empty() || config.label.len() > 64 {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Task Board report label must be a non-empty bounded string".to_owned(),
        });
    }
    Ok(())
}

impl Lifecycle for TaskBoardReport {
    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        STOPPED.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[lenso::provides(report::TaskBoardReport, audit::TaskBoardAudit)]
impl TaskBoardReport {
    async fn execute(
        &self,
        context: Ctx,
        request: report::ExecuteRequest,
    ) -> PluginResult<report::ExecuteResponse, report::ExecuteError> {
        let response = self
            .task_board
            .execute_with_context(context, task_board::ExecuteRequest { text: request.text })
            .await
            .map_err(map_task_board_error)?;
        Ok(report::ExecuteResponse {
            text: format!("{}: {}", self.config.label, response.text),
        })
    }

    async fn audit(
        &self,
        _context: Ctx,
        _request: audit::AuditRequest,
    ) -> PluginResult<audit::AuditResponse, audit::AuditError> {
        Ok(audit::AuditResponse {
            status: format!("{}: ready", self.config.label),
        })
    }
}

fn map_task_board_error(
    error: task_board::TaskBoardInvocationError,
) -> PluginError<report::ExecuteError> {
    match error {
        task_board::TaskBoardInvocationError::Domain(_) => {
            PluginError::domain(report::ExecuteError::Unavailable)
        }
        task_board::TaskBoardInvocationError::Runtime(error) => PluginError::runtime(error),
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
