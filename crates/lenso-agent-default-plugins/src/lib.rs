//! Standard surface-neutral Plugin linkage for the distributed Agent Hosts.

use lenso_agent_approval_hook_plugin as _;
use lenso_agent_ask_user_tools_plugin as _;
use lenso_agent_auth_openai_codex_plugin as _;
use lenso_agent_code_mode_tools_plugin as _;
use lenso_agent_context_compaction_command_plugin as _;
use lenso_agent_context_compaction_plugin as _;
use lenso_agent_git_tools_plugin as _;
use lenso_agent_http_fetch_plugin as _;
use lenso_agent_lifecycle_audit_plugin as _;
use lenso_agent_lifecycle_command_plugin as _;
use lenso_agent_loop_plugin as _;
use lenso_agent_mcp_client_plugin as _;
use lenso_agent_memory_command_plugin as _;
use lenso_agent_memory_sqlite_plugin as _;
use lenso_agent_model_fixture_plugin as _;
use lenso_agent_model_openai_codex_direct_plugin as _;
use lenso_agent_model_openai_compatible_plugin as _;
use lenso_agent_process_native_plugin as _;
use lenso_agent_process_tools_plugin as _;
use lenso_agent_prompt_filesystem_plugin as _;
use lenso_agent_prompt_plugin as _;
use lenso_agent_prompt_static_plugin as _;
use lenso_agent_session_file_plugin as _;
use lenso_agent_session_presentation_model_plugin as _;
use lenso_agent_session_presentation_plugin as _;
use lenso_agent_session_sqlite_plugin as _;
use lenso_agent_skills_filesystem_plugin as _;
use lenso_agent_subagent_tools_plugin as _;
use lenso_agent_text_tools_plugin as _;
use lenso_agent_tools_plugin as _;
use lenso_agent_user_interaction_local_plugin as _;
use lenso_agent_workspace_edit_plugin as _;
use lenso_agent_workspace_import_read_plugin as _;
use lenso_agent_workspace_read_plugin as _;
use lenso_agent_workspace_read_tools_plugin as _;
use lenso_secrets_command_plugin as _;
use lenso_secrets_encrypted_file_plugin as _;
use lenso_secrets_env_plugin as _;
use lenso_secrets_keychain_plugin as _;

/// Forces the standard Plugin inventory into the final Host executable.
pub fn link() {}
