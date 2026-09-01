use std::{
    env,
    path::{Path, PathBuf},
};

use directories::BaseDirs;

/// Environment override for the Lenso Agent's user-owned configuration and state root.
pub const AGENT_HOME_ENV: &str = "LENSO_AGENT_HOME";

/// Stable user-owned paths shared by every Lenso Agent surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentDirectories {
    home: PathBuf,
}

impl AgentDirectories {
    /// Resolves the configured Agent Home or defaults to `~/.lenso/agent`.
    pub fn resolve() -> Result<Self, String> {
        if let Some(home) = env::var_os(AGENT_HOME_ENV) {
            return Self::from_home(home);
        }
        let base =
            BaseDirs::new().ok_or_else(|| "the user home directory is unavailable".to_owned())?;
        Self::from_home(base.home_dir().join(".lenso/agent"))
    }

    /// Builds one explicit Agent Home, primarily for launchers and isolated tests.
    pub fn from_home(home: impl Into<PathBuf>) -> Result<Self, String> {
        let home = home.into();
        if !home.is_absolute() {
            return Err(format!(
                "{AGENT_HOME_ENV} must be an absolute path: {}",
                home.display()
            ));
        }
        if home.to_str().is_none() {
            return Err(format!(
                "{AGENT_HOME_ENV} must be valid UTF-8: {}",
                home.display()
            ));
        }
        Ok(Self { home })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn plugins(&self) -> PathBuf {
        self.home.join("plugins")
    }

    pub fn profiles(&self) -> PathBuf {
        self.home.join("profiles")
    }

    pub fn runtime(&self) -> PathBuf {
        self.home.join("runtime")
    }

    pub fn host_catalog(&self) -> PathBuf {
        self.home.join(".lenso/host-catalog.json")
    }

    pub fn sessions(&self) -> PathBuf {
        self.home.join("sessions")
    }

    pub fn artifacts(&self) -> PathBuf {
        self.home.join("artifacts")
    }

    pub fn session_database(&self) -> PathBuf {
        self.home.join("sessions.sqlite3")
    }

    pub fn memory_database(&self) -> PathBuf {
        self.home.join("memory.sqlite3")
    }

    pub fn lifecycle_events(&self) -> PathBuf {
        self.home.join("lifecycle/events.jsonl")
    }

    pub fn approvals(&self) -> PathBuf {
        self.home.join("approvals")
    }

    pub fn auth(&self) -> PathBuf {
        self.home.join("auth.json")
    }

    pub fn model_catalog_cache(&self) -> PathBuf {
        self.home
            .join("runtime/model-catalog/openai-codex-direct.json")
    }

    pub fn model_catalog_snapshot(&self) -> PathBuf {
        self.home
            .join("runtime/model-catalog/effective/openai-codex-direct.json")
    }

    pub fn channels(&self) -> PathBuf {
        self.home.join("channels.toml")
    }

    pub fn telegram_state(&self) -> PathBuf {
        self.home.join("telegram/state.json")
    }

    pub fn discord_state(&self) -> PathBuf {
        self.home.join("discord/state.json")
    }

    pub fn encrypted_secrets(&self) -> PathBuf {
        self.home.join("secrets.age")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_home_derives_every_user_owned_path() {
        let directories = AgentDirectories::from_home(PathBuf::from("/tmp/lenso-agent-home"))
            .expect("absolute test home");
        assert_eq!(
            directories.plugins(),
            Path::new("/tmp/lenso-agent-home/plugins")
        );
        assert_eq!(
            directories.profiles(),
            Path::new("/tmp/lenso-agent-home/profiles")
        );
        assert_eq!(
            directories.runtime(),
            Path::new("/tmp/lenso-agent-home/runtime")
        );
        assert_eq!(
            directories.host_catalog(),
            Path::new("/tmp/lenso-agent-home/.lenso/host-catalog.json")
        );
        assert_eq!(
            directories.session_database(),
            Path::new("/tmp/lenso-agent-home/sessions.sqlite3")
        );
        assert_eq!(
            directories.artifacts(),
            Path::new("/tmp/lenso-agent-home/artifacts")
        );
        assert_eq!(
            directories.model_catalog_cache(),
            Path::new("/tmp/lenso-agent-home/runtime/model-catalog/openai-codex-direct.json")
        );
        assert_eq!(
            directories.model_catalog_snapshot(),
            Path::new(
                "/tmp/lenso-agent-home/runtime/model-catalog/effective/openai-codex-direct.json"
            )
        );
    }

    #[test]
    fn relative_home_fails_closed() {
        let error = AgentDirectories::from_home("relative/home").unwrap_err();
        assert!(error.contains("must be an absolute path"));
    }
}
