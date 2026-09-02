//! Minimal, local-first Plugin inventory for the Console Agent Host.

use lenso_agent_artifact_file_plugin as _;
use lenso_agent_ask_user_tools_plugin as _;
use lenso_agent_auth_openai_codex_plugin as _;
use lenso_agent_console_instructions_plugin as _;
use lenso_agent_context_compaction_plugin as _;
use lenso_agent_http_fetch_plugin as _;
use lenso_agent_interactive_approval_hook_plugin as _;
use lenso_agent_loop_plugin as _;
use lenso_agent_memory_sqlite_plugin as _;
use lenso_agent_model_openai_codex_direct_plugin as _;
use lenso_agent_model_selection_plugin as _;
use lenso_agent_oauth_client_credentials_plugin as _;
use lenso_agent_prompt_plugin as _;
use lenso_agent_prompt_static_plugin as _;
use lenso_agent_session_sqlite_plugin as _;
use lenso_agent_tools_plugin as _;
use lenso_agent_user_interaction_local_plugin as _;

pub use lenso_agent_console_plugin_tools_plugin::{
    APPLY_PLUGIN_CHANGE_TOOL, CHECK_PLUGIN_CHANGE_TOOL, INSPECT_APP_TOOL, INSPECT_PLUGIN_TOOL,
    LIST_PLUGINS_TOOL,
};

/// First-party Plugin management Tools available to the Console Agent.
pub const PLUGIN_CONTROL_TOOLS: [&str; 5] = [
    INSPECT_APP_TOOL,
    LIST_PLUGINS_TOOL,
    INSPECT_PLUGIN_TOOL,
    CHECK_PLUGIN_CHANGE_TOOL,
    APPLY_PLUGIN_CHANGE_TOOL,
];

/// Forces only the Console Agent's reviewed Plugin inventory into the Host executable.
pub fn link() {}
