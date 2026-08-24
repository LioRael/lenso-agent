//! Bounded filesystem-backed Skill catalog Tool Provider Module.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    rc::Rc,
};

use directories::BaseDirs;
use futures::future::{LocalBoxFuture, ready};
use lenso_capability_agent_tool_provider::{
    CatalogError, CatalogRequest, CatalogResponse, CatalogResponseToolsItem, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecuteResponseContentType, ToolProviderEndpoint,
    ToolProviderProvider,
};
use lenso_kernel::{
    InvocationContext, ModuleFuture, ModuleLifecycle, NativeRequestEndpoint, PrepareContext,
    RuntimeFailure,
};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};
use sha2::{Digest, Sha256};

/// Runtime package identity selected by App Composition.
pub const PACKAGE_ID: &str = "lenso.agent.skills.filesystem";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Lists metadata for the snapshotted Skills.
pub const LIST_TOOL: &str = "skills.list";
/// Reads one full snapshotted Skill document.
pub const READ_TOOL: &str = "skills.read";

const MAX_PROVIDER_OUTPUT_BYTES: usize = 1_048_576;
const MAX_DESCRIPTION_BYTES: usize = 4_096;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillsConfig {
    root: PathBuf,
    max_skills: usize,
    max_file_bytes: usize,
    max_total_bytes: usize,
    max_catalog_bytes: usize,
}

/// Native factory for an explicitly selected filesystem Skill catalog.
#[derive(Clone, Debug, Default)]
pub struct FilesystemSkillsFactory;

impl NativeModuleFactory for FilesystemSkillsFactory {
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
            return Err(invalid_plan("unsupported filesystem Skills entrypoint"));
        }
        let config =
            serde_json::from_str::<SkillsConfig>(context.configuration()).map_err(|error| {
                invalid_plan(format!("invalid filesystem Skills configuration: {error}"))
            })?;
        validate_config(&config)?;
        let state = Rc::new(RefCell::new(None));
        let endpoint = Rc::new(ToolProviderEndpoint::new(FilesystemSkillsProvider {
            state: state.clone(),
        })) as Rc<dyn NativeRequestEndpoint>;
        Ok(NativeModuleInstance::with_lifecycle(
            vec![endpoint],
            FilesystemSkillsLifecycle { config, state },
        ))
    }
}

#[derive(Clone, Debug)]
struct SkillSnapshot {
    name: String,
    description: String,
    version: String,
    content: String,
}

#[derive(Clone, Debug)]
struct SkillsSnapshot {
    skills: BTreeMap<String, SkillSnapshot>,
    catalog_json: String,
}

#[derive(Clone, Debug)]
struct FilesystemSkillsProvider {
    state: Rc<RefCell<Option<SkillsSnapshot>>>,
}

impl FilesystemSkillsProvider {
    fn execute_now(&self, request: &ExecuteRequest) -> Result<ExecuteResponse, ProviderFailure> {
        let state = self
            .state
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::Unavailable {
                capability: lenso_capability_agent_tool_provider::CAPABILITY_ID,
            })?;
        match request.name.as_str() {
            LIST_TOOL => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Arguments {}

                serde_json::from_str::<Arguments>(&request.arguments_json)
                    .map_err(|_| ExecuteError::InvalidArguments)?;
                Ok(ExecuteResponse {
                    content: state.catalog_json,
                    content_type: ExecuteResponseContentType::Text,
                    metadata_json: serde_json::json!({ "skill_count": state.skills.len() })
                        .to_string(),
                })
            }
            READ_TOOL => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Arguments {
                    name: String,
                }

                let arguments = serde_json::from_str::<Arguments>(&request.arguments_json)
                    .map_err(|_| ExecuteError::InvalidArguments)?;
                if !valid_path_component(&arguments.name) {
                    return Err(ExecuteError::InvalidArguments.into());
                }
                let skill = state
                    .skills
                    .get(&arguments.name)
                    .ok_or(ExecuteError::NotFound)?;
                Ok(ExecuteResponse {
                    content: skill.content.clone(),
                    content_type: ExecuteResponseContentType::Text,
                    metadata_json: serde_json::json!({
                        "name": skill.name,
                        "version": skill.version,
                        "digest": format!("sha256:{}", skill.version),
                    })
                    .to_string(),
                })
            }
            _ => Err(ExecuteError::NotFound.into()),
        }
    }
}

#[derive(Debug)]
enum ProviderFailure {
    Domain(ExecuteError),
    Runtime(RuntimeFailure),
}

impl From<ExecuteError> for ProviderFailure {
    fn from(error: ExecuteError) -> Self {
        Self::Domain(error)
    }
}

impl From<RuntimeFailure> for ProviderFailure {
    fn from(error: RuntimeFailure) -> Self {
        Self::Runtime(error)
    }
}

impl ToolProviderProvider for FilesystemSkillsProvider {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> LocalBoxFuture<'static, Result<Result<CatalogResponse, CatalogError>, RuntimeFailure>>
    {
        Box::pin(ready(Ok(Ok(CatalogResponse {
            tools: vec![
                CatalogResponseToolsItem {
                    name: LIST_TOOL.to_owned(),
                    description:
                        "List available Skills by name, description, and immutable content version."
                            .to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{},"type":"object"}"#
                        .to_owned(),
                },
                CatalogResponseToolsItem {
                    name: READ_TOOL.to_owned(),
                    description: "Read the full SKILL.md for one available Skill by exact name."
                        .to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"name":{"minLength":1,"type":"string"}},"required":["name"],"type":"object"}"#
                        .to_owned(),
                },
            ],
        }))))
    }

    fn execute(
        &self,
        _context: InvocationContext,
        request: ExecuteRequest,
    ) -> LocalBoxFuture<'static, Result<Result<ExecuteResponse, ExecuteError>, RuntimeFailure>>
    {
        let result = match self.execute_now(&request) {
            Ok(response) => Ok(Ok(response)),
            Err(ProviderFailure::Domain(error)) => Ok(Err(error)),
            Err(ProviderFailure::Runtime(error)) => Err(error),
        };
        Box::pin(ready(result))
    }
}

#[derive(Debug)]
struct FilesystemSkillsLifecycle {
    config: SkillsConfig,
    state: Rc<RefCell<Option<SkillsSnapshot>>>,
}

impl ModuleLifecycle for FilesystemSkillsLifecycle {
    fn prepare(&self, _context: PrepareContext) -> ModuleFuture {
        let result = load_snapshot(&self.config);
        if let Ok(snapshot) = &result {
            self.state.replace(Some(snapshot.clone()));
        }
        Box::pin(ready(result.map(|_| ())))
    }
}

fn validate_config(config: &SkillsConfig) -> Result<(), RuntimeFailure> {
    if config.root.as_os_str().is_empty()
        || !(1..=1_024).contains(&config.max_skills)
        || !(1..=MAX_PROVIDER_OUTPUT_BYTES).contains(&config.max_file_bytes)
        || !(1..=16_777_216).contains(&config.max_total_bytes)
        || !(1..=MAX_PROVIDER_OUTPUT_BYTES).contains(&config.max_catalog_bytes)
        || config.max_file_bytes > config.max_total_bytes
    {
        return Err(invalid_plan(
            "filesystem Skills configuration limits are invalid",
        ));
    }
    Ok(())
}

fn load_snapshot(config: &SkillsConfig) -> Result<SkillsSnapshot, RuntimeFailure> {
    validate_config(config)?;
    let root = canonical_root(&config.root)?;
    let candidates = discover_skills(&root, config.max_skills)?;
    let mut total_bytes = 0_usize;
    let mut skills = BTreeMap::new();
    for (directory_name, document) in candidates {
        let (skill, file_bytes) = load_skill(&root, &directory_name, &document, config)?;
        total_bytes = total_bytes.saturating_add(file_bytes);
        if total_bytes > config.max_total_bytes {
            return Err(module_failure(
                "filesystem Skills exceed their aggregate content limit",
            ));
        }
        skills.insert(skill.name.clone(), skill);
    }
    let catalog_json = catalog_json(&skills, config.max_catalog_bytes)?;
    Ok(SkillsSnapshot {
        skills,
        catalog_json,
    })
}

fn canonical_root(configured: &Path) -> Result<PathBuf, RuntimeFailure> {
    let expanded = expand_home(configured)?;
    let root = fs::canonicalize(&expanded).map_err(|error| {
        module_failure(format!(
            "filesystem Skills root `{}` is unavailable: {error}",
            expanded.display()
        ))
    })?;
    if !root.is_dir() {
        return Err(module_failure("filesystem Skills root is not a directory"));
    }
    Ok(root)
}

fn discover_skills(
    root: &Path,
    max_skills: usize,
) -> Result<Vec<(String, PathBuf)>, RuntimeFailure> {
    let entries = fs::read_dir(root).map_err(|error| {
        module_failure(format!("filesystem Skills root is unreadable: {error}"))
    })?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            module_failure(format!(
                "filesystem Skills directory entry is unreadable: {error}"
            ))
        })?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| module_failure("filesystem Skill directory name is not valid UTF-8"))?;
        if name.starts_with('.') {
            continue;
        }
        if !valid_path_component(&name) {
            return Err(module_failure(format!(
                "filesystem Skill directory name `{name}` is invalid"
            )));
        }
        let document = entry.path().join("SKILL.md");
        if document.exists() {
            candidates.push((name, document));
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    if candidates.len() > max_skills {
        return Err(module_failure(
            "filesystem Skills exceed the configured catalog count limit",
        ));
    }
    Ok(candidates)
}

fn load_skill(
    root: &Path,
    directory_name: &str,
    document: &Path,
    config: &SkillsConfig,
) -> Result<(SkillSnapshot, usize), RuntimeFailure> {
    let resolved = fs::canonicalize(document).map_err(|error| {
        module_failure(format!(
            "filesystem Skill `{directory_name}` is unavailable: {error}"
        ))
    })?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err(module_failure(format!(
            "filesystem Skill `{directory_name}` escapes its configured root"
        )));
    }
    let metadata = fs::metadata(&resolved).map_err(|error| {
        module_failure(format!(
            "filesystem Skill `{directory_name}` metadata is unavailable: {error}"
        ))
    })?;
    let file_bytes = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if file_bytes == 0 || file_bytes > config.max_file_bytes {
        return Err(module_failure(format!(
            "filesystem Skill `{directory_name}` exceeds its file limit"
        )));
    }
    let content = fs::read_to_string(&resolved).map_err(|error| {
        module_failure(format!(
            "filesystem Skill `{directory_name}` is not readable UTF-8: {error}"
        ))
    })?;
    let (name, description) = parse_frontmatter(directory_name, &content)?;
    if name != directory_name {
        return Err(module_failure(format!(
            "filesystem Skill `{directory_name}` frontmatter name must match its directory"
        )));
    }
    let version = format!("{:x}", Sha256::digest(content.as_bytes()));
    Ok((
        SkillSnapshot {
            name,
            description,
            version,
            content,
        },
        file_bytes,
    ))
}

fn catalog_json(
    skills: &BTreeMap<String, SkillSnapshot>,
    max_catalog_bytes: usize,
) -> Result<String, RuntimeFailure> {
    let catalog = skills
        .values()
        .map(|skill| {
            serde_json::json!({
                "name": skill.name,
                "description": skill.description,
                "version": skill.version,
            })
        })
        .collect::<Vec<_>>();
    let catalog_json = serde_json::json!({ "skills": catalog }).to_string();
    if catalog_json.len() > max_catalog_bytes {
        return Err(module_failure(
            "filesystem Skills metadata exceed the configured catalog output limit",
        ));
    }
    Ok(catalog_json)
}

fn parse_frontmatter(
    directory_name: &str,
    content: &str,
) -> Result<(String, String), RuntimeFailure> {
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return Err(invalid_frontmatter(directory_name));
    }
    let mut frontmatter = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line == "---" {
            closed = true;
            break;
        }
        frontmatter.push(line);
    }
    if !closed {
        return Err(invalid_frontmatter(directory_name));
    }

    let name =
        scalar_field(&frontmatter, "name").ok_or_else(|| invalid_frontmatter(directory_name))?;
    let description = scalar_field(&frontmatter, "description")
        .ok_or_else(|| invalid_frontmatter(directory_name))?;
    if !valid_path_component(&name)
        || description.is_empty()
        || description.len() > MAX_DESCRIPTION_BYTES
    {
        return Err(invalid_frontmatter(directory_name));
    }
    Ok((name, description))
}

fn scalar_field(lines: &[&str], key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let index = lines.iter().position(|line| line.starts_with(&prefix))?;
    let value = lines[index].strip_prefix(&prefix)?.trim();
    if value.is_empty() || matches!(value, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
        let continuation = lines[index + 1..]
            .iter()
            .take_while(|line| line.starts_with(' ') || line.starts_with('\t') || line.is_empty())
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        return (!continuation.is_empty()).then_some(continuation);
    }
    let unquoted = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    };
    (!unquoted.is_empty()).then(|| unquoted.to_owned())
}

fn expand_home(path: &Path) -> Result<PathBuf, RuntimeFailure> {
    let value = path.to_string_lossy();
    if value == "~" {
        return BaseDirs::new()
            .map(|directories| directories.home_dir().to_path_buf())
            .ok_or_else(|| module_failure("home directory is unavailable"));
    }
    if let Some(relative) = value.strip_prefix("~/") {
        return BaseDirs::new()
            .map(|directories| directories.home_dir().join(relative))
            .ok_or_else(|| module_failure("home directory is unavailable"));
    }
    if value.starts_with('~') {
        return Err(invalid_plan(
            "filesystem Skills root supports only `~` or `~/...` home expansion",
        ));
    }
    Ok(path.to_path_buf())
}

fn valid_path_component(value: &str) -> bool {
    let path = Path::new(value);
    matches!(
        path.components().collect::<Vec<_>>().as_slice(),
        [Component::Normal(_)]
    ) && !value.starts_with('.')
        && value.len() <= 96
}

fn invalid_frontmatter(skill: &str) -> RuntimeFailure {
    module_failure(format!(
        "filesystem Skill `{skill}` has invalid name or description frontmatter"
    ))
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

fn module_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::ModuleFailure {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(root: &Path) -> SkillsConfig {
        SkillsConfig {
            root: root.to_path_buf(),
            max_skills: 8,
            max_file_bytes: 8_192,
            max_total_bytes: 32_768,
            max_catalog_bytes: 8_192,
        }
    }

    fn write_skill(root: &Path, name: &str, description: &str, body: &str) {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn lists_metadata_then_reads_one_snapshotted_document() {
        let temporary = tempfile::tempdir().unwrap();
        write_skill(temporary.path(), "alpha", "Alpha help", "ALPHA SECRET");
        write_skill(
            temporary.path(),
            "beta",
            ">-\n  Beta help\n  on two lines",
            "BETA SECRET",
        );
        let snapshot = load_snapshot(&config(temporary.path())).unwrap();
        assert!(snapshot.catalog_json.contains("Alpha help"));
        assert!(snapshot.catalog_json.contains("Beta help on two lines"));
        assert!(!snapshot.catalog_json.contains("ALPHA SECRET"));
        assert!(!snapshot.catalog_json.contains("BETA SECRET"));

        let state = Rc::new(RefCell::new(Some(snapshot)));
        let provider = FilesystemSkillsProvider { state };
        let listed = provider
            .execute_now(&ExecuteRequest {
                name: LIST_TOOL.to_owned(),
                arguments_json: "{}".to_owned(),
            })
            .unwrap();
        assert!(!listed.content.contains("SECRET"));
        let read = provider
            .execute_now(&ExecuteRequest {
                name: READ_TOOL.to_owned(),
                arguments_json: r#"{"name":"beta"}"#.to_owned(),
            })
            .unwrap();
        assert!(read.content.contains("BETA SECRET"));
        assert!(!read.content.contains("ALPHA SECRET"));
        assert!(read.metadata_json.contains("sha256:"));
    }

    #[test]
    fn accepts_an_indented_plain_multiline_description() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("multiline");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            "---\nname: multiline\ndescription:\n  First line\n  second line.\nlicense: MIT\n---\nbody\n",
        )
        .unwrap();
        let snapshot = load_snapshot(&config(temporary.path())).unwrap();
        assert_eq!(
            snapshot.skills["multiline"].description,
            "First line second line."
        );
    }

    #[test]
    fn snapshot_is_stable_after_source_changes() {
        let temporary = tempfile::tempdir().unwrap();
        write_skill(temporary.path(), "alpha", "Alpha help", "ORIGINAL");
        let snapshot = load_snapshot(&config(temporary.path())).unwrap();
        write_skill(temporary.path(), "alpha", "Alpha help", "CHANGED");
        assert!(snapshot.skills["alpha"].content.contains("ORIGINAL"));
        assert!(!snapshot.skills["alpha"].content.contains("CHANGED"));
    }

    #[test]
    fn rejects_name_mismatch_and_limits() {
        let temporary = tempfile::tempdir().unwrap();
        write_skill(temporary.path(), "alpha", "Alpha help", "body");
        fs::write(
            temporary.path().join("alpha/SKILL.md"),
            "---\nname: beta\ndescription: Wrong name\n---\nbody\n",
        )
        .unwrap();
        assert!(load_snapshot(&config(temporary.path())).is_err());

        write_skill(temporary.path(), "alpha", "Alpha help", "oversized");
        let mut limited = config(temporary.path());
        limited.max_file_bytes = 4;
        assert!(load_snapshot(&limited).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_skill_document_symlink_that_escapes_the_root() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skills");
        let outside = temporary.path().join("outside.md");
        fs::create_dir_all(root.join("escape")).unwrap();
        fs::write(
            &outside,
            "---\nname: escape\ndescription: Escape\n---\nsecret\n",
        )
        .unwrap();
        symlink(&outside, root.join("escape/SKILL.md")).unwrap();
        assert!(load_snapshot(&config(&root)).is_err());
    }

    #[test]
    fn rejects_missing_root_invalid_utf8_and_unknown_skill() {
        let temporary = tempfile::tempdir().unwrap();
        assert!(load_snapshot(&config(&temporary.path().join("missing"))).is_err());

        let root = temporary.path().join("skills");
        fs::create_dir_all(root.join("binary")).unwrap();
        fs::write(root.join("binary/SKILL.md"), [0xff, 0xfe]).unwrap();
        assert!(load_snapshot(&config(&root)).is_err());

        fs::remove_dir_all(root.join("binary")).unwrap();
        let provider = FilesystemSkillsProvider {
            state: Rc::new(RefCell::new(Some(load_snapshot(&config(&root)).unwrap()))),
        };
        assert!(matches!(
            provider.execute_now(&ExecuteRequest {
                name: READ_TOOL.to_owned(),
                arguments_json: r#"{"name":"missing"}"#.to_owned(),
            }),
            Err(ProviderFailure::Domain(ExecuteError::NotFound))
        ));
    }
}
