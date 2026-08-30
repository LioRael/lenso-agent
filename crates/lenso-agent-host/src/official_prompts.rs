use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use lenso_app_plan::authoring::HostPluginConfiguration;

pub(crate) const CODING_INSTRUCTION: &str = r"# Coding workflow

Complete the requested change in the existing Workspace.

## Authority and scope

- Follow explicit user instructions and the loaded Workspace instructions. Apply loaded Workspace instructions broad-to-specific; when they conflict, the more specific instruction wins. Before changing a file below the current directory, inspect for a nearer `AGENTS.md` and follow it when present.
- Treat Tool availability, permission decisions, and isolation boundaries as runtime facts. Request additional authority only when the requested outcome requires it.
- Preserve unrelated user work. Treat explanation, review, diagnosis, and planning requests as read-only unless the user also asks for a change. Create commits, branches, pull requests, releases, or other external side effects only when explicitly requested.

## Workflow

1. Inspect the relevant definitions, registration points, call paths, adjacent patterns, tests, and configuration before editing. Use history when the current source does not explain intent.
2. When the approach is uncertain, risky, cross-cutting, or has dependent phases, form a short plan with observable completion criteria and keep it aligned with the work. Otherwise proceed directly.
3. Before the first file mutation, call `checkpoint_create`. Pass its checkpoint ID to every `edit` or `create_file` call.
4. Implement the smallest coherent root-cause change. Keep behavior consistent with the surrounding code and leave unrelated failures and cleanup out of the patch.
5. Call `checkpoint_review`, inspect the complete diff, and correct unintended changes. Validate the changed behavior first, then the affected package, then broader checks in proportion to risk. Inspect failures and iterate on their root causes; a failed command is evidence to diagnose, not success.
6. Complete the requested outcome. When checkpoint acceptance or restoration is not already explicit, ask the user to choose before calling `checkpoint_accept` or `checkpoint_restore`.

## Tool and communication discipline

- Prefer dedicated read, search, edit, Git, Skill, and subagent Tools. Use `run_process` for builds, tests, or work that dedicated Tools do not cover.
- Ask a question only when a missing decision would materially change the result or new authority is required. Otherwise make a reasonable, stated assumption and continue.
- Group related Tool calls. For longer work, report concise progress without narrating routine reads.
- In the final response, lead with the outcome. Name the important changed behavior, validation actually run, and any remaining blocker or unimplemented follow-up.";

const NATIVE_EXECUTION_INSTRUCTION: &str = "Native processes execute trusted project code under configured program, environment, root, argument, output, timeout, and cancellation bounds. They are not a security sandbox. Describe the effective restrictions precisely and do not imply hostile-code isolation.";

const SANDBOX_EXECUTION_INSTRUCTION: &str = "Processes execute through the selected operating-system sandbox. The default profile grants read-only host files, Workspace and private temporary writes, and no network. Treat the selected backend and runtime policy as authoritative, and do not claim isolation stronger than that backend provides.";

pub(crate) const PLAN_INSTRUCTION: &str = r"# Planning workflow

Work in strictly read-only planning mode. Produce a source-grounded plan that another agent can execute without rediscovering the design.

1. Establish the requested outcome, constraints, and explicit non-goals.
2. Inspect the actual source definitions, registration points, call paths, tests, configuration, and relevant history. Distinguish observed implementation from inference and proposal.
3. Identify the smallest coherent vertical slices. For each slice, name the affected components, behavior or contract change, failure handling, validation, and observable completion criterion.
4. Resolve discoverable questions from the Workspace. Surface only decisions that materially change the implementation or require new authority.
5. Lead the final response with the recommended direction, then the ordered implementation plan, validation strategy, risks, and remaining decisions.

Keep the Workspace and external state unchanged. Report investigation as investigation, and never claim files, commands, tests, or external actions were changed or completed.";

const LEGACY_CODING_CONFIGURATION: &str = "[[contributions]]\nid = \"harness.coding\"\nversion = \"1.1.0\"\nkind = \"instruction\"\ncontent = \"Work as a coding agent. Inspect before editing and preserve unrelated work. Before the first file mutation, create a Workspace checkpoint and pass its ID to every edit or create_file call. Review the checkpoint after changes, then ask the user to accept or restore it when that decision is not already explicit. Keep changes bounded and verify the result. Treat native processes as trusted execution, not as a security sandbox.\"\n";
const LEGACY_SANDBOX_CONFIGURATION: &str = "[[contributions]]\nid = \"harness.sandbox-coding\"\nversion = \"1.0.0\"\nkind = \"instruction\"\ncontent = \"Work as a coding agent inside the selected OS sandbox. Inspect before editing and preserve unrelated work. Before the first file mutation, create a Workspace checkpoint and pass its ID to every edit or create_file call. Review the checkpoint after changes, then ask the user to accept or restore it when that decision is not already explicit. Keep changes bounded and verify the result. The process sandbox grants read-only host files, Workspace and private temporary writes, and no network by default; do not claim stronger isolation than the selected backend provides.\"\n";
const LEGACY_PLAN_CONFIGURATION: &str = "[[contributions]]\nid = \"harness.plan\"\nversion = \"1.0.0\"\nkind = \"instruction\"\ncontent = \"Work in read-only planning mode. Inspect the workspace, explain evidence and tradeoffs, and produce an executable plan. Do not claim to have changed files or external state.\"\n";

const MANAGED_PROMPT_FILES: [(&str, &str); 3] = [
    (
        "plugins/lenso.agent.prompt.static/coding.toml",
        LEGACY_CODING_CONFIGURATION,
    ),
    (
        "plugins/lenso.agent.prompt.static/sandbox-coding.toml",
        LEGACY_SANDBOX_CONFIGURATION,
    ),
    (
        "plugins/lenso.agent.prompt.static/plan.toml",
        LEGACY_PLAN_CONFIGURATION,
    ),
];

pub(crate) fn configurations() -> [HostPluginConfiguration; 3] {
    [
        prompt_configuration(
            "coding",
            &serde_json::json!([
                {
                    "id": "harness.coding",
                    "version": "2.0.0",
                    "kind": "instruction",
                    "content": CODING_INSTRUCTION
                },
                {
                    "id": "harness.execution.native",
                    "version": "1.0.0",
                    "kind": "instruction",
                    "content": NATIVE_EXECUTION_INSTRUCTION
                }
            ]),
        ),
        prompt_configuration(
            "sandbox-coding",
            &serde_json::json!([
                {
                    "id": "harness.coding",
                    "version": "2.0.0",
                    "kind": "instruction",
                    "content": CODING_INSTRUCTION
                },
                {
                    "id": "harness.execution.sandbox",
                    "version": "1.0.0",
                    "kind": "instruction",
                    "content": SANDBOX_EXECUTION_INSTRUCTION
                }
            ]),
        ),
        prompt_configuration(
            "plan",
            &serde_json::json!([{
                "id": "harness.plan",
                "version": "2.0.0",
                "kind": "instruction",
                "content": PLAN_INSTRUCTION
            }]),
        ),
    ]
}

fn prompt_configuration(
    instance: &str,
    contributions: &serde_json::Value,
) -> HostPluginConfiguration {
    HostPluginConfiguration::new(
        "lenso.agent.prompt.static",
        instance,
        serde_json::json!({"contributions": contributions}),
    )
}

pub(crate) fn prepare_named_profile(home: &Path, profile: &str) -> Result<(), String> {
    if !matches!(profile, "code" | "code-sandbox" | "plan") {
        return Ok(());
    }
    migrate_legacy_official_files(home)
}

/// Migrates only byte-exact legacy official Prompt files to empty enabling
/// configurations. Any customized content remains an explicit local override.
pub fn migrate_legacy_official_files(home: &Path) -> Result<(), String> {
    for (relative, legacy) in MANAGED_PROMPT_FILES {
        let path = home.join(relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("failed to inspect {}: {error}", path.display())),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let current = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if current == legacy {
            replace_file(&path, "")?;
        }
    }
    Ok(())
}

fn replace_file(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("official Prompt path has no parent: {}", path.display()))?;
    let temporary = parent.join(format!(".lenso-official-prompt-{}", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("failed to create {}: {error}", temporary.display()))?;
    if let Err(error) = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(format!("failed to write {}: {error}", temporary.display()));
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("failed to migrate {}: {error}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_prompt_configurations_share_the_coding_core_and_keep_modes_distinct() {
        let configurations = configurations();
        let coding = configurations[0].configuration();
        let sandbox = configurations[1].configuration();
        let plan = configurations[2].configuration();

        assert_eq!(
            coding["contributions"][0]["content"],
            sandbox["contributions"][0]["content"]
        );
        assert_eq!(coding["contributions"][0]["content"], CODING_INSTRUCTION);
        assert_eq!(coding["contributions"][1]["id"], "harness.execution.native");
        assert_eq!(
            sandbox["contributions"][1]["id"],
            "harness.execution.sandbox"
        );
        assert_eq!(plan["contributions"][0]["content"], PLAN_INSTRUCTION);
        for required in [
            "nearer `AGENTS.md`",
            "call `checkpoint_create`",
            "smallest coherent root-cause change",
            "validation actually run",
        ] {
            assert!(CODING_INSTRUCTION.contains(required));
        }
        assert!(PLAN_INSTRUCTION.contains("strictly read-only planning mode"));
        assert!(PLAN_INSTRUCTION.contains("observable completion criterion"));
    }

    #[test]
    fn legacy_official_prompt_migrates_but_custom_content_remains() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("plugins/lenso.agent.prompt.static");
        fs::create_dir_all(&root).unwrap();
        let coding = root.join("coding.toml");
        let sandbox = root.join("sandbox-coding.toml");
        fs::write(&coding, LEGACY_CODING_CONFIGURATION).unwrap();
        fs::write(&sandbox, "# custom\n").unwrap();

        prepare_named_profile(home.path(), "code").unwrap();

        assert_eq!(fs::read_to_string(coding).unwrap(), "");
        assert_eq!(fs::read_to_string(sandbox).unwrap(), "# custom\n");
    }

    #[test]
    fn unrelated_profile_does_not_migrate_official_prompt_files() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("plugins/lenso.agent.prompt.static");
        fs::create_dir_all(&root).unwrap();
        let coding = root.join("coding.toml");
        fs::write(&coding, LEGACY_CODING_CONFIGURATION).unwrap();

        prepare_named_profile(home.path(), "custom").unwrap();

        assert_eq!(
            fs::read_to_string(coding).unwrap(),
            LEGACY_CODING_CONFIGURATION
        );
    }
}
