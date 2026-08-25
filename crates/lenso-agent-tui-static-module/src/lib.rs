//! Static semantic panel contribution Module for the Agent TUI.

use futures::future::ready;
use lenso_capability_agent_tui_contribution::{
    self as tui_contract, SnapshotRequest, SnapshotResponse, SnapshotResponsePanelsItem,
    TuiContributionProvider, validate_snapshot_panels,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticTuiConfig {
    panels: Vec<StaticPanel>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticPanel {
    id: String,
    title: String,
    body: String,
}

fn validate_config(config: &StaticTuiConfig) -> Result<(), RuntimeFailure> {
    validate_panels(config).map(|_| ())
}

#[lenso::module(
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct StaticTui {
    #[config]
    config: StaticTuiConfig,
}

#[lenso::provides(tui_contract::TuiContribution)]
impl TuiContributionProvider for StaticTui {
    fn snapshot(
        &self,
        _context: InvocationContext,
        _request: SnapshotRequest,
    ) -> lenso_kernel::NativeRequestFuture<tui_contract::TuiContribution> {
        let response = validate_panels(&self.config).map(|panels| SnapshotResponse { panels });
        Box::pin(ready(response.map(Ok)))
    }
}

fn validate_panels(
    config: &StaticTuiConfig,
) -> Result<Vec<SnapshotResponsePanelsItem>, RuntimeFailure> {
    let panels = config
        .panels
        .iter()
        .map(|panel| {
            if panel.title.trim().is_empty() || panel.body.trim().is_empty() {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("TUI panel `{}` has empty content", panel.id),
                });
            }
            Ok(SnapshotResponsePanelsItem {
                id: panel.id.clone(),
                title: panel.title.clone(),
                body: panel.body.clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_snapshot_panels(&panels)
        .map_err(|detail| RuntimeFailure::InvalidResolvedPlan { detail })?;
    Ok(panels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_panel_ids() {
        let panel = StaticPanel {
            id: "agent.help".to_owned(),
            title: "Help".to_owned(),
            body: "Press Esc to exit.".to_owned(),
        };
        let error = validate_panels(&StaticTuiConfig {
            panels: vec![panel.clone(), panel],
        })
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }

    #[test]
    fn rejects_more_than_sixteen_panels() {
        let panels = (0..17)
            .map(|index| StaticPanel {
                id: format!("agent.panel-{index}"),
                title: format!("Panel {index}"),
                body: "Content".to_owned(),
            })
            .collect();
        let error = validate_panels(&StaticTuiConfig { panels }).unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }

    #[test]
    fn rejects_empty_panel_body() {
        let error = validate_panels(&StaticTuiConfig {
            panels: vec![StaticPanel {
                id: "agent.empty".to_owned(),
                title: "Empty".to_owned(),
                body: " \n".to_owned(),
            }],
        })
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }
}
