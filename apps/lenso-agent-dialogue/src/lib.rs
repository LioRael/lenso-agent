//! Execution support for the explicit deterministic dialogue starter.

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use lenso_agent_host::{AgentHost, HeadlessSurface, Profile};
use lenso_capability_agent::{RUN_TURN_OPERATION, RunTurnRequest};
use lenso_kernel::StreamEvent;

/// Runs one deterministic dialogue Turn through the explicit starter Plan.
///
/// The returned text comes from the fixture Model. No Coding, workspace,
/// network, or Tool Provider Plugin is included in the selected Plan.
pub async fn run_dialogue(home: &Path, input: String) -> Result<DialogueResult, String> {
    let plan_path = write_starter_plan(home)?;
    let host = AgentHost::builder()
        .agent_home(home)?
        .plugins(lenso_agent_dialogue_starter::link)
        .surface(HeadlessSurface::stdio())
        .build()
        .map_err(|error| format!("Dialogue starter Host composition failed: {error}"))?;
    let mut app = host.run(Profile::resolved_plan(plan_path)).await?;
    let lease = app.lease_turn().await?;
    let result = receive_turn(&lease, input).await;
    drop(lease);
    let shutdown = app.shutdown().await;
    match (result, shutdown) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

/// Returns the exact Plan path generated for one explicit dialogue run.
pub fn write_starter_plan(home: &Path) -> Result<PathBuf, String> {
    ensure_absolute_directory(home)?;
    let plan = lenso_agent_dialogue_starter::starter_plan(home)?;
    let bytes = serde_json::to_vec_pretty(&plan)
        .map_err(|error| format!("failed to encode dialogue starter Plan: {error}"))?;
    let runtime = home.join("runtime");
    fs::create_dir_all(&runtime).map_err(|error| {
        format!(
            "failed to create dialogue runtime {}: {error}",
            runtime.display()
        )
    })?;
    let path = runtime.join("dialogue-starter.plan.json");
    let temporary = runtime.join(format!("dialogue-starter.{}.tmp", std::process::id()));
    fs::write(&temporary, bytes)
        .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("failed to publish {}: {error}", path.display()))?;
    Ok(path)
}

/// Text and Session evidence produced by one completed dialogue Turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogueResult {
    /// Concatenated text deltas returned by the selected fixture Model.
    pub text: String,
    /// Session created or continued by the selected Session Plugin.
    pub session_id: Option<String>,
}

async fn receive_turn(
    lease: &lenso_agent_host::generation::TurnGeneration,
    input: String,
) -> Result<DialogueResult, String> {
    let stream = lease
        .handle()
        .open_with_context(
            RUN_TURN_OPERATION,
            lease.invocation_context()?,
            RunTurnRequest {
                attachments: None,
                input,
                session_id: None,
            },
        )
        .await
        .map_err(|error| format!("Dialogue Agent stream failed to open: {error:?}"))?
        .map_err(|error| format!("Dialogue Agent rejected the Turn: {error:?}"))?;
    stream
        .close_send()
        .await
        .map_err(|error| format!("Dialogue Agent failed to half-close input: {error:?}"))?;

    let mut text = String::new();
    let mut session_id = None;
    loop {
        match stream
            .receive()
            .await
            .map_err(|error| format!("Dialogue Agent stream failed: {error:?}"))?
        {
            StreamEvent::Message(message) => {
                if message.is_text_delta() {
                    text.push_str(&message.text);
                }
                session_id = message.session_id.or(session_id);
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => return Ok(DialogueResult { text, session_id }),
            StreamEvent::Terminal(Err(error)) => {
                return Err(format!("Dialogue Agent Turn failed: {error:?}"));
            }
        }
    }
}

fn ensure_absolute_directory(home: &Path) -> Result<(), String> {
    if !home.is_absolute() {
        return Err(format!(
            "Dialogue starter home must be absolute: {}",
            home.display()
        ));
    }
    match fs::symlink_metadata(home) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(format!(
            "Dialogue starter home must be a directory: {}",
            home.display()
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir_all(home)
            .map_err(|error| format!("failed to create dialogue home {}: {error}", home.display())),
        Err(error) => Err(format!(
            "failed to inspect dialogue home {}: {error}",
            home.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{run_dialogue, write_starter_plan};

    #[tokio::test(flavor = "current_thread")]
    async fn starter_runs_a_fixture_turn_without_publishing_legacy_host_defaults() {
        let temporary = tempfile::tempdir().expect("temporary dialogue home");
        tokio::task::LocalSet::new()
            .run_until(Box::pin(async {
                let plan = write_starter_plan(temporary.path()).expect("write starter Plan");
                assert!(plan.is_file());
                assert!(!temporary.path().join(".lenso/host-catalog.json").exists());

                let result = run_dialogue(
                    temporary.path(),
                    "Answer directly: selected dialogue starter".to_owned(),
                )
                .await
                .expect("run fixture dialogue");
                assert!(!result.text.is_empty());
                assert!(result.session_id.is_some());
                assert!(!temporary.path().join(".lenso/host-catalog.json").exists());
            }))
            .await;
    }
}
