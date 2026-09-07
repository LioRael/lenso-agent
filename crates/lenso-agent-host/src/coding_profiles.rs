//! Reviewed official coding Profile authoring files shared by installers.

pub fn previous_official_profile(relative: &str, existing: &str, current: &str) -> bool {
    relative.starts_with("profiles/")
        && current.contains("include_enabled = true\n")
        && existing == current.replacen("include_enabled = true\n", "", 1)
}

#[allow(
    clippy::too_many_lines,
    reason = "one reviewed list keeps the official Profile installation auditable"
)]
pub fn coding_profile_files() -> Vec<(String, String)> {
    let mut files = vec![
        (
            "plugins/lenso.agent.workspace-instructions/default.toml",
            "working_directory = \".\"\nfile_name = \"AGENTS.md\"\nmax_ancestor_depth = 32\nmax_file_bytes = 262144\nmax_total_bytes = 1048576\n",
        ),
        (
            "plugins/lenso.agent.workspace-edit/default.toml",
            "root = \".\"\nmax_file_bytes = 1048576\nmax_edit_bytes = 131072\nrequire_checkpoint = true\n",
        ),
        (
            "plugins/lenso.agent.process.native/default.toml",
            "root = \".\"\nallowed_programs = [\"cargo\", \"git\", \"rg\"]\nprogram_presets = [\"rust\", \"javascript\", \"python\", \"go\", \"build\"]\nenvironment_allowlist = [\"PATH\", \"HOME\", \"CARGO_HOME\", \"RUSTUP_HOME\", \"TMPDIR\", \"LANG\", \"LC_ALL\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
        ),
        (
            "plugins/lenso.agent.process.sandbox/default.toml",
            "root = \".\"\nbackend = \"auto\"\nallow_network = false\nallowed_programs = [\"cargo\", \"git\", \"rg\"]\nprogram_presets = [\"rust\", \"javascript\", \"python\", \"go\", \"build\"]\nenvironment_allowlist = [\"PATH\", \"HOME\", \"CARGO_HOME\", \"RUSTUP_HOME\", \"LANG\", \"LC_ALL\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
        ),
        (
            "plugins/lenso.agent.process-tools/default.toml",
            "default_timeout_ms = 120000\nmax_background_processes = 8\nmax_background_log_bytes = 262144\n",
        ),
        (
            "plugins/lenso.agent.git-tools/default.toml",
            "default_timeout_ms = 30000\nmax_log_entries = 50\nmax_commit_message_bytes = 4096\nenable_branch_management = false\nenable_history_integration = false\nallowed_network_remotes = []\n",
        ),
        (
            "plugins/lenso.agent.code-mode-tools/default.toml",
            "max_code_bytes = 32768\nmax_instructions = 1000000\nmax_memory_bytes = 8388608\nmax_output_bytes = 262144\nmax_parallel_subcalls = 4\nmax_subcalls = 16\n",
        ),
        (
            "plugins/lenso.agent.subagent-tools/worktree.toml",
            "max_output_bytes = 1048576\nmax_task_bytes = 262144\nmax_tasks = 8\nrequire_worktree_provider = true\n",
        ),
        (
            "plugins/lenso.agent.worktree-provider/default.toml",
            "mutation_agents = [\"lenso.agent.loop/worker-a\", \"lenso.agent.loop/worker-b\"]\nmax_worktrees = 8\ntimeout_ms = 120000\nmax_review_bytes = 1048576\n",
        ),
        (
            "plugins/lenso.agent.loop/researcher.toml",
            "# Named read-only child Agent selected by the coding Profiles.\n",
        ),
        (
            "plugins/lenso.agent.loop/reviewer.toml",
            "# Named read-only child Agent selected by the coding Profiles.\n",
        ),
        (
            "plugins/lenso.agent.loop/worker-a.toml",
            "# Named mutation-capable child Agent isolated in its own Git worktree.\n",
        ),
        (
            "plugins/lenso.agent.loop/worker-b.toml",
            "# Second mutation-capable child lane for concurrent isolated work.\n",
        ),
        (
            "plugins/lenso.agent.tools/worker-tools.toml",
            "# Private Tool runtime for mutation-capable child Agents.\n",
        ),
        (
            "plugins/lenso.agent.interactive-approval-hook/default.toml",
            "default_decision = \"ask\"\nallow_tools = [\"read_text\", \"skill_list\", \"skill\", \"skill_resources\", \"skill_resource\", \"ask_user\", \"git_status\", \"git_diff\", \"git_log\", \"list_subagents\", \"list_worktrees\", \"review_worktree\", \"checkpoint_create\", \"checkpoint_review\"]\nask_tools = []\ndeny_tools = []\nmax_preview_bytes = 16384\n",
        ),
        (
            "profiles/code.toml",
            "description = \"Official coding agent with workspace instructions, isolated child worktrees, and inline approval\"\ninclude_enabled = true\ninstances = [\n  \"lenso.agent.workspace-instructions/default\",\n  \"lenso.agent.workspace-edit/default\",\n  \"lenso.agent.process.native/default\",\n  \"lenso.agent.process-tools/default\",\n  \"lenso.agent.git-tools/default\",\n  \"lenso.agent.code-mode-tools/default\",\n  \"lenso.agent.worktree-provider/default\",\n  \"lenso.agent.subagent-tools/worktree\",\n  \"lenso.agent.tools/worker-tools\",\n  \"lenso.agent.loop/researcher\",\n  \"lenso.agent.loop/reviewer\",\n  \"lenso.agent.loop/worker-a\",\n  \"lenso.agent.loop/worker-b\",\n  \"lenso.agent.interactive-approval-hook/default\",\n  \"lenso.agent.prompt.static/coding\",\n]\n",
        ),
        (
            "profiles/code-sandbox.toml",
            "description = \"Official coding agent with OS-isolated process execution and isolated child worktrees\"\ninclude_enabled = true\ninstances = [\n  \"lenso.agent.workspace-instructions/default\",\n  \"lenso.agent.workspace-edit/default\",\n  \"lenso.agent.process.sandbox/default\",\n  \"lenso.agent.process-tools/default\",\n  \"lenso.agent.git-tools/default\",\n  \"lenso.agent.code-mode-tools/default\",\n  \"lenso.agent.worktree-provider/default\",\n  \"lenso.agent.subagent-tools/worktree\",\n  \"lenso.agent.tools/worker-tools\",\n  \"lenso.agent.loop/researcher\",\n  \"lenso.agent.loop/reviewer\",\n  \"lenso.agent.loop/worker-a\",\n  \"lenso.agent.loop/worker-b\",\n  \"lenso.agent.interactive-approval-hook/default\",\n  \"lenso.agent.prompt.static/sandbox-coding\",\n]\n",
        ),
        (
            "profiles/plan.toml",
            "description = \"Official read-only planning agent\"\ninclude_enabled = true\ninstances = [\n  \"lenso.agent.workspace-instructions/default\",\n  \"lenso.agent.prompt.static/plan\",\n]\n",
        ),
    ]
    .into_iter()
    .map(|(path, content)| (path.to_owned(), content.to_owned()))
    .collect::<Vec<_>>();
    files.extend([
        (
            "plugins/lenso.agent.prompt.static/coding.toml".to_owned(),
            String::new(),
        ),
        (
            "plugins/lenso.agent.prompt.static/sandbox-coding.toml".to_owned(),
            String::new(),
        ),
        (
            "plugins/lenso.agent.prompt.static/plan.toml".to_owned(),
            String::new(),
        ),
    ]);
    files.extend(
        [
            "lenso.agent.workspace-instructions/default",
            "lenso.agent.workspace-edit/default",
            "lenso.agent.process.native/default",
            "lenso.agent.process.sandbox/default",
            "lenso.agent.process-tools/default",
            "lenso.agent.git-tools/default",
            "lenso.agent.code-mode-tools/default",
            "lenso.agent.subagent-tools/worktree",
            "lenso.agent.worktree-provider/default",
            "lenso.agent.loop/worker-a",
            "lenso.agent.loop/worker-b",
            "lenso.agent.tools/worker-tools",
            "lenso.agent.interactive-approval-hook/default",
            "lenso.agent.prompt.static/coding",
            "lenso.agent.prompt.static/sandbox-coding",
            "lenso.agent.prompt.static/plan",
        ]
        .into_iter()
        .map(|instance| {
            let (plugin, key) = instance
                .split_once('/')
                .expect("official Profile Instance has a key");
            (format!("plugins/{plugin}/{key}.disabled"), String::new())
        }),
    );
    files
}
