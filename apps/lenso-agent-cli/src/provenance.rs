use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use lenso_agent_loop_module::inspect_turn_generation_provenance;
use lenso_agent_session_file_module::{
    inspect_all_turn_started_events, inspect_turn_started_events,
};
use lenso_plugin_control_plane::{AppGenerationSpec, CanonicalDocument};

use crate::plugins::retained_plugin_set_digests;

const GENERATION_DIRECTORY: &str = "generations";
const APP_ID: &str = "lenso.agent.harness";

#[derive(Debug)]
pub enum GenerationCommand {
    Inspect { digest: String, root: PathBuf },
    GcPreview { root: PathBuf, sessions: PathBuf },
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
    let mut root = PathBuf::from(".lenso/plugins");
    let mut sessions = PathBuf::from(".lenso/sessions");
    let mut digest = None;
    let mut arguments = rest.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--digest" if command == "inspect" => {
                digest = Some(arguments.next().ok_or_else(generation_usage)?.clone());
            }
            "--root" => root = PathBuf::from(arguments.next().ok_or_else(generation_usage)?),
            "--sessions" if command == "gc-preview" || command == "gc-plan" => {
                sessions = PathBuf::from(arguments.next().ok_or_else(generation_usage)?);
            }
            _ => return Err(generation_usage()),
        }
    }
    match command.as_str() {
        "inspect" => Ok(GenerationCommand::Inspect {
            digest: digest.ok_or_else(generation_usage)?,
            root,
        }),
        "gc-preview" | "gc-plan" => Ok(GenerationCommand::GcPreview { root, sessions }),
        _ => Err(generation_usage()),
    }
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
        GenerationCommand::GcPreview { root, sessions } => print_gc_preview(&root, &sessions),
    }
}

fn print_gc_preview(root: &Path, sessions: &Path) -> Result<(), String> {
    let plugin_sets = retained_plugin_set_digests(root)?;
    let session_generations = inspect_all_turn_started_events(sessions)?
        .into_iter()
        .map(|stored| {
            inspect_turn_generation_provenance(
                stored.event.revision,
                stored.event.turn_id.as_deref(),
                &stored.event.payload_json,
            )
            .map(|turn| turn.generation_spec_digest)
            .map_err(|error| {
                format!(
                    "Session `{}` has invalid Turn Generation provenance: {error}",
                    stored.session_id
                )
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let generations = load_all_generations(root)?;
    for digest in &session_generations {
        if !generations.contains_key(digest) {
            return Err(format!(
                "Session provenance references missing Generation Spec `{digest}`"
            ));
        }
    }

    let mut protected = 0;
    let mut candidates = 0;
    for (digest, generation) in generations {
        let mut reasons = Vec::new();
        if plugin_sets.contains(&generation.value().plugin_set_lock_digest) {
            reasons.push("plugin-set");
        }
        if session_generations.contains(&digest) {
            reasons.push("session");
        }
        if reasons.is_empty() {
            candidates += 1;
            println!("candidate: {digest}");
        } else {
            protected += 1;
            println!("protected: {digest} reason={}", reasons.join(","));
        }
    }
    println!("summary: protected={protected} candidates={candidates}");
    Ok(())
}

fn load_all_generations(
    root: &Path,
) -> Result<BTreeMap<String, CanonicalDocument<AppGenerationSpec>>, String> {
    let directory = root.join(GENERATION_DIRECTORY);
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|error| format!("failed to inspect Generation provenance directory: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Generation provenance path is not a regular directory".to_owned());
    }
    let mut entries = fs::read_dir(&directory)
        .map_err(|error| format!("failed to enumerate Generation Specs: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to enumerate Generation Specs: {error}"))?;
    entries.sort_by_key(fs::DirEntry::file_name);

    let mut generations = BTreeMap::new();
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Generation directory contains a non-UTF-8 name".to_owned())?;
        if name.starts_with('.')
            && Path::new(&name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"))
        {
            continue;
        }
        let hash = name
            .strip_suffix(".json")
            .filter(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or_else(|| format!("Generation entry `{name}` is not content-addressed"))?;
        let digest = format!("sha256:{hash}");
        generations.insert(digest.clone(), load_generation(root, &digest)?);
    }
    Ok(generations)
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
    "usage: lenso-agent-cli generations <inspect --digest <sha256:digest> [--root <plugin-root>]|gc-preview [--root <plugin-root>] [--sessions <session-directory>]>".to_owned()
}

fn session_usage() -> String {
    "usage: lenso-agent-cli sessions provenance --session <id> [--directory <session-directory>] [--plugin-root <plugin-root>]".to_owned()
}
