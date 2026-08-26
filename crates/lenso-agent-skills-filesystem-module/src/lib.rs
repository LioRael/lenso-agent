//! Bounded filesystem-backed Skill catalog and progressive-disclosure Provider Module.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    path::{Component, Path, PathBuf},
    rc::Rc,
};

use directories::BaseDirs;
use lenso::prelude::*;
use lenso_capability_agent_prompt_provider as prompt_provider;
use lenso_capability_agent_prompt_provider::{
    ContributeError, ContributeRequest, ContributeResponse, ContributeResponseContributionsItem,
    ContributeResponseContributionsItemKind,
};
use lenso_capability_agent_tool_provider as tool_provider;
use lenso_capability_agent_tool_provider::{
    CatalogError, CatalogRequest, CatalogResponse, ContentType, ExecuteError, ExecuteRequest,
    ExecuteResponse, ToolDefinition, ToolExecutionClass,
};
use sha2::{Digest, Sha256};

/// Lists metadata for the snapshotted Skills.
pub const LIST_TOOL: &str = "skill_list";
/// Reads one full snapshotted Skill document.
pub const READ_TOOL: &str = "skill";
/// Lists readable resources snapshotted below one Skill directory.
pub const LIST_RESOURCES_TOOL: &str = "skill_resources";
/// Reads one UTF-8 resource snapshotted below one Skill directory.
pub const READ_RESOURCE_TOOL: &str = "skill_resource";

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
    catalog_contribution_id: String,
    max_prompt_catalog_bytes: usize,
    max_resource_entries: usize,
    max_resource_file_bytes: usize,
    max_resource_total_bytes: usize,
    max_resource_manifest_bytes: usize,
}

#[derive(Clone, Debug)]
struct SkillSnapshot {
    name: String,
    description: String,
    version: String,
    content: String,
    resources: BTreeMap<String, ResourceSnapshot>,
    resource_manifest_json: String,
    omitted_resource_count: usize,
}

#[derive(Clone, Debug)]
struct ResourceSnapshot {
    path: String,
    version: String,
    content: String,
}

#[derive(Clone, Debug)]
struct SkillsSnapshot {
    skills: BTreeMap<String, SkillSnapshot>,
    catalog_json: String,
    catalog_contribution: ContributeResponseContributionsItem,
}

#[derive(Clone, Debug, Default)]
struct FilesystemSkillsProvider {
    state: Rc<RefCell<Option<SkillsSnapshot>>>,
}

/// One filesystem Skill catalog exposed through both Tool and Prompt roles.
#[lenso::module(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct FilesystemSkillsModule {
    #[config]
    config: SkillsConfig,
    provider: FilesystemSkillsProvider,
}

impl Lifecycle for FilesystemSkillsModule {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        let snapshot = load_snapshot(&self.config)?;
        self.provider.state.replace(Some(snapshot));
        Ok(())
    }
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

                serde_json::from_str::<Arguments>(request.arguments_json.as_str())
                    .map_err(|_| ExecuteError::InvalidArguments)?;
                Ok(ExecuteResponse {
                    content: state.catalog_json,
                    content_type: ContentType::Text,
                    metadata_json: json_metadata(
                        &serde_json::json!({ "skill_count": state.skills.len() }),
                    ),
                })
            }
            READ_TOOL => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Arguments {
                    name: String,
                }

                let arguments = serde_json::from_str::<Arguments>(request.arguments_json.as_str())
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
                    content_type: ContentType::Text,
                    metadata_json: json_metadata(&serde_json::json!({
                        "name": skill.name,
                        "version": skill.version,
                        "digest": format!("sha256:{}", skill.version),
                    })),
                })
            }
            LIST_RESOURCES_TOOL => {
                let name = parse_skill_name(request.arguments_json.as_str())?;
                let skill = state.skills.get(&name).ok_or(ExecuteError::NotFound)?;
                Ok(ExecuteResponse {
                    content: skill.resource_manifest_json.clone(),
                    content_type: ContentType::Text,
                    metadata_json: json_metadata(&serde_json::json!({
                        "name": skill.name,
                        "skill_version": skill.version,
                        "resource_count": skill.resources.len(),
                        "omitted_resource_count": skill.omitted_resource_count,
                    })),
                })
            }
            READ_RESOURCE_TOOL => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Arguments {
                    name: String,
                    path: String,
                }

                let arguments = serde_json::from_str::<Arguments>(request.arguments_json.as_str())
                    .map_err(|_| ExecuteError::InvalidArguments)?;
                if !valid_path_component(&arguments.name) || !valid_resource_path(&arguments.path) {
                    return Err(ExecuteError::InvalidArguments.into());
                }
                let skill = state
                    .skills
                    .get(&arguments.name)
                    .ok_or(ExecuteError::NotFound)?;
                let resource = skill
                    .resources
                    .get(&arguments.path)
                    .ok_or(ExecuteError::NotFound)?;
                Ok(ExecuteResponse {
                    content: resource.content.clone(),
                    content_type: ContentType::Text,
                    metadata_json: json_metadata(&serde_json::json!({
                        "name": skill.name,
                        "skill_version": skill.version,
                        "path": resource.path,
                        "version": resource.version,
                        "digest": format!("sha256:{}", resource.version),
                    })),
                })
            }
            _ => Err(ExecuteError::NotFound.into()),
        }
    }
}

fn json_metadata(value: &serde_json::Value) -> tool_provider::RawJson {
    value
        .to_string()
        .try_into()
        .expect("serde_json output must be valid JSON")
}

fn parse_skill_name(arguments_json: &str) -> Result<String, ExecuteError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Arguments {
        name: String,
    }

    let arguments = serde_json::from_str::<Arguments>(arguments_json)
        .map_err(|_| ExecuteError::InvalidArguments)?;
    if !valid_path_component(&arguments.name) {
        return Err(ExecuteError::InvalidArguments);
    }
    Ok(arguments.name)
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

#[lenso::provides(tool_provider::ToolProvider, prompt_provider::PromptProvider)]
impl FilesystemSkillsModule {
    #[allow(clippy::unused_self)]
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<Output = Result<CatalogResponse, CatalogError>> {
        std::future::ready(Ok(CatalogResponse {
            tools: vec![
                ToolDefinition {
                    name: LIST_TOOL.to_owned(),
                    description:
                        "List available Skills by name, description, and immutable content version."
                            .to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{},"type":"object"}"#.to_owned()
                        .try_into()
                        .expect("static Tool Schema must be valid JSON"),
                    execution: ToolExecutionClass::ParallelSafe,
                },
                ToolDefinition {
                    name: READ_TOOL.to_owned(),
                    description: "Read the full SKILL.md for one available Skill by exact name."
                        .to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"name":{"minLength":1,"type":"string"}},"required":["name"],"type":"object"}"#.to_owned()
                        .try_into()
                        .expect("static Tool Schema must be valid JSON"),
                    execution: ToolExecutionClass::ParallelSafe,
                },
                ToolDefinition {
                    name: LIST_RESOURCES_TOOL.to_owned(),
                    description: "List readable snapshotted resources for one Skill without returning their contents."
                        .to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"name":{"minLength":1,"type":"string"}},"required":["name"],"type":"object"}"#.to_owned()
                        .try_into()
                        .expect("static Tool Schema must be valid JSON"),
                    execution: ToolExecutionClass::ParallelSafe,
                },
                ToolDefinition {
                    name: READ_RESOURCE_TOOL.to_owned(),
                    description: "Read one UTF-8 snapshotted resource by Skill name and relative path. This never executes scripts."
                        .to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"name":{"minLength":1,"type":"string"},"path":{"minLength":1,"type":"string"}},"required":["name","path"],"type":"object"}"#.to_owned()
                        .try_into()
                        .expect("static Tool Schema must be valid JSON"),
                    execution: ToolExecutionClass::ParallelSafe,
                },
            ],
        }))
    }

    fn execute(
        &self,
        _context: Ctx,
        request: ExecuteRequest,
    ) -> impl std::future::Future<Output = ModuleResult<ExecuteResponse, ExecuteError>> {
        let result = self.provider.execute_now(&request);
        drop(request);
        std::future::ready(match result {
            Ok(response) => Ok(response),
            Err(ProviderFailure::Domain(error)) => Err(ModuleError::domain(error)),
            Err(ProviderFailure::Runtime(error)) => Err(ModuleError::runtime(error)),
        })
    }

    fn contribute(
        &self,
        _context: Ctx,
        _request: ContributeRequest,
    ) -> impl std::future::Future<Output = ModuleResult<ContributeResponse, ContributeError>> {
        let result = self
            .provider
            .state
            .borrow()
            .as_ref()
            .map(|state| ContributeResponse {
                contributions: vec![state.catalog_contribution.clone()],
            })
            .ok_or(RuntimeFailure::Unavailable {
                capability: lenso_capability_agent_prompt_provider::CAPABILITY_ID,
            })
            .map_err(ModuleError::runtime);
        std::future::ready(result)
    }
}

fn validate_config(config: &SkillsConfig) -> Result<(), RuntimeFailure> {
    if config.root.as_os_str().is_empty()
        || !(1..=1_024).contains(&config.max_skills)
        || !(1..=MAX_PROVIDER_OUTPUT_BYTES).contains(&config.max_file_bytes)
        || !(1..=16_777_216).contains(&config.max_total_bytes)
        || !(1..=MAX_PROVIDER_OUTPUT_BYTES).contains(&config.max_catalog_bytes)
        || !valid_contribution_id(&config.catalog_contribution_id)
        || !(512..=262_144).contains(&config.max_prompt_catalog_bytes)
        || !(1..=65_536).contains(&config.max_resource_entries)
        || !(1..=MAX_PROVIDER_OUTPUT_BYTES).contains(&config.max_resource_file_bytes)
        || !(1..=67_108_864).contains(&config.max_resource_total_bytes)
        || !(1..=MAX_PROVIDER_OUTPUT_BYTES).contains(&config.max_resource_manifest_bytes)
        || config.max_file_bytes > config.max_total_bytes
        || config.max_resource_file_bytes > config.max_resource_total_bytes
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
    let mut resource_budget = ResourceBudget::default();
    let mut skills = BTreeMap::new();
    for (directory_name, document) in candidates {
        let (skill, file_bytes) = load_skill(
            &root,
            &directory_name,
            &document,
            config,
            &mut resource_budget,
        )?;
        total_bytes = total_bytes.saturating_add(file_bytes);
        if total_bytes > config.max_total_bytes {
            return Err(module_failure(
                "filesystem Skills exceed their aggregate content limit",
            ));
        }
        skills.insert(skill.name.clone(), skill);
    }
    let catalog_json = catalog_json(&skills, config.max_catalog_bytes)?;
    let catalog_content = prompt_catalog(&skills, config.max_prompt_catalog_bytes)?;
    let catalog_contribution = ContributeResponseContributionsItem {
        id: config.catalog_contribution_id.clone(),
        version: format!("{:x}", Sha256::digest(catalog_content.as_bytes())),
        kind: ContributeResponseContributionsItemKind::Instruction,
        content: catalog_content,
    };
    Ok(SkillsSnapshot {
        skills,
        catalog_json,
        catalog_contribution,
    })
}

#[derive(Debug, Default)]
struct ResourceBudget {
    entries: usize,
    content_bytes: usize,
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
    resource_budget: &mut ResourceBudget,
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
    let skill_root = resolved.parent().ok_or_else(|| {
        module_failure(format!("filesystem Skill `{directory_name}` has no root"))
    })?;
    let (resources, omitted_resource_count) =
        load_resources(skill_root, directory_name, config, resource_budget)?;
    let resource_manifest_json = resource_manifest_json(
        directory_name,
        &version,
        &resources,
        omitted_resource_count,
        config.max_resource_manifest_bytes,
    )?;
    Ok((
        SkillSnapshot {
            name,
            description,
            version,
            content,
            resources,
            resource_manifest_json,
            omitted_resource_count,
        },
        file_bytes,
    ))
}

fn load_resources(
    skill_root: &Path,
    skill_name: &str,
    config: &SkillsConfig,
    budget: &mut ResourceBudget,
) -> Result<(BTreeMap<String, ResourceSnapshot>, usize), RuntimeFailure> {
    let mut directories = vec![skill_root.to_path_buf()];
    let mut candidates = Vec::new();
    let mut omitted = 0_usize;
    while let Some(directory) = directories.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| {
            module_failure(format!(
                "filesystem Skill `{skill_name}` resource directory is unreadable: {error}"
            ))
        })?;
        let mut entries = entries.collect::<Result<Vec<_>, _>>().map_err(|error| {
            module_failure(format!(
                "filesystem Skill `{skill_name}` resource entry is unreadable: {error}"
            ))
        })?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(skill_root).map_err(|_| {
                module_failure(format!(
                    "filesystem Skill `{skill_name}` resource escapes its root"
                ))
            })?;
            let file_type = entry.file_type().map_err(|error| {
                module_failure(format!(
                    "filesystem Skill `{skill_name}` resource type is unavailable: {error}"
                ))
            })?;
            if file_type.is_symlink() {
                return Err(module_failure(format!(
                    "filesystem Skill `{skill_name}` contains a resource symlink"
                )));
            }
            if hidden_resource_path(relative) {
                continue;
            }
            budget.entries = budget.entries.saturating_add(1);
            if budget.entries > config.max_resource_entries {
                return Err(module_failure(
                    "filesystem Skill resources exceed their aggregate entry limit",
                ));
            }
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file() && relative != Path::new("SKILL.md") {
                candidates.push((relative_resource_path(relative)?, path));
            } else if !file_type.is_file() {
                return Err(module_failure(format!(
                    "filesystem Skill `{skill_name}` contains an unsupported resource entry"
                )));
            }
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));

    let mut resources = BTreeMap::new();
    for (path, source) in candidates {
        let Some(resource) =
            snapshot_resource(skill_root, skill_name, path, &source, config, budget)?
        else {
            omitted = omitted.saturating_add(1);
            continue;
        };
        resources.insert(resource.path.clone(), resource);
    }
    Ok((resources, omitted))
}

fn snapshot_resource(
    skill_root: &Path,
    skill_name: &str,
    path: String,
    source: &Path,
    config: &SkillsConfig,
    budget: &mut ResourceBudget,
) -> Result<Option<ResourceSnapshot>, RuntimeFailure> {
    let resolved = fs::canonicalize(source).map_err(|error| {
        module_failure(format!(
            "filesystem Skill `{skill_name}` resource `{path}` is unavailable: {error}"
        ))
    })?;
    if !resolved.starts_with(skill_root) || !resolved.is_file() {
        return Err(module_failure(format!(
            "filesystem Skill `{skill_name}` resource `{path}` escapes its root"
        )));
    }
    let metadata = fs::metadata(&resolved).map_err(|error| {
        module_failure(format!(
            "filesystem Skill `{skill_name}` resource `{path}` metadata is unavailable: {error}"
        ))
    })?;
    let file_bytes = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if file_bytes > config.max_resource_file_bytes {
        return Ok(None);
    }
    let bytes = fs::read(&resolved).map_err(|error| {
        module_failure(format!(
            "filesystem Skill `{skill_name}` resource `{path}` is unreadable: {error}"
        ))
    })?;
    let Ok(content) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    budget.content_bytes = budget.content_bytes.saturating_add(content.len());
    if budget.content_bytes > config.max_resource_total_bytes {
        return Err(module_failure(
            "filesystem Skill resources exceed their aggregate content limit",
        ));
    }
    let version = format!("{:x}", Sha256::digest(content.as_bytes()));
    Ok(Some(ResourceSnapshot {
        path,
        version,
        content,
    }))
}

fn resource_manifest_json(
    skill_name: &str,
    skill_version: &str,
    resources: &BTreeMap<String, ResourceSnapshot>,
    omitted_resource_count: usize,
    max_bytes: usize,
) -> Result<String, RuntimeFailure> {
    let entries = resources
        .values()
        .map(|resource| {
            serde_json::json!({
                "path": resource.path,
                "bytes": resource.content.len(),
                "version": resource.version,
            })
        })
        .collect::<Vec<_>>();
    let manifest = serde_json::json!({
        "name": skill_name,
        "skill_version": skill_version,
        "resources": entries,
        "omitted_resource_count": omitted_resource_count,
    })
    .to_string();
    if manifest.len() > max_bytes {
        return Err(module_failure(format!(
            "filesystem Skill `{skill_name}` resource manifest exceeds its output limit"
        )));
    }
    Ok(manifest)
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

fn prompt_catalog(
    skills: &BTreeMap<String, SkillSnapshot>,
    max_bytes: usize,
) -> Result<String, RuntimeFailure> {
    const HEADER: &str = "Available Skills (metadata only). When a task matches a Skill, call `skill` with its exact name before following it. Use `skill_list` only when this catalog reports omissions or no visible Skill matches.\n\n";
    const EMPTY: &str = "No Skills are available.\n";

    if skills.is_empty() {
        let content = format!("{HEADER}{EMPTY}");
        if content.len() > max_bytes {
            return Err(module_failure(
                "filesystem Skills prompt catalog limit is too small",
            ));
        }
        return Ok(content);
    }

    let maximum_footer = format!(
        "\n{} additional Skills were omitted by the prompt catalog byte limit; call `skill_list` to inspect them.\n",
        skills.len()
    );
    let mut content = String::from(HEADER);
    let mut omitted = 0_usize;
    let mut catalog_full = false;
    for skill in skills.values() {
        let line = format!("- `{}`: {}\n", skill.name, skill.description);
        if !catalog_full
            && content
                .len()
                .saturating_add(line.len())
                .saturating_add(maximum_footer.len())
                <= max_bytes
        {
            content.push_str(&line);
        } else {
            catalog_full = true;
            omitted = omitted.saturating_add(1);
        }
    }
    if omitted > 0 {
        write!(
            &mut content,
            "\n{omitted} additional Skills were omitted by the prompt catalog byte limit; call `skill_list` to inspect them.\n"
        )
        .expect("writing to a String cannot fail");
    }
    if content.len() > max_bytes {
        return Err(module_failure(
            "filesystem Skills prompt catalog exceeds its output limit",
        ));
    }
    Ok(content)
}

fn valid_contribution_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'/' | b'-'))
        })
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

fn valid_resource_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 512
        || value.contains('\\')
        || value.split('/').any(str::is_empty)
    {
        return false;
    }
    let path = Path::new(value);
    path.components().all(|component| match component {
        Component::Normal(component) => component
            .to_str()
            .is_some_and(|part| !part.is_empty() && !part.starts_with('.')),
        _ => false,
    })
}

fn hidden_resource_path(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(component) => component.to_str().is_none_or(|part| part.starts_with('.')),
        _ => true,
    })
}

fn relative_resource_path(path: &Path) -> Result<String, RuntimeFailure> {
    let parts =
        path.components()
            .map(|component| match component {
                Component::Normal(part) => part.to_str().map(ToOwned::to_owned).ok_or_else(|| {
                    module_failure("filesystem Skill resource path is not valid UTF-8")
                }),
                _ => Err(module_failure(
                    "filesystem Skill resource path is not relative",
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
    let value = parts.join("/");
    if !valid_resource_path(&value) {
        return Err(module_failure("filesystem Skill resource path is invalid"));
    }
    Ok(value)
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
            catalog_contribution_id: "agents.skills.catalog".to_owned(),
            max_prompt_catalog_bytes: 8_192,
            max_resource_entries: 64,
            max_resource_file_bytes: 8_192,
            max_resource_total_bytes: 32_768,
            max_resource_manifest_bytes: 8_192,
        }
    }

    #[test]
    fn generated_descriptor_owns_both_provider_roles() {
        let descriptor: serde_json::Value = serde_json::from_str(MODULE_DESCRIPTOR_JSON).unwrap();
        let provided = descriptor["provided_capabilities"].as_array().unwrap();

        assert_eq!(descriptor["package_id"], "lenso.agent.skills.filesystem");
        assert_eq!(provided.len(), 2);
        assert_eq!(provided[0]["capability_id"], "lenso.agent.tool-provider@2");
        assert_eq!(
            provided[1]["capability_id"],
            "lenso.agent.prompt-provider@1"
        );
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
        assert_eq!(snapshot.catalog_contribution.id, "agents.skills.catalog");
        assert!(snapshot.catalog_contribution.content.contains("Alpha help"));
        assert!(
            snapshot
                .catalog_contribution
                .content
                .contains("Beta help on two lines")
        );
        assert!(
            !snapshot
                .catalog_contribution
                .content
                .contains("ALPHA SECRET")
        );

        let state = Rc::new(RefCell::new(Some(snapshot)));
        let provider = FilesystemSkillsProvider { state };
        let listed = provider
            .execute_now(&ExecuteRequest {
                name: LIST_TOOL.to_owned(),
                arguments_json: "{}".to_owned().try_into().unwrap(),
            })
            .unwrap();
        assert!(!listed.content.contains("SECRET"));
        let read = provider
            .execute_now(&ExecuteRequest {
                name: READ_TOOL.to_owned(),
                arguments_json: r#"{"name":"beta"}"#.to_owned().try_into().unwrap(),
            })
            .unwrap();
        assert!(read.content.contains("BETA SECRET"));
        assert!(!read.content.contains("ALPHA SECRET"));
        assert!(read.metadata_json.as_str().contains("sha256:"));
    }

    #[test]
    fn lists_and_reads_nested_text_resources_without_executing_scripts() {
        let temporary = tempfile::tempdir().unwrap();
        write_skill(
            temporary.path(),
            "alpha",
            "Alpha help",
            "Read references/checklist.md.",
        );
        let skill = temporary.path().join("alpha");
        fs::create_dir_all(skill.join("references/nested")).unwrap();
        fs::create_dir_all(skill.join("scripts")).unwrap();
        fs::write(skill.join("references/checklist.md"), "RESOURCE CHECKLIST").unwrap();
        fs::write(skill.join("references/nested/details.txt"), "DETAILS").unwrap();
        let sentinel = temporary.path().join("must-not-exist");
        fs::write(
            skill.join("scripts/do-not-run.sh"),
            format!("touch {}\n", sentinel.display()),
        )
        .unwrap();

        let snapshot = load_snapshot(&config(temporary.path())).unwrap();
        assert!(!sentinel.exists());
        let provider = FilesystemSkillsProvider {
            state: Rc::new(RefCell::new(Some(snapshot))),
        };
        let listed = provider
            .execute_now(&ExecuteRequest {
                name: LIST_RESOURCES_TOOL.to_owned(),
                arguments_json: r#"{"name":"alpha"}"#.to_owned().try_into().unwrap(),
            })
            .unwrap();
        assert!(listed.content.contains("references/checklist.md"));
        assert!(listed.content.contains("references/nested/details.txt"));
        assert!(listed.content.contains("scripts/do-not-run.sh"));
        assert!(!listed.content.contains("RESOURCE CHECKLIST"));

        let read = provider
            .execute_now(&ExecuteRequest {
                name: READ_RESOURCE_TOOL.to_owned(),
                arguments_json: r#"{"name":"alpha","path":"references/checklist.md"}"#
                    .to_owned()
                    .try_into()
                    .unwrap(),
            })
            .unwrap();
        assert_eq!(read.content, "RESOURCE CHECKLIST");
        assert!(read.metadata_json.as_str().contains("sha256:"));
        assert!(!sentinel.exists());
        assert!(matches!(
            provider.execute_now(&ExecuteRequest {
                name: READ_RESOURCE_TOOL.to_owned(),
                arguments_json: r#"{"name":"alpha","path":"references/missing.md"}"#
                    .to_owned()
                    .try_into()
                    .unwrap(),
            }),
            Err(ProviderFailure::Domain(ExecuteError::NotFound))
        ));
    }

    #[test]
    fn omits_binary_and_oversized_resources_and_keeps_resource_snapshots_stable() {
        let temporary = tempfile::tempdir().unwrap();
        write_skill(temporary.path(), "alpha", "Alpha help", "body");
        let skill = temporary.path().join("alpha");
        fs::write(skill.join("binary.bin"), [0xff, 0xfe]).unwrap();
        fs::write(skill.join("large.txt"), "too large").unwrap();
        fs::write(skill.join("stable.txt"), "ORIGINAL RESOURCE").unwrap();
        let mut bounded = config(temporary.path());
        bounded.max_resource_file_bytes = 20;
        let snapshot = load_snapshot(&bounded).unwrap();
        assert_eq!(snapshot.skills["alpha"].omitted_resource_count, 1);
        assert!(
            !snapshot.skills["alpha"]
                .resources
                .contains_key("binary.bin")
        );
        fs::write(skill.join("stable.txt"), "CHANGED RESOURCE").unwrap();
        assert_eq!(
            snapshot.skills["alpha"].resources["stable.txt"].content,
            "ORIGINAL RESOURCE"
        );

        let mut aggregate_limited = bounded.clone();
        aggregate_limited.max_resource_total_bytes = 20;
        assert!(load_snapshot(&aggregate_limited).is_err());

        bounded.max_resource_file_bytes = 4;
        let bounded_snapshot = load_snapshot(&bounded).unwrap();
        assert!(bounded_snapshot.skills["alpha"].resources.is_empty());
        assert_eq!(bounded_snapshot.skills["alpha"].omitted_resource_count, 3);
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
    fn prompt_catalog_is_bounded_and_reports_deterministic_omissions() {
        let temporary = tempfile::tempdir().unwrap();
        for name in ["alpha", "beta", "gamma", "omega"] {
            write_skill(
                temporary.path(),
                name,
                &format!("{name} {}", "description ".repeat(20)),
                "PRIVATE BODY",
            );
        }
        let mut bounded = config(temporary.path());
        bounded.max_prompt_catalog_bytes = 512;
        let snapshot = load_snapshot(&bounded).unwrap();
        let content = &snapshot.catalog_contribution.content;
        assert!(content.len() <= 512);
        assert!(content.contains("additional Skills were omitted"));
        assert!(content.contains("skill_list"));
        assert!(!content.contains("PRIVATE BODY"));
        assert_eq!(
            snapshot.catalog_contribution.version,
            format!("{:x}", Sha256::digest(content.as_bytes()))
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

        let mut invalid_id = config(temporary.path());
        invalid_id.catalog_contribution_id = "Invalid ID".to_owned();
        assert!(load_snapshot(&invalid_id).is_err());
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

    #[cfg(unix)]
    #[test]
    fn rejects_resource_symlinks_and_invalid_resource_arguments() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        write_skill(temporary.path(), "alpha", "Alpha help", "body");
        let outside = temporary.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        symlink(&outside, temporary.path().join("alpha/escape.txt")).unwrap();
        assert!(load_snapshot(&config(temporary.path())).is_err());

        fs::remove_file(temporary.path().join("alpha/escape.txt")).unwrap();
        let provider = FilesystemSkillsProvider {
            state: Rc::new(RefCell::new(Some(
                load_snapshot(&config(temporary.path())).unwrap(),
            ))),
        };
        assert!(matches!(
            provider.execute_now(&ExecuteRequest {
                name: READ_RESOURCE_TOOL.to_owned(),
                arguments_json:
                    r#"{"name":"alpha","path":"../outside.txt"}"#.to_owned().try_into().unwrap(),
            }),
            Err(ProviderFailure::Domain(ExecuteError::InvalidArguments))
        ));
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
                arguments_json: r#"{"name":"missing"}"#.to_owned().try_into().unwrap(),
            }),
            Err(ProviderFailure::Domain(ExecuteError::NotFound))
        ));
    }
}
