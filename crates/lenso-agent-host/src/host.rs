use std::{fmt::Debug, path::PathBuf};

use crate::{generation::AgentApp, plan_bytes_for_profile};

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
    fn control_directory(&self) -> &'static str;
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
    fn control_directory(&self) -> &'static str {
        crate::generation::CONTROL_DIRECTORY
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
    fn control_directory(&self) -> &'static str {
        crate::generation::TUI_CONTROL_DIRECTORY
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
    fn control_directory(&self) -> &'static str {
        crate::generation::CHANNEL_CONTROL_DIRECTORY
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
    fn control_directory(&self) -> &'static str {
        crate::generation::TELEGRAM_CONTROL_DIRECTORY
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
    fn control_directory(&self) -> &'static str {
        crate::generation::DISCORD_CONTROL_DIRECTORY
    }
}

/// Entry point for composing an Agent Harness binary.
#[derive(Clone, Copy, Debug, Default)]
pub struct AgentHost;

impl AgentHost {
    pub const fn builder() -> AgentHostBuilder<()> {
        AgentHostBuilder { surface: () }
    }
}

/// Compile-time composition of linked Plugins and one process-owned surface.
#[derive(Debug)]
pub struct AgentHostBuilder<S> {
    surface: S,
}

impl<S> AgentHostBuilder<S> {
    /// Links one Plugin set into this Host Build.
    #[must_use]
    pub fn plugins(self, link: fn()) -> Self {
        link();
        self
    }

    /// Selects the process-owned surface without turning it into a Plugin.
    pub fn surface<T: AgentSurface>(self, surface: T) -> AgentHostBuilder<T> {
        AgentHostBuilder { surface }
    }
}

impl<S: AgentSurface> AgentHostBuilder<S> {
    /// Validates the static Host composition. Runtime Plan resolution happens in `run`.
    pub fn build(self) -> Result<ConfiguredAgentHost<S>, String> {
        validate_control_directory(self.surface.control_directory())?;
        Ok(ConfiguredAgentHost {
            surface: self.surface,
        })
    }
}

/// A validated Agent Host Build ready to run one Profile.
#[derive(Debug)]
pub struct ConfiguredAgentHost<S> {
    surface: S,
}

impl<S: AgentSurface> ConfiguredAgentHost<S> {
    /// Resolves the selected Profile and starts one immutable App Generation.
    pub async fn run(self, profile: Profile) -> Result<AgentApp, String> {
        let (plan, profile_name) = match profile {
            Profile::Default => (None, None),
            Profile::Named(name) => (None, Some(name)),
            Profile::ResolvedPlan(path) => (Some(path), None),
        };
        let bytes = plan_bytes_for_profile(plan.as_deref(), profile_name.as_deref())
            .map_err(|error| format!("App resolution failed: {error}"))?;
        AgentApp::start_with_store_control_directory_profile_and_host_build(
            &bytes,
            std::path::Path::new(".lenso/runtime"),
            self.surface.control_directory(),
            profile_name,
            crate::generation::HostBuildIdentity::current()?,
        )
        .await
        .map_err(|error| format!("App startup failed: {error}"))
    }
}

fn validate_control_directory(directory: &str) -> Result<(), String> {
    if directory.is_empty()
        || directory == "."
        || directory == ".."
        || directory.contains(['/', '\\'])
    {
        return Err(format!(
            "invalid Agent surface control directory `{directory}`"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    static LINKED: AtomicBool = AtomicBool::new(false);

    fn link_test_plugins() {
        LINKED.store(true, Ordering::SeqCst);
    }

    #[derive(Debug)]
    struct UnsafeSurface;

    impl AgentSurface for UnsafeSurface {
        fn control_directory(&self) -> &'static str {
            "../shared"
        }
    }

    #[test]
    fn surface_cannot_escape_runtime_control_root() {
        let result = AgentHost::builder().surface(UnsafeSurface).build();
        assert_eq!(
            result.expect_err("unsafe surface must be rejected"),
            "invalid Agent surface control directory `../shared`"
        );
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
