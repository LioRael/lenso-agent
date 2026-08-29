//! Presentation of online Generation changes without owning reconciliation.

use super::{AgentApp, OnlineGenerationEvent, TranscriptEntry, TuiState};

pub(super) async fn present_online_generation_events(app: &AgentApp, state: &mut TuiState) {
    for event in app.take_online_generation_events() {
        match event {
            OnlineGenerationEvent::Preparing { .. } => {
                state.push_system("Preparing plugin changes…".to_owned());
            }
            OnlineGenerationEvent::Switched { .. } => {
                state.advance_task_generation_epoch();
                match app.tui_panels().await {
                    Ok(panels) => state.replace_plugin_panels(panels),
                    Err(error) => state.transcript.push(TranscriptEntry::Error {
                        text: format!(
                            "Plugin changes were applied, but the interface could not refresh: {error}"
                        ),
                    }),
                }
                state.push_system("Plugin changes applied.".to_owned());
            }
            OnlineGenerationEvent::Rejected { detail, .. } => state.transcript.push(TranscriptEntry::Error {
                text: format!(
                    "Plugin changes were not loaded; the current plugins remain active: {detail}"
                ),
            }),
            OnlineGenerationEvent::RolledBack { detail, .. } => {
                state.advance_task_generation_epoch();
                match app.tui_panels().await {
                    Ok(panels) => state.replace_plugin_panels(panels),
                    Err(error) => state.transcript.push(TranscriptEntry::Error {
                        text: format!(
                            "The previous plugins were restored, but the interface could not refresh: {error}"
                        ),
                    }),
                }
                state.transcript.push(TranscriptEntry::Error {
                    text: format!(
                        "A plugin change failed and the previous plugins were restored: {detail}"
                    ),
                });
            }
            OnlineGenerationEvent::Failed { detail, .. } => {
                state.advance_task_generation_epoch();
                state.transcript.push(TranscriptEntry::Error {
                    text: format!(
                        "A plugin change failed and no working previous setup was available; new requests are paused: {detail}"
                    ),
                });
            }
            OnlineGenerationEvent::WatchDegraded { detail } => {
                state.transcript.push(TranscriptEntry::Error {
                    text: format!(
                        "Plugin folder watching encountered a problem; periodic scanning remains active: {detail}"
                    ),
                });
            }
        }
    }
}
