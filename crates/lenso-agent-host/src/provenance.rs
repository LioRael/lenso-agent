use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use lenso_agent_loop_plugin::inspect_turn_generation_provenance;
use lenso_agent_session_file_plugin::{FileSessionImporter, FileSessionInspector};
use lenso_agent_session_inspection::{
    SessionArchive, SessionImporter, SessionInspector, inspect_turn_started,
};
use lenso_agent_session_sqlite_plugin::{SqliteSessionImporter, SqliteSessionInspector};
use lenso_plugin_control_plane::{AppGenerationSpec, CanonicalDocument};

use crate::{
    AgentDirectories,
    authority::AuthorityCoordinator,
    generation::live_controller_generation_digests,
    generation_authority::{
        prune_recovery_generation_authorities_unfenced,
        recovery_generation_authority_gc_candidates_unfenced,
        retained_resolution_authority_digests, retained_resolution_authority_digests_unfenced,
    },
};

const GENERATION_DIRECTORY: &str = "generations";
const APP_ID: &str = "lenso.agent.harness";

#[derive(Debug)]
pub enum GenerationCommand {
    Inspect {
        digest: String,
        root: PathBuf,
    },
    GcPreview {
        root: PathBuf,
        sessions: SessionStore,
    },
    GcApply {
        root: PathBuf,
        sessions: SessionStore,
    },
}

#[derive(Clone, Debug)]
pub enum SessionStore {
    File(PathBuf),
    Sqlite(PathBuf),
}

#[derive(Debug)]
pub enum SessionCommand {
    Provenance {
        session_id: String,
        store: SessionStore,
        runtime_root: PathBuf,
    },
    Export {
        session_id: Option<String>,
        store: SessionStore,
        archive: PathBuf,
    },
    Import {
        archive: PathBuf,
        store: SessionStore,
    },
    Migrate {
        session_id: Option<String>,
        source: SessionStore,
        destination: SessionStore,
    },
}

pub fn parse_generation_command(arguments: &[String]) -> Result<GenerationCommand, String> {
    let [command, rest @ ..] = arguments else {
        return Err(generation_usage());
    };
    let directories = AgentDirectories::resolve()?;
    let mut root = directories.runtime();
    let mut sessions = directories.sessions();
    let mut session_database = Some(directories.session_database());
    let mut session_database_explicit = false;
    let mut sessions_explicit = false;
    let mut digest = None;
    let mut apply = false;
    let mut arguments = rest.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--digest" if command == "inspect" => {
                digest = Some(arguments.next().ok_or_else(generation_usage)?.clone());
            }
            "--root" => root = PathBuf::from(arguments.next().ok_or_else(generation_usage)?),
            "--sessions" if command == "gc-preview" || command == "gc-plan" => {
                sessions = PathBuf::from(arguments.next().ok_or_else(generation_usage)?);
                session_database = None;
                sessions_explicit = true;
            }
            "--sessions" if command == "gc" => {
                sessions = PathBuf::from(arguments.next().ok_or_else(generation_usage)?);
                session_database = None;
                sessions_explicit = true;
            }
            "--session-database"
                if command == "gc-preview" || command == "gc-plan" || command == "gc" =>
            {
                session_database = Some(PathBuf::from(
                    arguments.next().ok_or_else(generation_usage)?,
                ));
                session_database_explicit = true;
            }
            "--apply" if command == "gc" => apply = true,
            _ => return Err(generation_usage()),
        }
    }
    if sessions_explicit && session_database_explicit {
        return Err(generation_usage());
    }
    match command.as_str() {
        "inspect" => Ok(GenerationCommand::Inspect {
            digest: digest.ok_or_else(generation_usage)?,
            root,
        }),
        "gc-preview" | "gc-plan" => Ok(GenerationCommand::GcPreview {
            root,
            sessions: session_database.map_or(SessionStore::File(sessions), SessionStore::Sqlite),
        }),
        "gc" if apply => Ok(GenerationCommand::GcApply {
            root,
            sessions: session_database.map_or(SessionStore::File(sessions), SessionStore::Sqlite),
        }),
        _ => Err(generation_usage()),
    }
}

pub fn parse_session_command(arguments: &[String]) -> Result<SessionCommand, String> {
    let [command, rest @ ..] = arguments else {
        return Err(session_usage());
    };
    if command == "migrate" {
        return parse_session_migration(rest);
    }
    let mut session_id = None;
    let directories = AgentDirectories::resolve()?;
    let mut directory = directories.sessions();
    let mut database = Some(directories.session_database());
    let mut database_explicit = false;
    let mut directory_explicit = false;
    let mut runtime_root = directories.runtime();
    let mut runtime_root_explicit = false;
    let mut archive = None;
    let mut arguments = rest.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--session" => session_id = Some(arguments.next().ok_or_else(session_usage)?.clone()),
            "--directory" => {
                directory = PathBuf::from(arguments.next().ok_or_else(session_usage)?);
                database = None;
                directory_explicit = true;
            }
            "--database" => {
                database = Some(PathBuf::from(arguments.next().ok_or_else(session_usage)?));
                database_explicit = true;
            }
            "--runtime-root" => {
                runtime_root = PathBuf::from(arguments.next().ok_or_else(session_usage)?);
                runtime_root_explicit = true;
            }
            "--archive" => {
                archive = Some(PathBuf::from(arguments.next().ok_or_else(session_usage)?));
            }
            _ => return Err(session_usage()),
        }
    }
    if directory_explicit && database_explicit {
        return Err(session_usage());
    }
    let store = database.map_or(SessionStore::File(directory), SessionStore::Sqlite);
    match command.as_str() {
        "provenance" if archive.is_none() => Ok(SessionCommand::Provenance {
            session_id: session_id.ok_or_else(session_usage)?,
            store,
            runtime_root,
        }),
        "export" if archive.is_some() && !runtime_root_explicit => Ok(SessionCommand::Export {
            session_id,
            store,
            archive: archive.expect("checked"),
        }),
        "import" if archive.is_some() && session_id.is_none() && !runtime_root_explicit => {
            Ok(SessionCommand::Import {
                archive: archive.expect("checked"),
                store,
            })
        }
        _ => Err(session_usage()),
    }
}

fn parse_session_migration(arguments: &[String]) -> Result<SessionCommand, String> {
    let mut session_id = None;
    let mut source = None;
    let mut destination = None;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let value = arguments.next().ok_or_else(session_usage)?.clone();
        match argument.as_str() {
            "--session" => session_id = Some(value),
            "--from-directory" => {
                set_store(&mut source, SessionStore::File(PathBuf::from(value)))?;
            }
            "--from-database" => {
                set_store(&mut source, SessionStore::Sqlite(PathBuf::from(value)))?;
            }
            "--to-directory" => {
                set_store(&mut destination, SessionStore::File(PathBuf::from(value)))?;
            }
            "--to-database" => {
                set_store(&mut destination, SessionStore::Sqlite(PathBuf::from(value)))?;
            }
            _ => return Err(session_usage()),
        }
    }
    let source = source.ok_or_else(session_usage)?;
    let destination = destination.ok_or_else(session_usage)?;
    if same_store(&source, &destination) {
        return Err("Session migration source and destination must differ".to_owned());
    }
    Ok(SessionCommand::Migrate {
        session_id,
        source,
        destination,
    })
}

fn set_store(target: &mut Option<SessionStore>, value: SessionStore) -> Result<(), String> {
    if target.replace(value).is_some() {
        return Err(session_usage());
    }
    Ok(())
}

fn same_store(left: &SessionStore, right: &SessionStore) -> bool {
    match (left, right) {
        (SessionStore::File(left), SessionStore::File(right))
        | (SessionStore::Sqlite(left), SessionStore::Sqlite(right)) => left == right,
        _ => false,
    }
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
            println!(
                "resolution-authority: {}",
                generation.value().resolution_authority_digest
            );
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
        GenerationCommand::GcApply { root, sessions } => apply_gc(&root, &sessions),
    }
}

fn print_gc_preview(root: &Path, sessions: &SessionStore) -> Result<(), String> {
    let resolution_authorities = retained_resolution_authority_roots(root, false);
    print_gc_plan(&build_gc_plan(root, sessions, &resolution_authorities)?)
}

#[derive(Debug)]
struct GenerationGcPlan {
    generations: BTreeMap<String, CanonicalDocument<AppGenerationSpec>>,
    reasons: BTreeMap<String, Vec<&'static str>>,
}

impl GenerationGcPlan {
    fn candidates(&self) -> impl Iterator<Item = &String> {
        self.reasons
            .iter()
            .filter_map(|(digest, reasons)| reasons.is_empty().then_some(digest))
    }

    fn protected_resolution_authorities(&self) -> BTreeSet<String> {
        self.reasons
            .iter()
            .filter(|(_, reasons)| !reasons.is_empty())
            .filter_map(|(digest, _)| self.generations.get(digest))
            .map(|generation| generation.value().resolution_authority_digest.clone())
            .collect()
    }
}

fn build_gc_plan(
    root: &Path,
    sessions: &SessionStore,
    resolution_authorities: &BTreeSet<String>,
) -> Result<GenerationGcPlan, String> {
    let inspector = session_inspector(sessions);
    let session_generations = inspect_turn_started(inspector.as_ref(), None)?
        .into_iter()
        .map(|stored| {
            inspect_turn_generation_provenance(
                stored.revision,
                stored.turn_id.as_deref(),
                &stored.payload_json,
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
    let controller_generations = live_controller_generation_digests(root)?;
    let generations = load_all_generations(root)?;
    for (source, digests) in [
        ("Session provenance", &session_generations),
        ("Generation Controller", &controller_generations),
    ] {
        for digest in digests {
            if !generations.contains_key(digest) {
                return Err(format!(
                    "{source} references missing Generation Spec `{digest}`"
                ));
            }
        }
    }

    let reasons = generations
        .iter()
        .map(|(digest, generation)| {
            let mut reasons = Vec::new();
            if resolution_authorities.contains(&generation.value().resolution_authority_digest) {
                reasons.push("resolution-authority");
            }
            if controller_generations.contains(digest) {
                reasons.push("controller");
            }
            if session_generations.contains(digest) {
                reasons.push("session");
            }
            (digest.clone(), reasons)
        })
        .collect();
    Ok(GenerationGcPlan {
        generations,
        reasons,
    })
}

fn print_gc_plan(plan: &GenerationGcPlan) -> Result<(), String> {
    let mut protected = 0;
    let mut candidates = 0;
    for digest in plan.generations.keys() {
        let reasons = plan
            .reasons
            .get(digest)
            .ok_or_else(|| "Generation GC plan lost one classification".to_owned())?;
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

fn apply_gc(root: &Path, sessions: &SessionStore) -> Result<(), String> {
    let coordinator = AuthorityCoordinator::open_existing(root)?;
    let _gc_fence = coordinator.generation_gc_transition()?;
    let _authority_fence = coordinator.transition()?;
    let resolution_authorities = retained_resolution_authority_roots(root, true);
    let plan = build_gc_plan(root, sessions, &resolution_authorities)?;
    let candidates = plan.candidates().cloned().collect::<Vec<_>>();
    let mut retained_authorities = plan.protected_resolution_authorities();
    retained_authorities.extend(resolution_authorities);
    recovery_generation_authority_gc_candidates_unfenced(root, &retained_authorities);
    for digest in &candidates {
        remove_generation(root, digest)?;
        println!("removed-generation: {digest}");
    }
    let removed_authorities =
        prune_recovery_generation_authorities_unfenced(root, &retained_authorities);
    for digest in &removed_authorities {
        println!("removed-recovery-authority: {digest}");
    }
    println!(
        "summary: removed-generations={} removed-recovery-authorities={}",
        candidates.len(),
        removed_authorities.len()
    );
    Ok(())
}

fn retained_resolution_authority_roots(root: &Path, authority_fenced: bool) -> BTreeSet<String> {
    if authority_fenced {
        retained_resolution_authority_digests_unfenced(root)
    } else {
        retained_resolution_authority_digests(root)
    }
}

fn remove_generation(root: &Path, digest: &str) -> Result<(), String> {
    load_generation(root, digest)?;
    let hash = canonical_digest_hash(digest)?;
    fs::remove_file(root.join(GENERATION_DIRECTORY).join(format!("{hash}.json")))
        .map_err(|error| format!("failed to remove Generation Spec `{digest}`: {error}"))?;
    fs::File::open(root.join(GENERATION_DIRECTORY))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("failed to sync Generation provenance directory: {error}"))
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
            store,
            runtime_root,
        } => {
            let inspector = session_inspector(&store);
            let events = inspect_turn_started(inspector.as_ref(), Some(&session_id))?;
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
                let status = generation_status(&runtime_root, &turn.generation_spec_digest);
                println!(
                    "turn: {} revision={} generation={} spec={status}",
                    turn.turn_id, turn.revision, turn.generation_spec_digest
                );
            }
            Ok(())
        }
        SessionCommand::Export {
            session_id,
            store,
            archive,
        } => {
            let inspector = session_inspector(&store);
            let sessions = session_id.map_or_else(
                || inspector.inspect_all(),
                |session_id| {
                    inspector
                        .inspect_one(&session_id)
                        .map(|session| vec![session])
                },
            )?;
            let archive_document = SessionArchive::new(sessions)?;
            let bytes = archive_document.to_pretty_json()?;
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&archive)
                .map_err(|error| format!("failed to create Session archive: {error}"))?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|error| format!("failed to persist Session archive: {error}"))?;
            println!(
                "exported: {} sessions to {}",
                archive_document.sessions.len(),
                archive.display()
            );
            Ok(())
        }
        SessionCommand::Import { archive, store } => {
            let document = read_session_archive(&archive)?;
            session_importer(&store).import(&document)?;
            println!(
                "imported: {} sessions from {}",
                document.sessions.len(),
                archive.display()
            );
            Ok(())
        }
        SessionCommand::Migrate {
            session_id,
            source,
            destination,
        } => {
            let inspector = session_inspector(&source);
            let sessions = session_id.map_or_else(
                || inspector.inspect_all(),
                |session_id| {
                    inspector
                        .inspect_one(&session_id)
                        .map(|session| vec![session])
                },
            )?;
            let archive = SessionArchive::new(sessions)?;
            session_importer(&destination).import(&archive)?;
            println!("migrated: {} sessions", archive.sessions.len());
            Ok(())
        }
    }
}

fn session_inspector(store: &SessionStore) -> Box<dyn SessionInspector> {
    match store {
        SessionStore::File(directory) => Box::new(FileSessionInspector::new(directory)),
        SessionStore::Sqlite(database) => Box::new(SqliteSessionInspector::new(database)),
    }
}

fn session_importer(store: &SessionStore) -> Box<dyn SessionImporter> {
    match store {
        SessionStore::File(directory) => Box::new(FileSessionImporter::new(directory)),
        SessionStore::Sqlite(database) => Box::new(SqliteSessionImporter::new(database)),
    }
}

fn read_session_archive(path: &Path) -> Result<SessionArchive, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect Session archive: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Session archive is not a regular file".to_owned());
    }
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read Session archive: {error}"))?;
    SessionArchive::parse(&bytes)
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
    "usage: lenso-agent-cli generations <inspect --digest <sha256:digest> [--root <plugin-root>]|gc-preview [--root <plugin-root>] [--sessions <session-directory>|--session-database <sqlite-path>]|gc --apply [--root <plugin-root>] [--sessions <session-directory>|--session-database <sqlite-path>]>".to_owned()
}

fn session_usage() -> String {
    "usage: lenso-agent-cli sessions provenance --session <id> [--directory <session-directory>|--database <sqlite-path>] [--runtime-root <plugin-root>]\n       lenso-agent-cli sessions export --archive <json-path> [--session <id>] [--directory <session-directory>|--database <sqlite-path>]\n       lenso-agent-cli sessions import --archive <json-path> [--directory <session-directory>|--database <sqlite-path>]\n       lenso-agent-cli sessions migrate [--session <id>] <--from-directory <path>|--from-database <path>> <--to-directory <path>|--to-database <path>>".to_owned()
}
