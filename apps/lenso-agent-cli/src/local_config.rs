use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const LOCAL_CONFIGURATION_SCHEMA_VERSION: u32 = 1;
const LOCAL_CONFIGURATION_FILE: &str = "lenso.local.toml";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalPluginConfiguration {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) enabled: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalConfiguration {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "LocalPluginConfiguration::is_empty")]
    pub(crate) plugins: LocalPluginConfiguration,
}

impl Default for LocalConfiguration {
    fn default() -> Self {
        Self {
            schema_version: LOCAL_CONFIGURATION_SCHEMA_VERSION,
            plugins: LocalPluginConfiguration::default(),
        }
    }
}

impl LocalPluginConfiguration {
    fn is_empty(&self) -> bool {
        self.enabled.is_empty()
    }
}

impl LocalConfiguration {
    fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

pub(crate) struct LocalConfigurationSnapshot {
    path: PathBuf,
    expected_bytes: Option<Vec<u8>>,
    pub(crate) configuration: LocalConfiguration,
}

impl LocalConfigurationSnapshot {
    pub(crate) fn exists(&self) -> bool {
        self.expected_bytes.is_some()
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn write(&self, next: &LocalConfiguration) -> Result<(), String> {
        let current = match fs::read(&self.path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!("failed to read {}: {error}", self.path.display()));
            }
        };
        if current != self.expected_bytes {
            return Err(format!(
                "{} changed while the candidate Generation was being checked; retry the command",
                self.path.display()
            ));
        }
        if next.is_empty() {
            if current.is_some() {
                fs::remove_file(&self.path).map_err(|error| {
                    format!("failed to remove {}: {error}", self.path.display())
                })?;
            }
            return Ok(());
        }

        let bytes = toml::to_string_pretty(next)
            .map_err(|error| format!("failed to encode {}: {error}", self.path.display()))?;
        let temporary = self
            .path
            .with_extension(format!("local-{}.tmp", std::process::id()));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|error| format!("failed to create {}: {error}", temporary.display()))?;
            file.write_all(bytes.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
            fs::rename(&temporary, &self.path).map_err(|error| {
                format!(
                    "failed to replace {} with checked local configuration: {error}",
                    self.path.display()
                )
            })
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result
    }
}

pub(crate) fn load(definition: &Path) -> Result<LocalConfigurationSnapshot, String> {
    let path = definition
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(LOCAL_CONFIGURATION_FILE);
    let (expected_bytes, configuration) = match fs::read(&path) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| format!("invalid UTF-8 in {}: {error}", path.display()))?;
            let configuration = toml::from_str::<LocalConfiguration>(text)
                .map_err(|error| format!("invalid {}: {error}", path.display()))?;
            if configuration.schema_version != LOCAL_CONFIGURATION_SCHEMA_VERSION {
                return Err(format!(
                    "unsupported local configuration schema version {} in {}; expected {LOCAL_CONFIGURATION_SCHEMA_VERSION}",
                    configuration.schema_version,
                    path.display()
                ));
            }
            (Some(bytes), configuration)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (None, LocalConfiguration::default())
        }
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    Ok(LocalConfigurationSnapshot {
        path,
        expected_bytes,
        configuration,
    })
}
