use std::{fmt::Debug, path::PathBuf, sync::Arc};

use lenso_app_authoring::{PluginConfigurationAuthority, PluginSelectionAuthority};

use crate::{
    AgentDirectories, AgentToolTarget, PluginManagementTarget, generation::AgentApp,
    plan_bytes_for_profile_in,
};

/// Selects the product configuration for one Agent Host session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Profile {
    /// Uses every configured Plugin instance from the Plugin Root.
    #[default]
    Default,
    /// Uses one named profile while retaining each selected instance's own configuration.
    Named(String),
    /// Runs one exact resolved Plan as a diagnostic override.
    ResolvedPlan(PathBuf),
}

impl Profile {
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }

    pub fn resolved_plan(path: impl Into<PathBuf>) -> Self {
        Self::ResolvedPlan(path.into())
    }
}

/// Describes a process-owned presentation surface and its independent Controller lineage.
pub trait AgentSurface: Debug {
    fn kind(&self) -> AgentSurfaceKind;
}

/// Logical process-owned presentation surface. Durable lineage and lease
/// layout remain private Host policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentSurfaceKind {
    Acp,
    Headless,
    Tui,
    Channels,
    Telegram,
    Discord,
    Web,
}

/// The editor-facing Agent Client Protocol stdio surface.
#[derive(Clone, Copy, Debug, Default)]
pub struct AcpSurface;

impl AcpSurface {
    pub const fn stdio() -> Self {
        Self
    }
}

impl AgentSurface for AcpSurface {
    fn kind(&self) -> AgentSurfaceKind {
        AgentSurfaceKind::Acp
    }
}

/// A one-shot stdin/stdout or programmatic Agent surface.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeadlessSurface;

impl HeadlessSurface {
    pub const fn stdio() -> Self {
        Self
    }
}

impl AgentSurface for HeadlessSurface {
    fn kind(&self) -> AgentSurfaceKind {
        AgentSurfaceKind::Headless
    }
}

/// The interactive terminal surface.
#[derive(Clone, Copy, Debug, Default)]
pub struct TuiSurface;

impl TuiSurface {
    pub const fn terminal() -> Self {
        Self
    }
}

impl AgentSurface for TuiSurface {
    fn kind(&self) -> AgentSurfaceKind {
        AgentSurfaceKind::Tui
    }
}

/// All configured messaging channels in one process.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChannelSurface;

impl ChannelSurface {
    pub const fn messaging() -> Self {
        Self
    }
}

impl AgentSurface for ChannelSurface {
    fn kind(&self) -> AgentSurfaceKind {
        AgentSurfaceKind::Channels
    }
}

/// A Telegram-only process surface.
#[derive(Clone, Copy, Debug, Default)]
pub struct TelegramSurface;

impl TelegramSurface {
    pub const fn messaging() -> Self {
        Self
    }
}

impl AgentSurface for TelegramSurface {
    fn kind(&self) -> AgentSurfaceKind {
        AgentSurfaceKind::Telegram
    }
}

/// A Discord-only process surface.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiscordSurface;

impl DiscordSurface {
    pub const fn messaging() -> Self {
        Self
    }
}

impl AgentSurface for DiscordSurface {
    fn kind(&self) -> AgentSurfaceKind {
        AgentSurfaceKind::Discord
    }
}

/// The browser-based Agent surface.
#[derive(Clone, Copy, Debug, Default)]
pub struct WebSurface;

impl WebSurface {
    pub const fn browser() -> Self {
        Self
    }
}

impl AgentSurface for WebSurface {
    fn kind(&self) -> AgentSurfaceKind {
        AgentSurfaceKind::Web
    }
}

/// Entry point for composing a Lenso Agent binary.
#[derive(Clone, Copy, Debug, Default)]
pub struct AgentHost;

impl AgentHost {
    pub const fn builder() -> AgentHostBuilder<()> {
        AgentHostBuilder {
            directories: None,
            plugin_configuration_authority: None,
            plugin_management_target: None,
            agent_tool_target: None,
            plugin_selection_authority: None,
            surface: (),
        }
    }
}

/// Compile-time composition of linked Plugins and one process-owned surface.
#[derive(Debug)]
pub struct AgentHostBuilder<S> {
    directories: Option<AgentDirectories>,
    plugin_configuration_authority: Option<Arc<dyn PluginConfigurationAuthority>>,
    plugin_management_target: Option<Arc<dyn PluginManagementTarget>>,
    agent_tool_target: Option<Arc<dyn AgentToolTarget>>,
    plugin_selection_authority: Option<Arc<dyn PluginSelectionAuthority>>,
    surface: S,
}

impl<S> AgentHostBuilder<S> {
    /// Uses one explicit Agent Home instead of `LENSO_AGENT_HOME` or `~/.lenso/agent`.
    pub fn agent_home(mut self, home: impl Into<PathBuf>) -> Result<Self, String> {
        self.directories = Some(AgentDirectories::from_home(home)?);
        Ok(self)
    }

    /// Links one Plugin set into this Host Build.
    #[must_use]
    pub fn plugins(self, link: fn()) -> Self {
        link();
        self
    }

    /// Supplies the Host-owned authority used by Plugin configuration capability consumers.
    #[must_use]
    pub fn plugin_configuration_authority(
        mut self,
        authority: Arc<dyn PluginConfigurationAuthority>,
    ) -> Self {
        self.plugin_configuration_authority = Some(authority);
        self
    }

    /// Supplies the Host-owned authority used by Plugin selection capability consumers.
    #[must_use]
    pub fn plugin_selection_authority(
        mut self,
        authority: Arc<dyn PluginSelectionAuthority>,
    ) -> Self {
        self.plugin_selection_authority = Some(authority);
        self
    }

    /// Supplies non-local Agent-qualified Plugin management targets to Console Agent Tools.
    #[must_use]
    pub fn plugin_management_target(mut self, target: Arc<dyn PluginManagementTarget>) -> Self {
        self.plugin_management_target = Some(target);
        self
    }

    /// Supplies the Host-owned App Agent Tool routing target to Console Agent Plugins.
    #[must_use]
    pub fn agent_tool_target(mut self, target: Arc<dyn AgentToolTarget>) -> Self {
        self.agent_tool_target = Some(target);
        self
    }

    /// Selects the process-owned surface without turning it into a Plugin.
    pub fn surface<T: AgentSurface>(self, surface: T) -> AgentHostBuilder<T> {
        AgentHostBuilder {
            directories: self.directories,
            plugin_configuration_authority: self.plugin_configuration_authority,
            plugin_management_target: self.plugin_management_target,
            agent_tool_target: self.agent_tool_target,
            plugin_selection_authority: self.plugin_selection_authority,
            surface,
        }
    }
}

impl<S: AgentSurface> AgentHostBuilder<S> {
    /// Validates the static Host composition. Runtime Plan resolution happens in `run`.
    pub fn build(self) -> Result<ConfiguredAgentHost<S>, String> {
        Ok(ConfiguredAgentHost {
            directories: self
                .directories
                .map_or_else(AgentDirectories::resolve, Ok)?,
            plugin_configuration_authority: self.plugin_configuration_authority,
            plugin_management_target: self.plugin_management_target,
            agent_tool_target: self.agent_tool_target,
            plugin_selection_authority: self.plugin_selection_authority,
            surface: self.surface,
        })
    }
}

/// A validated Agent Host Build ready to run one Profile.
#[derive(Debug)]
pub struct ConfiguredAgentHost<S> {
    directories: AgentDirectories,
    plugin_configuration_authority: Option<Arc<dyn PluginConfigurationAuthority>>,
    plugin_management_target: Option<Arc<dyn PluginManagementTarget>>,
    agent_tool_target: Option<Arc<dyn AgentToolTarget>>,
    plugin_selection_authority: Option<Arc<dyn PluginSelectionAuthority>>,
    surface: S,
}

impl<S: AgentSurface> ConfiguredAgentHost<S> {
    /// Publishes this exact Host Build Catalog before external authoring reconciliation.
    ///
    /// This does not resolve a Plan or start a Generation. Hosts with an
    /// external Plugin configuration authority use it to validate and
    /// materialize desired configuration before `run` performs initial App
    /// resolution.
    pub fn prepare_authoring(&self) -> Result<(), String> {
        crate::ensure_host_catalog(&self.directories)
    }

    /// Supplies the Host-owned authority after authoring preparation has made
    /// the current Host Catalog available to authority implementations.
    #[must_use]
    pub fn plugin_configuration_authority(
        mut self,
        authority: Arc<dyn PluginConfigurationAuthority>,
    ) -> Self {
        self.plugin_configuration_authority = Some(authority);
        self
    }

    /// Supplies the Host-owned Plugin selection authority after authoring preparation.
    #[must_use]
    pub fn plugin_selection_authority(
        mut self,
        authority: Arc<dyn PluginSelectionAuthority>,
    ) -> Self {
        self.plugin_selection_authority = Some(authority);
        self
    }

    /// Supplies non-local Agent-qualified Plugin management targets after authoring preparation.
    #[must_use]
    pub fn plugin_management_target(mut self, target: Arc<dyn PluginManagementTarget>) -> Self {
        self.plugin_management_target = Some(target);
        self
    }

    /// Supplies the Host-owned App Agent Tool target after authoring preparation.
    #[must_use]
    pub fn agent_tool_target(mut self, target: Arc<dyn AgentToolTarget>) -> Self {
        self.agent_tool_target = Some(target);
        self
    }

    /// Resolves the selected Profile and starts one immutable App Generation.
    pub async fn run(self, profile: Profile) -> Result<AgentApp, String> {
        let (plan, profile_name) = match profile {
            Profile::Default => (None, None),
            Profile::Named(name) => (None, Some(name)),
            Profile::ResolvedPlan(path) => (Some(path), None),
        };
        let bytes =
            plan_bytes_for_profile_in(&self.directories, plan.as_deref(), profile_name.as_deref())
                .map_err(|error| format!("App resolution failed: {error}"))?;
        AgentApp::start_with_runtime_state_profile_and_host_build(
            &bytes,
            &self.directories.runtime(),
            self.directories.session_database(),
            self.surface.kind(),
            profile_name,
            crate::generation::HostBuildIdentity::current()?,
            crate::generation::PluginAuthoringAuthorities {
                configuration: self.plugin_configuration_authority,
                management_target: self.plugin_management_target,
                tool_target: self.agent_tool_target,
                selection: self.plugin_selection_authority,
            },
        )
        .await
        .map_err(|error| format!("App startup failed: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    static LINKED: AtomicBool = AtomicBool::new(false);

    fn link_test_plugins() {
        LINKED.store(true, Ordering::SeqCst);
    }

    #[test]
    fn builder_links_the_selected_plugin_set() {
        LINKED.store(false, Ordering::SeqCst);
        let _host = AgentHost::builder()
            .plugins(link_test_plugins)
            .surface(HeadlessSurface::stdio())
            .build()
            .expect("valid Host composition should build");
        assert!(LINKED.load(Ordering::SeqCst));
    }
}
