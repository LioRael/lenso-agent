//! Minimal, local-first Plugin inventory for the Console Agent Host.

use lenso_agent_ask_user_tools_plugin as _;
use lenso_agent_auth_openai_codex_plugin as _;
use lenso_agent_context_compaction_plugin as _;
use lenso_agent_http_fetch_plugin as _;
use lenso_agent_loop_plugin as _;
use lenso_agent_memory_sqlite_plugin as _;
use lenso_agent_model_openai_codex_direct_plugin as _;
use lenso_agent_prompt_plugin as _;
use lenso_agent_prompt_static_plugin as _;
use lenso_agent_session_sqlite_plugin as _;
use lenso_agent_tools_plugin as _;
use lenso_agent_user_interaction_local_plugin as _;

/// Forces only the Console Agent's reviewed Plugin inventory into the Host executable.
pub fn link() {}
