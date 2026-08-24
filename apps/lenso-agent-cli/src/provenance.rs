use std::{fs, path::PathBuf};

use lenso_agent_loop_module::inspect_turn_generation_provenance;
use lenso_agent_session_file_module::inspect_turn_started_events;
use lenso_plugin_control_plane::{AppGenerationSpec, CanonicalDocument};

const GENERATION_DIRECTORY: &str = "generations";
const APP_ID: &str = "lenso.agent.harness";

#[derive(Debug)]
pub enum GenerationCommand {
    Inspect { digest: String, root: PathBuf },
}

#[derive(Debug)]
pub enum SessionCommand {
    Provenance {
        session_id: String,
        directory: PathBuf,
        plugin_root: PathBuf,
    },
}

pub fn parse_generation_command(arguments: &[String]) -> Result<GenerationCommand, String> {
    let [command, rest @ ..] = arguments else {
        return Err(generation_usage());
    };
    if command != "inspect" {
        return Err(generation_usage());
    }
    let mut digest = None;
    let mut root = PathBuf::from(".lenso/plugins");
    let mut arguments = rest.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--digest" => digest = Some(arguments.next().ok_or_else(generation_usage)?.clone()),
            "--root" => root = PathBuf::from(arguments.next().ok_or_else(generation_usage)?),
            _ => return Err(generation_usage()),
        }
    }
    Ok(GenerationCommand::Inspect {
        digest: digest.ok_or_else(generation_usage)?,
        root,
    })
}

pub fn parse_session_command(arguments: &[String]) -> Result<SessionCommand, String> {
    let [command, rest @ ..] = arguments else {
        return Err(session_usage());
    };
    if command != "provenance" {
        return Err(session_usage());
    }
    let mut session_id = None;
    let mut directory = PathBuf::from(".lenso/sessions");
    let mut plugin_root = PathBuf::from(".lenso/plugins");
    let mut arguments = rest.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--session" => session_id = Some(arguments.next().ok_or_else(session_usage)?.clone()),
            "--directory" => directory = PathBuf::from(arguments.next().ok_or_else(session_usage)?),
            "--plugin-root" => {
                plugin_root = PathBuf::from(arguments.next().ok_or_else(session_usage)?);
            }
            _ => return Err(session_usage()),
        }
    }
    Ok(SessionCommand::Provenance {
        session_id: session_id.ok_or_else(session_usage)?,
        directory,
        plugin_root,
    })
}

pub fn run_generation(command: GenerationCommand) -> Result<(), String> {
    match command {
        GenerationCommand::Inspect { digest, root } => {
            let generation = load_generation(&root, &digest)?;
            println!("generation: {}", generation.digest());
            println!("app: {}", generation.value().app_id);
            println!(
                "host-build: {}",
                generation.value().host_build_manifest_digest
            );
            println!(
                "execution-policy: {}",
                generation.value().host_execution_policy_digest
            );
            println!("plan: {}", generation.value().resolved_plan_digest);
            println!("plugin-set: {}", generation.value().plugin_set_lock_digest);
            println!(
                "artifact-set: {}",
                generation.value().resolved_artifact_set_digest
            );
            println!(
                "grant-set: {}",
                generation.value().effective_host_grant_set_digest
            );
            Ok(())
        }
    }
}

pub fn run_session(command: SessionCommand) -> Result<(), String> {
    match command {
        SessionCommand::Provenance {
            session_id,
            directory,
            plugin_root,
        } => {
            let events = inspect_turn_started_events(&directory, &session_id)?;
            let provenance = events
                .iter()
                .map(|event| {
                    inspect_turn_generation_provenance(
                        event.revision,
                        event.turn_id.as_deref(),
                        &event.payload_json,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            println!("session: {session_id}");
            if provenance.is_empty() {
                println!("No Turn Generation provenance.");
            }
            for turn in provenance {
                let status = generation_status(&plugin_root, &turn.generation_spec_digest);
                println!(
                    "turn: {} revision={} generation={} spec={status}",
                    turn.turn_id, turn.revision, turn.generation_spec_digest
                );
            }
            Ok(())
        }
    }
}

fn load_generation(
    root: &std::path::Path,
    digest: &str,
) -> Result<CanonicalDocument<AppGenerationSpec>, String> {
    let hash = canonical_digest_hash(digest)?;
    let directory = root.join(GENERATION_DIRECTORY);
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|error| format!("failed to inspect Generation provenance directory: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Generation provenance path is not a regular directory".to_owned());
    }
    let path = directory.join(format!("{hash}.json"));
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("failed to inspect Generation Spec: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Generation Spec is not a regular file".to_owned());
    }
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read Generation Spec: {error}"))?;
    let generation = CanonicalDocument::<AppGenerationSpec>::parse("lenso-generation.json", &bytes)
        .map_err(|error| format!("Generation Spec validation failed: {error}"))?;
    if generation.digest() != digest {
        return Err("Generation Spec does not match its requested digest".to_owned());
    }
    if generation.value().app_id != APP_ID {
        return Err("Generation Spec belongs to another App".to_owned());
    }
    Ok(generation)
}

fn generation_status(root: &std::path::Path, digest: &str) -> &'static str {
    let Ok(hash) = canonical_digest_hash(digest) else {
        return "invalid";
    };
    let path = root.join(GENERATION_DIRECTORY).join(format!("{hash}.json"));
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
        _ if load_generation(root, digest).is_ok() => "available",
        _ => "invalid",
    }
}

fn canonical_digest_hash(digest: &str) -> Result<&str, String> {
    digest
        .strip_prefix("sha256:")
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| "digest is not canonical SHA-256".to_owned())
}

fn generation_usage() -> String {
    "usage: lenso-agent-cli generations inspect --digest <sha256:digest> [--root <plugin-root>]"
        .to_owned()
}

fn session_usage() -> String {
    "usage: lenso-agent-cli sessions provenance --session <id> [--directory <session-directory>] [--plugin-root <plugin-root>]".to_owned()
}
