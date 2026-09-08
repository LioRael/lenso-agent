//! Named Profile drafts are owned by the configured SQLite authority.
use super::{
    Connection, Context, OptionalExtension, Path, SqlitePluginConfigurationAuthority, bail, fs,
    params,
};
use crate::plugin_control::{PluginMutationLinearization, StagedHome, atomic_write};
use lenso_agent_host::profile::{ProfileDocument, validate_profile_name};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditableProfile {
    pub name: String,
    pub revision: String,
    pub read_only: bool,
    pub document: ProfileDocument,
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SaveProfile {
    pub name: String,
    pub expected_revision: Option<String>,
    pub document: ProfileDocument,
}
fn revision(source: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(source.as_bytes()))
}
fn decode(name: &str, source: &str, read_only: bool) -> anyhow::Result<EditableProfile> {
    Ok(EditableProfile {
        name: name.into(),
        revision: revision(source),
        read_only,
        document: toml::from_str(source)?,
    })
}
fn source_file(root: &Path, name: &str) -> anyhow::Result<Option<String>> {
    validate_profile_name(name).map_err(anyhow::Error::msg)?;
    let path = root.join("profiles").join(format!("{name}.toml"));
    for part in [root.join("profiles"), path.clone()] {
        match fs::symlink_metadata(&part) {
            Ok(meta)
                if meta.file_type().is_symlink()
                    || (part == path && (!meta.is_file() || meta.len() > 262_144)) =>
            {
                bail!("Profile must be a regular file of at most 256 KiB")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    match fs::read_to_string(path) {
        Ok(source) => Ok(Some(source)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
impl SqlitePluginConfigurationAuthority {
    pub(crate) fn profiles(&self) -> anyhow::Result<Vec<EditableProfile>> {
        self.with_operation(|connection| {
            self.reconcile(connection)?;
            let mut profiles = std::collections::BTreeMap::new();
            profiles.insert("default".to_owned(), decode("default", "description = 'Default Agent configuration'\ninclude_enabled = true\ninstances = []\n", true)?);
            if self.root.join("profiles").exists() {
                if fs::symlink_metadata(self.root.join("profiles"))?.file_type().is_symlink() { bail!("Profile directory must not be a symlink"); }
                for entry in fs::read_dir(self.root.join("profiles"))? {
                    let path = entry?.path();
                    if path.extension().is_some_and(|value| value == "toml") {
                        let name = path.file_stem().and_then(|value| value.to_str()).context("Invalid Profile filename")?;
                        if let Some(source) = source_file(&self.root, name)? { profiles.insert(name.into(), decode(name, &source, true)?); }
                    }
                }
            }
            let mut query = connection.prepare("SELECT name, source FROM profile_drafts ORDER BY name")?;
            for record in query.query_map([], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?)))? {
                let (name, source) = record?; profiles.insert(name.clone(), decode(&name, &source, false)?);
            }
            Ok(profiles.into_values().collect())
        })
    }
    pub(crate) fn save_profile(&self, request: &SaveProfile) -> anyhow::Result<EditableProfile> {
        validate_profile_name(&request.name).map_err(anyhow::Error::msg)?;
        if ["default", "plan", "code", "code-sandbox"].contains(&request.name.as_str()) {
            bail!("Built-in Profiles are read-only. Save a copy with a different name.");
        }
        let source = toml::to_string(&request.document)?;
        if source.len() > 262_144 {
            bail!("Profile exceeds 256 KiB");
        }
        self.with_operation(|connection| {
            self.reconcile(connection)?;
            let current: Option<String> = connection.query_row("SELECT source FROM profile_drafts WHERE name = ?1", [&request.name], |row| row.get(0)).optional()?;
            if current.as_deref().map(revision) != request.expected_revision { bail!("Profile revision conflict. Reload before saving."); }
            if current.is_none() && source_file(&self.root, &request.name)?.is_some() { bail!("An existing file uses this Profile name. Save a copy with a different name."); }
            let _fence = PluginMutationLinearization::acquire(&self.root, &self.root).map_err(anyhow::Error::msg)?;
            let staged = StagedHome::new(&self.root).map_err(anyhow::Error::msg)?;
            atomic_write(&staged.home.join("profiles").join(format!("{}.toml", request.name)), source.as_bytes()).map_err(anyhow::Error::msg)?;
            lenso_agent_host::snapshot_desired_plugin_root_for_home(&staged.home, &self.root, Some(&request.name)).map_err(anyhow::Error::msg)?;
            connection.execute("INSERT INTO profile_drafts(name, source) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET source = excluded.source", params![request.name,source])?;
            decode(&request.name, &source, false)
        })
    }
    /// Materialize only on explicit activation; saving a draft never changes the live file.
    pub(crate) fn materialize_profile(
        &self,
        name: &str,
        expected_revision: Option<&str>,
    ) -> anyhow::Result<()> {
        self.with_operation(|connection| {
            self.reconcile(connection)?;
            let source: Option<String> = connection.query_row("SELECT source FROM profile_drafts WHERE name = ?1", [name], |row| row.get(0)).optional()?;
            let Some(source) = source else { return Ok(()); };
            if expected_revision.is_some_and(|expected| expected != revision(&source)) { bail!("Profile revision changed; reload before applying"); }
            let before = source_file(&self.root, name)?;
            let expected: Option<String> = connection.query_row("SELECT materialized FROM profile_drafts WHERE name = ?1", [name], |row| row.get(0))?;
            if before != expected { bail!("Profile file changed outside the editor; activation was not applied"); }
            // Commit the desired materialization before atomic rename. Startup repairs an interrupted write.
            connection.execute("UPDATE profile_drafts SET previous_materialized = materialized, materialized = ?2, materializing = 1 WHERE name = ?1", params![name,source])?;
            self.recover_profile_drafts(connection)
        })
    }
    pub(super) fn recover_profile_drafts(&self, connection: &Connection) -> anyhow::Result<()> {
        let pending = connection.prepare("SELECT name, materialized, previous_materialized FROM profile_drafts WHERE materializing = 1")?
            .query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
        for (name, source, before) in pending {
            let current = source_file(&self.root, &name)?;
            if current != before && current.as_deref() != Some(&source) {
                bail!("Profile materialization found an unexplained external edit");
            }
            atomic_write(
                &self.root.join("profiles").join(format!("{name}.toml")),
                source.as_bytes(),
            )
            .map_err(anyhow::Error::msg)?;
            connection.execute("UPDATE profile_drafts SET materializing = 0, previous_materialized = NULL WHERE name = ?1", [name])?;
        }
        Ok(())
    }
    pub(crate) fn profile_revision(&self, name: Option<&str>) -> anyhow::Result<Option<String>> {
        let Some(name) = name else {
            return Ok(None);
        };
        source_file(&self.root, name).map(|source| source.as_deref().map(revision))
    }
    pub(super) fn validate_custom_profile(
        &self,
        connection: &Connection,
        name: &str,
    ) -> anyhow::Result<bool> {
        let source: Option<Option<String>> = connection
            .query_row(
                "SELECT materialized FROM profile_drafts WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(source) = source {
            let source = source.context("Save and apply this Profile before selecting it")?;
            if source_file(&self.root, name)?.as_deref() != Some(&source) {
                bail!("Profile file diverges from its managed revision");
            }
            return Ok(true);
        }
        Ok(false)
    }
}
