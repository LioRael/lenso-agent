//! Explicit filesystem-backed Skill contribution Module.

use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    rc::Rc,
};

use directories::BaseDirs;
use futures::future::ready;
use lenso_capability_agent_prompt_provider::{
    ContributeRequest, ContributeResponse, ContributeResponseContributionsItem,
    ContributeResponseContributionsItemKind, PromptProviderEndpoint, PromptProviderProvider,
};
use lenso_kernel::{
    InvocationContext, ModuleFuture, ModuleLifecycle, NativeRequestEndpoint, PrepareContext,
    RuntimeFailure,
};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};
use sha2::{Digest, Sha256};

/// Runtime package identity selected by App Composition.
pub const PACKAGE_ID: &str = "lenso.agent.prompt.filesystem";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesystemPromptConfig {
    root: PathBuf,
    skills: Vec<String>,
    id_prefix: String,
    max_file_bytes: usize,
    max_total_bytes: usize,
}

/// Native factory for an explicitly selected filesystem Skill set.
#[derive(Clone, Debug, Default)]
pub struct FilesystemPromptFactory;

impl NativeModuleFactory for FilesystemPromptFactory {
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
            return Err(invalid_plan("unsupported filesystem Prompt entrypoint"));
        }
        let config = serde_json::from_str::<FilesystemPromptConfig>(context.configuration())
            .map_err(|error| {
                invalid_plan(format!("invalid filesystem Prompt configuration: {error}"))
            })?;
        validate_config(&config)?;
        let state = Rc::new(RefCell::new(None));
        let endpoint = Rc::new(PromptProviderEndpoint::new(FilesystemPrompt {
            state: state.clone(),
        })) as Rc<dyn NativeRequestEndpoint>;
        Ok(NativeModuleInstance::with_lifecycle(
            vec![endpoint],
            FilesystemPromptLifecycle { config, state },
        ))
    }
}

#[derive(Clone, Debug)]
struct FilesystemPrompt {
    state: Rc<RefCell<Option<Vec<ContributeResponseContributionsItem>>>>,
}

impl PromptProviderProvider for FilesystemPrompt {
    fn contribute(
        &self,
        _context: InvocationContext,
        _request: ContributeRequest,
    ) -> lenso_kernel::NativeRequestFuture<lenso_capability_agent_prompt_provider::PromptProvider>
    {
        let result = self
            .state
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::Unavailable {
                capability: lenso_capability_agent_prompt_provider::CAPABILITY_ID,
            });
        Box::pin(ready(
            result.map(|contributions| Ok(ContributeResponse { contributions })),
        ))
    }
}

#[derive(Debug)]
struct FilesystemPromptLifecycle {
    config: FilesystemPromptConfig,
    state: Rc<RefCell<Option<Vec<ContributeResponseContributionsItem>>>>,
}

impl ModuleLifecycle for FilesystemPromptLifecycle {
    fn prepare(&self, _context: PrepareContext) -> ModuleFuture {
        let result = load_skills(&self.config);
        if let Ok(contributions) = &result {
            self.state.replace(Some(contributions.clone()));
        }
        Box::pin(ready(result.map(|_| ())))
    }
}

fn validate_config(config: &FilesystemPromptConfig) -> Result<(), RuntimeFailure> {
    if config.root.as_os_str().is_empty()
        || config.skills.is_empty()
        || config.skills.len() > 64
        || !valid_id(&config.id_prefix)
        || config.id_prefix.ends_with('/')
        || !(1..=65_536).contains(&config.max_file_bytes)
        || !(1..=262_144).contains(&config.max_total_bytes)
        || config.max_file_bytes > config.max_total_bytes
    {
        return Err(invalid_plan(
            "filesystem Prompt configuration limits are invalid",
        ));
    }
    let mut selected = BTreeSet::new();
    for skill in &config.skills {
        if !valid_path_component(skill) || !selected.insert(skill) {
            return Err(invalid_plan(format!(
                "invalid or duplicate filesystem Skill name `{skill}`"
            )));
        }
        if !valid_id(&format!("{}/{skill}", config.id_prefix)) {
            return Err(invalid_plan(format!(
                "filesystem Skill `{skill}` does not form a valid contribution id"
            )));
        }
    }
    Ok(())
}

fn load_skills(
    config: &FilesystemPromptConfig,
) -> Result<Vec<ContributeResponseContributionsItem>, RuntimeFailure> {
    validate_config(config)?;
    let configured_root = expand_home(&config.root)?;
    let root = fs::canonicalize(&configured_root).map_err(|error| {
        module_failure(format!(
            "filesystem Skill root `{}` is unavailable: {error}",
            configured_root.display()
        ))
    })?;
    if !root.is_dir() {
        return Err(module_failure("filesystem Skill root is not a directory"));
    }

    let mut total_bytes = 0_usize;
    let mut contributions = Vec::with_capacity(config.skills.len());
    for skill in &config.skills {
        let requested = root.join(skill).join("SKILL.md");
        let resolved = fs::canonicalize(&requested).map_err(|error| {
            module_failure(format!(
                "selected filesystem Skill `{skill}` is unavailable: {error}"
            ))
        })?;
        if !resolved.starts_with(&root) || !resolved.is_file() {
            return Err(module_failure(format!(
                "selected filesystem Skill `{skill}` escapes its configured root"
            )));
        }
        let metadata = fs::metadata(&resolved).map_err(|error| {
            module_failure(format!(
                "selected filesystem Skill `{skill}` metadata is unavailable: {error}"
            ))
        })?;
        let file_bytes = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        if file_bytes == 0 || file_bytes > config.max_file_bytes {
            return Err(module_failure(format!(
                "selected filesystem Skill `{skill}` exceeds its file limit"
            )));
        }
        let content = fs::read_to_string(&resolved).map_err(|error| {
            module_failure(format!(
                "selected filesystem Skill `{skill}` is not readable UTF-8: {error}"
            ))
        })?;
        if content.len() > config.max_file_bytes {
            return Err(module_failure(format!(
                "selected filesystem Skill `{skill}` exceeds its file limit"
            )));
        }
        let separator_bytes = usize::from(!contributions.is_empty()) * 2;
        total_bytes = total_bytes
            .saturating_add(separator_bytes)
            .saturating_add(content.len());
        if total_bytes > config.max_total_bytes {
            return Err(module_failure(
                "selected filesystem Skills exceed their aggregate limit",
            ));
        }
        validate_skill_document(skill, &content)?;
        contributions.push(ContributeResponseContributionsItem {
            id: format!("{}/{skill}", config.id_prefix),
            version: format!("{:x}", Sha256::digest(content.as_bytes())),
            kind: ContributeResponseContributionsItemKind::Skill,
            content,
        });
    }
    Ok(contributions)
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
            "filesystem Skill root supports only `~` or `~/...` home expansion",
        ));
    }
    Ok(path.to_path_buf())
}

fn validate_skill_document(skill: &str, content: &str) -> Result<(), RuntimeFailure> {
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return Err(module_failure(format!(
            "selected filesystem Skill `{skill}` is missing YAML frontmatter"
        )));
    }
    let mut has_name = false;
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        if line
            .strip_prefix("name:")
            .is_some_and(|name| !name.trim().is_empty())
        {
            has_name = true;
        }
    }
    if !closed || !has_name {
        return Err(module_failure(format!(
            "selected filesystem Skill `{skill}` has invalid YAML frontmatter"
        )));
    }
    Ok(())
}

fn valid_path_component(value: &str) -> bool {
    let path = Path::new(value);
    matches!(
        path.components().collect::<Vec<_>>().as_slice(),
        [Component::Normal(_)]
    ) && !value.starts_with('.')
        && value.len() <= 96
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'/' | b'-'))
        })
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

    fn config(root: &Path, skills: &[&str]) -> FilesystemPromptConfig {
        FilesystemPromptConfig {
            root: root.to_path_buf(),
            skills: skills.iter().map(|skill| (*skill).to_owned()).collect(),
            id_prefix: "agents.skills".to_owned(),
            max_file_bytes: 1024,
            max_total_bytes: 4096,
        }
    }

    fn write_skill(root: &Path, name: &str, body: &str) {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: fixture\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn loads_only_explicit_skills_in_configured_order() {
        let temporary = tempfile::tempdir().unwrap();
        write_skill(temporary.path(), "second", "Second instruction.");
        write_skill(temporary.path(), "first", "First instruction.");
        write_skill(temporary.path(), "ignored", "Ignored instruction.");

        let contributions = load_skills(&config(temporary.path(), &["second", "first"]))
            .expect("load selected Skills");
        assert_eq!(contributions.len(), 2);
        assert_eq!(contributions[0].id, "agents.skills/second");
        assert_eq!(contributions[1].id, "agents.skills/first");
        assert!(!contributions[0].content.contains("Ignored instruction."));
        assert_eq!(contributions[0].version.len(), 64);
    }

    #[test]
    fn rejects_missing_and_malformed_skill_documents() {
        let temporary = tempfile::tempdir().unwrap();
        write_skill(temporary.path(), "valid", "Valid.");
        fs::write(temporary.path().join("valid/SKILL.md"), "not a Skill").unwrap();
        assert!(load_skills(&config(temporary.path(), &["valid"])).is_err());
        assert!(load_skills(&config(temporary.path(), &["missing"])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_selected_skill_that_escapes_through_a_symlink() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        write_skill(&outside, "escaped", "Outside instruction.");
        symlink(outside.join("escaped"), root.join("escaped")).unwrap();

        assert!(load_skills(&config(&root, &["escaped"])).is_err());
    }

    #[test]
    fn rejects_traversal_and_duplicate_selections() {
        let temporary = tempfile::tempdir().unwrap();
        assert!(validate_config(&config(temporary.path(), &["../escape"])).is_err());
        assert!(validate_config(&config(temporary.path(), &["same", "same"])).is_err());
    }

    #[test]
    fn expands_the_agents_skill_root_below_the_current_home() {
        let expected = BaseDirs::new().unwrap().home_dir().join(".agents/skills");
        assert_eq!(
            expand_home(Path::new("~/.agents/skills")).unwrap(),
            expected
        );
    }
}
