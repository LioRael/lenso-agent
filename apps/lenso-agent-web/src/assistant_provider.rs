//! Console member provider selection. Identity is supplied by verified ingress,
//! never decoded from a settings payload. Provider Hosts are immutable leases.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Mutex,
};

use age::secrecy::SecretString;
use lenso_agent_host::{AgentHost, Profile, WebSurface, generation::AgentApp};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Host-owned settings. An empty policy denies all member provider access.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantProviderConfig {
    #[serde(default)]
    pub providers: Vec<AssistantProviderDefinition>,
    #[serde(default)]
    pub shared_provider_id: Option<String>,
    /// Keys use the exact JSON [issuer, subject] identity from verified ingress.
    #[serde(default)]
    pub users: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub groups: BTreeMap<String, AssistantProviderGroup>,
    #[serde(default)]
    pub byok_enabled: bool,
    pub settings_database: PathBuf,
    #[serde(default)]
    pub byok_root: Option<PathBuf>,
    #[serde(default = "default_user_revisions")]
    pub max_byok_revisions_per_user: usize,
    #[serde(default = "default_total_revisions")]
    pub max_byok_revisions_total: usize,
    #[serde(default = "default_cached_hosts")]
    pub max_cached_provider_hosts: usize,
}

const fn default_user_revisions() -> usize {
    8
}
const fn default_total_revisions() -> usize {
    128
}
const fn default_cached_hosts() -> usize {
    32
}
impl Default for AssistantProviderConfig {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            shared_provider_id: None,
            users: BTreeMap::new(),
            groups: BTreeMap::new(),
            byok_enabled: false,
            settings_database: PathBuf::new(),
            byok_root: None,
            max_byok_revisions_per_user: default_user_revisions(),
            max_byok_revisions_total: default_total_revisions(),
            max_cached_provider_hosts: default_cached_hosts(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantProviderGroup {
    pub owners: Vec<String>,
    pub providers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantProviderDefinition {
    pub id: String,
    pub label: String,
    /// Separately prepared Host Home. Session Plugin must point at the shared,
    /// authenticated database; each Home owns its separate runtime ledger.
    pub agent_home: PathBuf,
    #[serde(default)]
    pub profile: Option<String>,
    pub model: String,
    /// Per-provider member Tool ceiling; this slice accepts only `ask_user`,
    /// whose provider verifies the signed owner. Intersected with Host grants.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub byok: Option<ByokTemplate>,
}

/// BYOK copies only this trusted provider's Plugin Root into a new immutable Home.
/// Its Secrets Plugin resolves a logical reference from an encrypted file.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ByokTemplate {
    pub secrets_instance: String,
    pub credential_reference: String,
    pub key_environment_variable: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssistantProviderOption {
    pub id: String,
    pub label: String,
    pub model: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssistantSettings {
    pub providers: Vec<AssistantProviderOption>,
    pub selected_provider_id: Option<String>,
    pub byok_enabled: bool,
    pub has_byok: bool,
}

/// Presence differs from null: omission keeps BYOK, null removes the selected
/// provider's personal credential. JSON never carries an owner or endpoint.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantSettingsUpdate {
    /// Omission preserves the preference; null resets to the configured default.
    #[serde(default, deserialize_with = "deserialize_provider_change")]
    pub provider_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_byok_change")]
    pub byok: Option<Option<ByokUpdate>>,
}

#[allow(
    clippy::option_option,
    reason = "JSON omission, explicit null and replacement are distinct patch operations"
)]
fn deserialize_provider_change<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

#[allow(
    clippy::option_option,
    reason = "JSON omission retains credentials while explicit null removes them"
)]
fn deserialize_byok_change<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<ByokUpdate>>, D::Error> {
    Option::<ByokUpdate>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByokUpdate {
    pub provider_id: String,
    pub api_key: String,
}

impl fmt::Debug for ByokUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ByokUpdate")
            .field("provider_id", &self.provider_id)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for AssistantSettingsUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssistantSettingsUpdate")
            .field("provider_id", &self.provider_id)
            .field(
                "byok",
                &self.byok.as_ref().map(|v| v.as_ref().map(|_| "[REDACTED]")),
            )
            .finish()
    }
}

impl Drop for ByokUpdate {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.api_key);
    }
}

#[derive(Clone, Debug)]
pub struct ProviderRoute {
    pub id: String,
    pub agent_home: PathBuf,
    pub profile: Option<String>,
    pub model: String,
    pub allowed_tools: Vec<String>,
    /// Versioned key prevents credential changes from replacing an active lease.
    pub lease_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderSettingsError {
    PermissionDenied,
    InvalidSettings,
    Unavailable,
    CapacityExhausted,
}

impl fmt::Display for ProviderSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PermissionDenied => "permission_denied",
            Self::InvalidSettings => "invalid_settings",
            Self::Unavailable => "provider_settings_unavailable",
            Self::CapacityExhausted => "provider_settings_capacity_exhausted",
        })
    }
}
impl std::error::Error for ProviderSettingsError {}

#[derive(Serialize)]
struct SecretDocument<'a> {
    version: u32,
    secrets: BTreeMap<&'static str, &'a str>,
}

pub struct AssistantProviderService {
    config: AssistantProviderConfig,
    database: Mutex<Connection>,
    encryption_key_source: fn(&str) -> Result<Zeroizing<String>, ProviderSettingsError>,
}
impl fmt::Debug for AssistantProviderService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssistantProviderService")
            .field("providers", &self.config.providers.len())
            .finish_non_exhaustive()
    }
}

impl AssistantProviderService {
    pub fn open(config: AssistantProviderConfig) -> Result<Self, ProviderSettingsError> {
        validate_config(&config)?;
        if let Some(parent) = config.settings_database.parent() {
            fs::create_dir_all(parent).map_err(|_| ProviderSettingsError::Unavailable)?;
        }
        let database = Connection::open(&config.settings_database)
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        database
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        database.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS assistant_provider_selection(owner TEXT PRIMARY KEY, provider_id TEXT NOT NULL); CREATE TABLE IF NOT EXISTS assistant_provider_byok(owner TEXT NOT NULL, provider_id TEXT NOT NULL, lease_key TEXT NOT NULL, home TEXT NOT NULL, PRIMARY KEY(owner,provider_id)); CREATE TABLE IF NOT EXISTS assistant_provider_byok_revisions(lease_key TEXT PRIMARY KEY, owner TEXT NOT NULL, home TEXT NOT NULL, committed INTEGER NOT NULL DEFAULT 0);").map_err(|_| ProviderSettingsError::Unavailable)?;
        Ok(Self {
            config,
            database: Mutex::new(database),
            encryption_key_source: environment_encryption_key,
        })
    }

    pub fn config(&self) -> &AssistantProviderConfig {
        &self.config
    }

    /// History, fork, compact and attachments use the original surface Host.
    /// Its verified durable boundaries must match every selected provider Host.
    pub fn validate_default_home(&self, home: &Path) -> Result<(), ProviderSettingsError> {
        let canonical =
            fs::canonicalize(home).map_err(|_| ProviderSettingsError::InvalidSettings)?;
        for provider in &self.config.providers {
            if fs::canonicalize(&provider.agent_home)
                .map_err(|_| ProviderSettingsError::InvalidSettings)?
                == canonical
            {
                // Runtime ledgers expose one Web attachment per Home. Starting
                // another Host here would contend with the original surface.
                return Err(ProviderSettingsError::InvalidSettings);
            }
        }
        if let Some(provider) = self.config.providers.first()
            && authenticated_home_boundary(home)?
                != authenticated_home_boundary(&provider.agent_home)?
        {
            return Err(ProviderSettingsError::InvalidSettings);
        }
        Ok(())
    }

    /// The ingress verifier and every owned provider must share the same
    /// configured authority. This checks public verification material only.
    pub fn validate_ingress_authority(
        &self,
        issuer: &str,
        verification_key: &str,
    ) -> Result<(), ProviderSettingsError> {
        if let Some(provider) = self.config.providers.first() {
            let (_, expected_issuer, expected_key, _) =
                authenticated_home_boundary(&provider.agent_home)?;
            if issuer != expected_issuer || verification_key != expected_key {
                return Err(ProviderSettingsError::InvalidSettings);
            }
        }
        Ok(())
    }

    fn admitted(&self, owner: &str) -> Vec<&AssistantProviderDefinition> {
        let mut ids = self
            .config
            .users
            .get(owner)
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        for group in self
            .config
            .groups
            .values()
            .filter(|group| group.owners.iter().any(|value| value == owner))
        {
            ids.extend(group.providers.iter().cloned());
        }
        ids.extend(self.config.shared_provider_id.iter().cloned());
        let mut seen = BTreeSet::new();
        ids.into_iter()
            .filter(|id| seen.insert(id.clone()))
            .filter_map(|id| {
                self.config
                    .providers
                    .iter()
                    .find(|provider| provider.id == id)
            })
            .collect()
    }

    pub fn settings(&self, owner: &str) -> Result<AssistantSettings, ProviderSettingsError> {
        let providers = self.admitted(owner);
        if providers.is_empty() {
            return Err(ProviderSettingsError::PermissionDenied);
        }
        let db = self
            .database
            .lock()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        let stored: Option<String> = db
            .query_row(
                "SELECT provider_id FROM assistant_provider_selection WHERE owner=?1",
                [owner],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        // A removed policy grant immediately invalidates a persisted preference.
        let selected = stored
            .filter(|id| providers.iter().any(|provider| &provider.id == id))
            .unwrap_or_else(|| providers[0].id.clone());
        let has_byok = db
            .query_row(
                "SELECT 1 FROM assistant_provider_byok WHERE owner=?1 AND provider_id=?2",
                params![owner, selected],
                |_| Ok(()),
            )
            .optional()
            .map_err(|_| ProviderSettingsError::Unavailable)?
            .is_some();
        let byok_enabled = self.config.byok_enabled
            && providers
                .iter()
                .any(|provider| provider.id == selected && provider.byok.is_some());
        Ok(AssistantSettings {
            providers: providers
                .into_iter()
                .map(|provider| AssistantProviderOption {
                    id: provider.id.clone(),
                    label: provider.label.clone(),
                    model: provider.model.clone(),
                })
                .collect(),
            selected_provider_id: Some(selected),
            byok_enabled,
            has_byok: byok_enabled && has_byok,
        })
    }

    pub fn resolve(&self, owner: &str) -> Result<ProviderRoute, ProviderSettingsError> {
        let settings = self.settings(owner)?;
        let id = settings
            .selected_provider_id
            .ok_or(ProviderSettingsError::PermissionDenied)?;
        let definition = self
            .config
            .providers
            .iter()
            .find(|provider| provider.id == id)
            .ok_or(ProviderSettingsError::PermissionDenied)?;
        if settings.has_byok {
            let db = self
                .database
                .lock()
                .map_err(|_| ProviderSettingsError::Unavailable)?;
            let (lease_key, home): (String, String) = db.query_row("SELECT lease_key,home FROM assistant_provider_byok WHERE owner=?1 AND provider_id=?2", params![owner, id], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|_| ProviderSettingsError::Unavailable)?;
            return Ok(ProviderRoute {
                id,
                agent_home: PathBuf::from(home),
                profile: None,
                model: definition.model.clone(),
                allowed_tools: definition.allowed_tools.clone(),
                lease_key,
            });
        }
        Ok(ProviderRoute {
            id: id.clone(),
            agent_home: definition.agent_home.clone(),
            profile: definition.profile.clone(),
            model: definition.model.clone(),
            allowed_tools: definition.allowed_tools.clone(),
            lease_key: format!("platform:{id}"),
        })
    }

    /// Run outside the local Agent runtime thread: Age encryption is deliberately
    /// expensive and must not block other users' streams or cancellation.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "transaction owns the decoded credential so Drop zeroizes it before returning"
    )]
    pub fn update(
        &self,
        owner: &str,
        update: AssistantSettingsUpdate,
    ) -> Result<AssistantSettings, ProviderSettingsError> {
        let allowed = self.admitted(owner);
        if allowed.is_empty() {
            return Err(ProviderSettingsError::PermissionDenied);
        }
        if update
            .provider_id
            .as_ref()
            .and_then(Option::as_ref)
            .is_some_and(|id| !allowed.iter().any(|provider| &provider.id == id))
        {
            return Err(ProviderSettingsError::PermissionDenied);
        }
        let selected = match &update.provider_id {
            Some(Some(id)) => id.clone(),
            Some(None) => allowed[0].id.clone(),
            None => self
                .settings(owner)?
                .selected_provider_id
                .ok_or(ProviderSettingsError::PermissionDenied)?,
        };
        let prepared = match update.byok.as_ref() {
            Some(Some(byok)) => {
                if byok.provider_id != selected {
                    return Err(ProviderSettingsError::InvalidSettings);
                }
                let provider = allowed
                    .iter()
                    .find(|provider| provider.id == byok.provider_id)
                    .ok_or(ProviderSettingsError::PermissionDenied)?;
                if !self.config.byok_enabled {
                    return Err(ProviderSettingsError::PermissionDenied);
                }
                let template = provider
                    .byok
                    .as_ref()
                    .ok_or(ProviderSettingsError::PermissionDenied)?;
                if byok.api_key.is_empty()
                    || byok.api_key.len() > 16_384
                    || byok.api_key.bytes().any(|byte| byte.is_ascii_control())
                {
                    return Err(ProviderSettingsError::InvalidSettings);
                }
                let passphrase = (self.encryption_key_source)(&template.key_environment_variable)?;
                let reservation = self.reserve_revision(owner, provider)?;
                if let Err(error) = materialize_byok(
                    provider,
                    template,
                    &byok.api_key,
                    &passphrase,
                    &reservation.1,
                ) {
                    self.release_revision(&reservation);
                    return Err(error);
                }
                Some(reservation)
            }
            _ => None,
        };
        let result = self.persist_update(owner, &selected, &update, prepared.as_ref());
        if let Err(error) = result {
            if let Some(reservation) = &prepared {
                self.release_revision(reservation);
            }
            return Err(error);
        }
        self.settings(owner)
    }
    fn persist_update(
        &self,
        owner: &str,
        selected: &str,
        update: &AssistantSettingsUpdate,
        prepared: Option<&(String, PathBuf)>,
    ) -> Result<(), ProviderSettingsError> {
        let mut db = self
            .database
            .lock()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        let transaction = db
            .transaction()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        if matches!(update.provider_id, Some(Some(_))) {
            transaction.execute("INSERT INTO assistant_provider_selection(owner,provider_id) VALUES(?1,?2) ON CONFLICT(owner) DO UPDATE SET provider_id=excluded.provider_id", params![owner, selected]).map_err(|_| ProviderSettingsError::Unavailable)?;
        } else if matches!(update.provider_id, Some(None)) {
            transaction
                .execute(
                    "DELETE FROM assistant_provider_selection WHERE owner=?1",
                    [owner],
                )
                .map_err(|_| ProviderSettingsError::Unavailable)?;
        }
        if let Some((lease_key, home)) = prepared {
            transaction
                .execute(
                    "UPDATE assistant_provider_byok_revisions SET committed=1 WHERE lease_key=?1",
                    [lease_key],
                )
                .map_err(|_| ProviderSettingsError::Unavailable)?;
            transaction.execute("INSERT INTO assistant_provider_byok(owner,provider_id,lease_key,home) VALUES(?1,?2,?3,?4) ON CONFLICT(owner,provider_id) DO UPDATE SET lease_key=excluded.lease_key,home=excluded.home", params![owner, selected, lease_key, home.to_str().ok_or(ProviderSettingsError::Unavailable)?]).map_err(|_| ProviderSettingsError::Unavailable)?;
        } else if matches!(update.byok, Some(None)) {
            transaction
                .execute(
                    "DELETE FROM assistant_provider_byok WHERE owner=?1 AND provider_id=?2",
                    params![owner, selected],
                )
                .map_err(|_| ProviderSettingsError::Unavailable)?;
        }
        transaction
            .commit()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        Ok(())
    }

    fn reserve_revision(
        &self,
        owner: &str,
        provider: &AssistantProviderDefinition,
    ) -> Result<(String, PathBuf), ProviderSettingsError> {
        let mut db = self
            .database
            .lock()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        let transaction = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        let total: i64 = transaction
            .query_row(
                "SELECT count(*) FROM assistant_provider_byok_revisions",
                [],
                |row| row.get(0),
            )
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        let revision_count: i64 = transaction
            .query_row(
                "SELECT count(*) FROM assistant_provider_byok_revisions WHERE owner=?1",
                [owner],
                |row| row.get(0),
            )
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        let total = usize::try_from(total).map_err(|_| ProviderSettingsError::Unavailable)?;
        let revision_count =
            usize::try_from(revision_count).map_err(|_| ProviderSettingsError::Unavailable)?;
        if total >= self.config.max_byok_revisions_total
            || revision_count >= self.config.max_byok_revisions_per_user
        {
            return Err(ProviderSettingsError::CapacityExhausted);
        }
        let root = self
            .config
            .byok_root
            .as_ref()
            .ok_or(ProviderSettingsError::Unavailable)?;
        let revision = uuid::Uuid::new_v4().to_string();
        let lease_key = format!("byok:{}:{revision}", provider.id);
        let home = root
            .join(hex::encode(Sha256::digest(owner.as_bytes())))
            .join(&provider.id)
            .join(revision);
        transaction.execute("INSERT INTO assistant_provider_byok_revisions(lease_key,owner,home) VALUES(?1,?2,?3)", params![lease_key, owner, home.to_str().ok_or(ProviderSettingsError::Unavailable)?]).map_err(|_| ProviderSettingsError::Unavailable)?;
        transaction
            .commit()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        Ok((lease_key, home))
    }

    fn release_revision(&self, reservation: &(String, PathBuf)) {
        // Retain the reservation if cleanup fails: a failed deletion must not
        // let repeated uploads create unaccounted encrypted Homes.
        if let Err(error) = fs::remove_dir_all(&reservation.1)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return;
        }
        if let Ok(db) = self.database.lock() {
            let _ = db.execute(
                "DELETE FROM assistant_provider_byok_revisions WHERE lease_key=?1 AND committed=0",
                [&reservation.0],
            );
        }
    }
}

fn validate_byok_template(
    provider: &AssistantProviderDefinition,
    template: &ByokTemplate,
) -> Result<(), ProviderSettingsError> {
    if provider.profile.is_some()
        || !valid_provider_id(&template.secrets_instance)
        || template.credential_reference.is_empty()
        || template.key_environment_variable.is_empty()
        || !template
            .key_environment_variable
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(ProviderSettingsError::InvalidSettings);
    }
    // BYOK in this slice is the existing OpenAI-compatible Secrets
    // consumer. OAuth-only and fixture Providers cannot advertise BYOK.
    let directory = provider
        .agent_home
        .join("plugins/lenso.agent.model.openai-compatible");
    let mut configurations = Vec::new();
    for entry in fs::read_dir(directory).map_err(|_| ProviderSettingsError::InvalidSettings)? {
        let entry = entry.map_err(|_| ProviderSettingsError::InvalidSettings)?;
        let path = entry.path();
        if path.extension().is_some_and(|value| value == "toml")
            && !path.with_extension("disabled").exists()
        {
            configurations.push(path);
        }
    }
    if configurations.len() != 1 {
        return Err(ProviderSettingsError::InvalidSettings);
    }
    let text = fs::read_to_string(&configurations[0])
        .map_err(|_| ProviderSettingsError::InvalidSettings)?;
    let model: toml::Value =
        toml::from_str(&text).map_err(|_| ProviderSettingsError::InvalidSettings)?;
    if model.get("api_key_ref").and_then(toml::Value::as_str)
        != Some(template.credential_reference.as_str())
        || model.get("model").and_then(toml::Value::as_str) != Some(provider.model.as_str())
    {
        return Err(ProviderSettingsError::InvalidSettings);
    }
    Ok(())
}

fn valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_config(config: &AssistantProviderConfig) -> Result<(), ProviderSettingsError> {
    let ids = config
        .providers
        .iter()
        .map(|provider| provider.id.as_str())
        .collect::<BTreeSet<_>>();
    if !config.settings_database.is_absolute()
        || ids.len() != config.providers.len()
        || config.providers.len() > 64
        || !(1..=1024).contains(&config.max_cached_provider_hosts)
        || config.providers.len() > config.max_cached_provider_hosts
        || !(1..=1024).contains(&config.max_byok_revisions_per_user)
        || !(1..=65_536).contains(&config.max_byok_revisions_total)
    {
        return Err(ProviderSettingsError::InvalidSettings);
    }
    let mut session_authority = None;
    let mut homes = BTreeSet::new();
    for provider in &config.providers {
        if !valid_provider_id(&provider.id)
            || provider.label.is_empty()
            || provider.label.len() > 256
            || provider.model.is_empty()
            || !provider.agent_home.is_absolute()
            || provider.allowed_tools.iter().any(|tool| tool != "ask_user")
            || provider.allowed_tools.len() > 1
        {
            return Err(ProviderSettingsError::InvalidSettings);
        }
        if let Some(template) = &provider.byok {
            validate_byok_template(provider, template)?;
        }
        // Separate Homes prevent a Profile switch or reconciliation in one
        // provider Host from altering another Host's runtime ledger.
        if !homes.insert(
            fs::canonicalize(&provider.agent_home)
                .map_err(|_| ProviderSettingsError::InvalidSettings)?,
        ) {
            return Err(ProviderSettingsError::InvalidSettings);
        }
        let authority = authenticated_home_boundary(&provider.agent_home)?;
        if session_authority
            .as_ref()
            .is_some_and(|expected| expected != &authority)
        {
            return Err(ProviderSettingsError::InvalidSettings);
        }
        session_authority = Some(authority);
    }
    if config
        .shared_provider_id
        .as_ref()
        .is_some_and(|id| !ids.contains(id.as_str()))
        || config
            .users
            .values()
            .flatten()
            .chain(
                config
                    .groups
                    .values()
                    .flat_map(|group| group.providers.iter()),
            )
            .any(|id| !ids.contains(id.as_str()))
        || (config.byok_enabled
            && !config
                .byok_root
                .as_ref()
                .is_some_and(|path| path.is_absolute()))
    {
        return Err(ProviderSettingsError::InvalidSettings);
    }
    Ok(())
}

fn authenticated_home_boundary(
    home: &Path,
) -> Result<(String, String, String, String), ProviderSettingsError> {
    let session_text =
        fs::read_to_string(home.join("plugins/lenso.agent.session.sqlite/sessions.toml"))
            .map_err(|_| ProviderSettingsError::InvalidSettings)?;
    let session: toml::Value =
        toml::from_str(&session_text).map_err(|_| ProviderSettingsError::InvalidSettings)?;
    let database = session
        .get("database")
        .and_then(toml::Value::as_str)
        .filter(|path| Path::new(path).is_absolute())
        .ok_or(ProviderSettingsError::InvalidSettings)?;
    let auth = session
        .get("authentication")
        .ok_or(ProviderSettingsError::InvalidSettings)?;
    let issuer = auth
        .get("issuer")
        .and_then(toml::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ProviderSettingsError::InvalidSettings)?;
    let key = auth
        .get("verification_key")
        .and_then(toml::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ProviderSettingsError::InvalidSettings)?;
    lenso_auth_sdk::ActorAssertionVerifier::from_public_key_base64(issuer, key)
        .map_err(|_| ProviderSettingsError::InvalidSettings)?;
    let mut artifact_directory = None;
    for relative in [
        "plugins/lenso.agent.artifact.file/artifacts.toml",
        "plugins/lenso.agent.user-interaction.local/local-interaction.toml",
    ] {
        let text = fs::read_to_string(home.join(relative))
            .map_err(|_| ProviderSettingsError::InvalidSettings)?;
        let value: toml::Value =
            toml::from_str(&text).map_err(|_| ProviderSettingsError::InvalidSettings)?;
        if relative.contains("artifact.file") {
            artifact_directory = value
                .get("directory")
                .and_then(toml::Value::as_str)
                .filter(|path| Path::new(path).is_absolute())
                .map(str::to_owned);
        }
        let auth = value
            .get("authentication")
            .ok_or(ProviderSettingsError::InvalidSettings)?;
        if auth.get("issuer").and_then(toml::Value::as_str) != Some(issuer)
            || auth.get("verification_key").and_then(toml::Value::as_str) != Some(key)
        {
            return Err(ProviderSettingsError::InvalidSettings);
        }
    }
    Ok((
        database.to_owned(),
        issuer.to_owned(),
        key.to_owned(),
        artifact_directory.ok_or(ProviderSettingsError::InvalidSettings)?,
    ))
}

fn environment_encryption_key(name: &str) -> Result<Zeroizing<String>, ProviderSettingsError> {
    std::env::var(name)
        .map(Zeroizing::new)
        .map_err(|_| ProviderSettingsError::Unavailable)
}

fn materialize_byok(
    provider: &AssistantProviderDefinition,
    template: &ByokTemplate,
    api_key: &str,
    passphrase: &str,
    home: &Path,
) -> Result<(), ProviderSettingsError> {
    if passphrase.is_empty() {
        return Err(ProviderSettingsError::Unavailable);
    }
    fs::create_dir_all(home).map_err(|_| ProviderSettingsError::Unavailable)?;
    private_directory(home)?;
    let result = (|| {
        copy_plugins(&provider.agent_home.join("plugins"), &home.join("plugins"))?;
        // The single Secrets Slot must never retain another platform account's
        // source or silently fall back to a shared credential in a personal Host.
        for plugin in [
            "lenso.secrets.env",
            "lenso.secrets.command",
            "lenso.secrets.keychain",
            "lenso.secrets.encrypted-file",
        ] {
            let path = home.join("plugins").join(plugin);
            if path.exists() {
                fs::remove_dir_all(path).map_err(|_| ProviderSettingsError::Unavailable)?;
            }
        }
        // Session database configuration must explicitly retain the source path;
        // a provider Home must never silently create a separate member history.
        let session = provider
            .agent_home
            .join("plugins/lenso.agent.session.sqlite/sessions.toml");
        if !session.is_file() {
            return Err(ProviderSettingsError::InvalidSettings);
        }
        let secrets_path = home.join("credentials.age");
        let document = SecretDocument {
            version: 1,
            secrets: BTreeMap::from([("assistant", api_key)]),
        };
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&document).map_err(|_| ProviderSettingsError::Unavailable)?,
        );
        let encryptor =
            age::Encryptor::with_user_passphrase(SecretString::from(passphrase.to_owned()));
        let mut file = tempfile::NamedTempFile::new_in(home)
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        let mut writer = encryptor
            .wrap_output(file.as_file_mut())
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        writer
            .write_all(&plaintext)
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        writer
            .finish()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        file.as_file()
            .sync_all()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        file.persist(&secrets_path)
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        let secrets_root = home.join("plugins/lenso.secrets.encrypted-file");
        fs::create_dir_all(&secrets_root).map_err(|_| ProviderSettingsError::Unavailable)?;
        let mut references = BTreeMap::new();
        references.insert(template.credential_reference.clone(), "assistant");
        let document = serde_json::json!({"path": secrets_path, "key_environment_variable": template.key_environment_variable, "references": references, "max_file_bytes": 1_048_576, "max_plaintext_bytes": 65_536, "max_records": 1});
        // Plugin Root format is TOML; ciphertext stays outside non-secret config.
        let text = toml::to_string(&document).map_err(|_| ProviderSettingsError::Unavailable)?;
        fs::write(
            secrets_root.join(format!("{}.toml", template.secrets_instance)),
            text,
        )
        .map_err(|_| ProviderSettingsError::Unavailable)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(home);
    }
    result
}

fn copy_plugins(source: &Path, target: &Path) -> Result<(), ProviderSettingsError> {
    fs::create_dir_all(target).map_err(|_| ProviderSettingsError::Unavailable)?;
    for entry in fs::read_dir(source).map_err(|_| ProviderSettingsError::Unavailable)? {
        let entry = entry.map_err(|_| ProviderSettingsError::Unavailable)?;
        let kind = entry
            .file_type()
            .map_err(|_| ProviderSettingsError::Unavailable)?;
        let destination = target.join(entry.file_name());
        if kind.is_symlink() {
            return Err(ProviderSettingsError::InvalidSettings);
        }
        if kind.is_dir() {
            copy_plugins(&entry.path(), &destination)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), destination).map_err(|_| ProviderSettingsError::Unavailable)?;
        } else {
            return Err(ProviderSettingsError::InvalidSettings);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<(), ProviderSettingsError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| ProviderSettingsError::Unavailable)
}
#[cfg(not(unix))]
fn private_directory(_path: &Path) -> Result<(), ProviderSettingsError> {
    Ok(())
}

/// Boot once per immutable route. Runtime admission caches this lease by key.
/// Never calls `select_profile` on a Host already used by another user.
pub async fn boot_route(route: &ProviderRoute, plugins: fn()) -> Result<Rc<AgentApp>, String> {
    let host = AgentHost::builder()
        .plugins(plugins)
        .agent_home(route.agent_home.clone())?
        .surface(WebSurface::browser())
        .build()?;
    let profile = route
        .profile
        .as_ref()
        .map_or(Profile::Default, |name| Profile::Named(name.clone()));
    let app = Box::pin(host.run(profile)).await?;
    let catalog = app.provider_model_catalog().await?;
    if catalog
        .resolved_turn_profile
        .as_ref()
        .is_none_or(|selected| selected.model != route.model)
    {
        return Err("configured assistant provider model does not match its immutable Host".into());
    }
    Ok(Rc::new(app))
}

pub async fn boot_profiles(
    config: &AssistantProviderConfig,
    plugins: fn(),
) -> Result<BTreeMap<String, Rc<AgentApp>>, String> {
    let mut apps = BTreeMap::new();
    for provider in &config.providers {
        let route = ProviderRoute {
            id: provider.id.clone(),
            agent_home: provider.agent_home.clone(),
            profile: provider.profile.clone(),
            model: provider.model.clone(),
            allowed_tools: provider.allowed_tools.clone(),
            lease_key: format!("platform:{}", provider.id),
        };
        apps.insert(route.lease_key.clone(), boot_route(&route, plugins).await?);
    }
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn policy(root: &Path) -> AssistantProviderConfig {
        let session_database = root.join("shared-sessions.sqlite");
        let issuer =
            lenso_auth_sdk::ActorAssertionIssuer::from_signing_key("fixture.auth", [7; 32]);
        let mut providers = Vec::new();
        for id in ["alpha", "beta"] {
            let home = root.join(id);
            let model_root = home.join("plugins/lenso.agent.model.openai-compatible");
            fs::create_dir_all(&model_root).unwrap();
            fs::write(model_root.join("model.toml"), format!("base_url=\"http://127.0.0.1:1/v1\"\nmodel=\"fixture/{id}\"\napi_key_ref=\"model/api-key\"\n")).unwrap();
            let session_root = home.join("plugins/lenso.agent.session.sqlite");
            fs::create_dir_all(&session_root).unwrap();
            fs::write(session_root.join("sessions.toml"), format!("database = {}\n[authentication]\nissuer = \"fixture.auth\"\nverification_key = {}\n", serde_json::to_string(&session_database).unwrap(), serde_json::to_string(&issuer.public_key_base64()).unwrap())).unwrap();
            for (plugin, name, fields) in [
                (
                    "lenso.agent.artifact.file",
                    "artifacts.toml",
                    format!(
                        "directory={}\nmax_artifact_bytes=1048576\nmax_total_bytes=4194304\nmax_items=16\n",
                        serde_json::to_string(&root.join("artifacts")).unwrap()
                    ),
                ),
                (
                    "lenso.agent.user-interaction.local",
                    "local-interaction.toml",
                    "max_pending=16\ntimeout_ms=10000\n".into(),
                ),
            ] {
                let directory = home.join("plugins").join(plugin);
                fs::create_dir_all(&directory).unwrap();
                fs::write(
                    directory.join(name),
                    format!(
                        "{fields}[authentication]\nissuer=\"fixture.auth\"\nverification_key={}\n",
                        serde_json::to_string(&issuer.public_key_base64()).unwrap()
                    ),
                )
                .unwrap();
            }
            providers.push(AssistantProviderDefinition {
                id: id.into(),
                label: id.into(),
                agent_home: home,
                profile: None,
                model: format!("fixture/{id}"),
                allowed_tools: Vec::new(),
                byok: Some(ByokTemplate {
                    secrets_instance: "personal".into(),
                    credential_reference: "model/api-key".into(),
                    key_environment_variable: "LENSO_ASSISTANT_SYNTHETIC_TEST_PASSPHRASE".into(),
                }),
            });
        }
        AssistantProviderConfig {
            providers,
            shared_provider_id: None,
            users: BTreeMap::from([
                ("[\"fixture.auth\",\"alice\"]".into(), vec!["alpha".into()]),
                ("[\"fixture.auth\",\"bob\"]".into(), vec!["beta".into()]),
            ]),
            groups: BTreeMap::new(),
            byok_enabled: true,
            settings_database: root.join("settings.sqlite"),
            byok_root: Some(root.join("byok")),
            max_byok_revisions_per_user: 2,
            max_byok_revisions_total: 3,
            max_cached_provider_hosts: 32,
        }
    }

    const ALICE: &str = "[\"fixture.auth\",\"alice\"]";
    const BOB: &str = "[\"fixture.auth\",\"bob\"]";

    #[test]
    fn provider_assignments_are_owner_scoped_and_persist_without_secret_fields() {
        let root = tempfile::tempdir().unwrap();
        let mut config = policy(root.path());
        config.groups.insert(
            "reviewers".into(),
            AssistantProviderGroup {
                owners: vec![ALICE.into()],
                providers: vec!["beta".into()],
            },
        );
        let service = AssistantProviderService::open(config.clone()).unwrap();
        assert_eq!(service.resolve(ALICE).unwrap().id, "alpha");
        assert_eq!(service.resolve(BOB).unwrap().id, "beta");
        assert_eq!(
            service
                .settings("[\"other.issuer\",\"alice\"]")
                .unwrap_err(),
            ProviderSettingsError::PermissionDenied
        );
        assert_eq!(
            service
                .update(
                    BOB,
                    serde_json::from_value(serde_json::json!({"provider_id":"alpha"})).unwrap()
                )
                .unwrap_err(),
            ProviderSettingsError::PermissionDenied
        );
        service
            .update(
                ALICE,
                serde_json::from_value(serde_json::json!({"provider_id":"beta"})).unwrap(),
            )
            .unwrap();
        service
            .update(
                ALICE,
                serde_json::from_value(serde_json::json!({})).unwrap(),
            )
            .unwrap();
        assert_eq!(service.resolve(ALICE).unwrap().id, "beta");
        service
            .update(
                ALICE,
                serde_json::from_value(serde_json::json!({"provider_id":null})).unwrap(),
            )
            .unwrap();
        assert_eq!(service.resolve(ALICE).unwrap().id, "alpha");
        service
            .update(
                ALICE,
                serde_json::from_value(serde_json::json!({"provider_id":"beta"})).unwrap(),
            )
            .unwrap();
        drop(service);
        let service = AssistantProviderService::open(config.clone()).unwrap();
        assert_eq!(service.resolve(ALICE).unwrap().id, "beta");
        assert_eq!(service.resolve(BOB).unwrap().id, "beta");
        drop(service);
        config.groups.clear();
        let service = AssistantProviderService::open(config).unwrap();
        assert_eq!(service.resolve(ALICE).unwrap().id, "alpha");
        let settings = serde_json::to_string(&service.settings(ALICE).unwrap()).unwrap();
        assert!(!settings.contains("api_key"));
        assert!(!settings.contains("agent_home"));
        assert!(!settings.contains("passphrase"));
    }

    #[test]
    fn empty_policy_and_mismatched_session_authority_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let config = AssistantProviderConfig {
            settings_database: root.path().join("empty.sqlite"),
            ..AssistantProviderConfig::default()
        };
        let service = AssistantProviderService::open(config).unwrap();
        assert_eq!(
            service.settings(ALICE).unwrap_err(),
            ProviderSettingsError::PermissionDenied
        );
        let config = policy(root.path());
        fs::write(config.providers[1].agent_home.join("plugins/lenso.agent.session.sqlite/sessions.toml"), "database = '/tmp/other.sqlite'\n[authentication]\nissuer='fixture.auth'\nverification_key='other'\n").unwrap();
        assert!(matches!(
            AssistantProviderService::open(config),
            Err(ProviderSettingsError::InvalidSettings)
        ));
    }

    #[test]
    fn default_host_and_member_tools_require_reviewed_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let mut config = policy(root.path());
        config.providers[0].allowed_tools = vec!["ask_user".into()];
        let service = AssistantProviderService::open(config.clone()).unwrap();
        assert_eq!(service.resolve(ALICE).unwrap().allowed_tools, ["ask_user"]);
        let base = root.path().join("base");
        copy_plugins(
            &config.providers[0].agent_home.join("plugins"),
            &base.join("plugins"),
        )
        .unwrap();
        service.validate_default_home(&base).unwrap();
        let (_, issuer, key, _) = authenticated_home_boundary(&base).unwrap();
        service.validate_ingress_authority(&issuer, &key).unwrap();
        assert_eq!(
            service
                .validate_ingress_authority("other.issuer", &key)
                .unwrap_err(),
            ProviderSettingsError::InvalidSettings
        );
        assert_eq!(
            service
                .validate_ingress_authority(&issuer, "other-key")
                .unwrap_err(),
            ProviderSettingsError::InvalidSettings
        );
        assert_eq!(
            service
                .validate_default_home(&config.providers[0].agent_home)
                .unwrap_err(),
            ProviderSettingsError::InvalidSettings
        );
        config.providers[0].allowed_tools.push("run_process".into());
        assert!(matches!(
            AssistantProviderService::open(config),
            Err(ProviderSettingsError::InvalidSettings)
        ));
    }

    #[test]
    fn byok_reservations_bound_global_capacity_and_failed_uploads_release_quota() {
        let root = tempfile::tempdir().unwrap();
        let config = policy(root.path());
        let mut service = AssistantProviderService::open(config).unwrap();
        let alpha = &service.config.providers[0];
        let beta = &service.config.providers[1];
        let first = service.reserve_revision(ALICE, alpha).unwrap();
        let second = service.reserve_revision(ALICE, alpha).unwrap();
        let third = service.reserve_revision(BOB, beta).unwrap();
        assert_eq!(
            service.reserve_revision(BOB, beta).unwrap_err(),
            ProviderSettingsError::CapacityExhausted
        );
        service.release_revision(&third);
        let replacement = service.reserve_revision(BOB, beta).unwrap();
        service.release_revision(&first);
        service.release_revision(&second);
        service.release_revision(&replacement);
        service.encryption_key_source = |_| Ok(Zeroizing::new(String::new()));
        let update = serde_json::from_value(serde_json::json!({"provider_id":"alpha","byok":{"provider_id":"alpha","api_key":"synthetic-key"}})).unwrap();
        assert_eq!(
            service.update(ALICE, update).unwrap_err(),
            ProviderSettingsError::Unavailable
        );
        let failed_cleanup = service
            .reserve_revision(ALICE, &service.config.providers[0])
            .unwrap();
        fs::create_dir_all(failed_cleanup.1.parent().unwrap()).unwrap();
        fs::write(&failed_cleanup.1, b"synthetic-non-directory").unwrap();
        service.release_revision(&failed_cleanup);
        {
            let db = service.database.lock().unwrap();
            let retained: i64 = db
                .query_row(
                    "SELECT count(*) FROM assistant_provider_byok_revisions",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(retained, 1);
        }
        fs::remove_file(&failed_cleanup.1).unwrap();
        service.release_revision(&failed_cleanup);
        let db = service.database.lock().unwrap();
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM assistant_provider_byok_revisions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn settings_wire_rejects_owner_endpoint_and_redacts_byok() {
        for value in [
            serde_json::json!({"provider_id":"alpha","owner":BOB}),
            serde_json::json!({"byok":{"provider_id":"alpha","api_key":"synthetic","base_url":"https://untrusted.example"}}),
        ] {
            assert!(serde_json::from_value::<AssistantSettingsUpdate>(value).is_err());
        }
        let update: AssistantSettingsUpdate = serde_json::from_value(serde_json::json!({"provider_id":"alpha","byok":{"provider_id":"alpha","api_key":"synthetic-secret-never-print"}})).unwrap();
        assert!(!format!("{update:?}").contains("synthetic-secret"));
        let delete: AssistantSettingsUpdate =
            serde_json::from_value(serde_json::json!({"provider_id":"alpha","byok":null})).unwrap();
        assert!(matches!(delete.byok, Some(None)));
        let keep: AssistantSettingsUpdate =
            serde_json::from_value(serde_json::json!({"provider_id":"alpha"})).unwrap();
        assert!(keep.byok.is_none());
    }

    #[test]
    // Keep the stateful write/decrypt/rotate/delete/quota scenario together so
    // each assertion checks the same immutable leases and durable store.
    #[allow(clippy::too_many_lines)]
    fn byok_encrypts_compatible_secrets_and_revisions_are_immutable() {
        let passphrase = "synthetic-age-passphrase-001";
        let root = tempfile::tempdir().unwrap();
        let mut config = policy(root.path());
        config.users.get_mut(ALICE).unwrap().push("beta".into());
        config.max_byok_revisions_per_user = 3;
        config.max_byok_revisions_total = 4;
        let mut service = AssistantProviderService::open(config).unwrap();
        // Inject only the private key source; exercise the production mutation,
        // encryption, SQLite transaction and immutable-version selection path.
        service.encryption_key_source =
            |_| Ok(Zeroizing::new("synthetic-age-passphrase-001".into()));
        let synthetic = "synthetic-key-alpha-001";
        let update = |key| {
            serde_json::from_value(serde_json::json!({"provider_id":"alpha","byok":{"provider_id":"alpha","api_key":key}})).unwrap()
        };
        assert_eq!(
            service.update(BOB, update(synthetic)).unwrap_err(),
            ProviderSettingsError::PermissionDenied
        );
        let settings = service.update(ALICE, update(synthetic)).unwrap();
        assert!(settings.has_byok);
        let first = service.resolve(ALICE).unwrap();
        let ciphertext = fs::read(first.agent_home.join("credentials.age")).unwrap();
        assert!(!String::from_utf8_lossy(&ciphertext).contains(synthetic));
        let decryptor = age::Decryptor::new(ciphertext.as_slice()).unwrap();
        let identity = age::scrypt::Identity::new(SecretString::from(passphrase.to_owned()));
        let mut plaintext = String::new();
        decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .unwrap()
            .read_to_string(&mut plaintext)
            .unwrap();
        let document: serde_json::Value = serde_json::from_str(&plaintext).unwrap();
        assert_eq!(document["version"], 1);
        assert_eq!(document["secrets"]["assistant"], synthetic);
        let config_text = fs::read_to_string(
            first
                .agent_home
                .join("plugins/lenso.secrets.encrypted-file/personal.toml"),
        )
        .unwrap();
        let secrets_config: toml::Value = toml::from_str(&config_text).unwrap();
        assert_eq!(
            secrets_config["references"]["model/api-key"].as_str(),
            Some("assistant")
        );
        assert!(!config_text.contains(synthetic));
        assert!(
            !fs::read_to_string(
                first
                    .agent_home
                    .join("plugins/lenso.agent.session.sqlite/sessions.toml")
            )
            .unwrap()
            .contains(synthetic)
        );
        service
            .update(ALICE, update("synthetic-key-alpha-002"))
            .unwrap();
        let second = service.resolve(ALICE).unwrap();
        assert_ne!(first.lease_key, second.lease_key);
        assert_eq!(
            fs::read(first.agent_home.join("credentials.age")).unwrap(),
            ciphertext
        );
        assert_eq!(service.resolve(BOB).unwrap().lease_key, "platform:beta");
        service.update(ALICE, serde_json::from_value(serde_json::json!({"provider_id":"beta","byok":{"provider_id":"beta","api_key":"synthetic-key-beta-003"}})).unwrap()).unwrap();
        let beta = service.resolve(ALICE).unwrap();
        assert!(beta.lease_key.starts_with("byok:beta:"));
        service
            .update(
                ALICE,
                serde_json::from_value(serde_json::json!({"byok":null})).unwrap(),
            )
            .unwrap();
        assert_eq!(service.resolve(ALICE).unwrap().lease_key, "platform:beta");
        assert_eq!(
            service
                .settings(ALICE)
                .unwrap()
                .selected_provider_id
                .as_deref(),
            Some("beta")
        );
        service
            .update(
                ALICE,
                serde_json::from_value(serde_json::json!({"provider_id":"alpha"})).unwrap(),
            )
            .unwrap();
        assert_eq!(service.resolve(ALICE).unwrap().lease_key, second.lease_key);
        assert!(beta.agent_home.is_dir());
        // Draining turns retain their old encrypted versions until Host shutdown.
        assert!(first.agent_home.is_dir());
        assert!(second.agent_home.is_dir());
        assert_eq!(
            service
                .update(ALICE, update("synthetic-key-alpha-004"))
                .unwrap_err(),
            ProviderSettingsError::CapacityExhausted
        );
        let db = service.database.lock().unwrap();
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM assistant_provider_byok_revisions WHERE owner=?1",
                [ALICE],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
        for path in [
            service.config.settings_database.clone(),
            service
                .config
                .settings_database
                .with_extension("sqlite-wal"),
        ] {
            if let Ok(bytes) = fs::read(path) {
                assert!(!String::from_utf8_lossy(&bytes).contains(synthetic));
            }
        }
        assert!(
            !serde_json::to_string(&settings)
                .unwrap()
                .contains(synthetic)
        );
    }
}
