//! SQLite-owned publication of the reviewed coding preset, with exact crash undo.
use super::{
    Connection, Context, OptionalExtension, Path, SqlitePluginConfigurationAuthority,
    TransactionBehavior, bail, fs, params,
};
use crate::plugin_control::{PluginMutationLinearization, StagedHome, atomic_write};
use lenso_agent_host::{coding_profile_files, previous_official_profile};

#[derive(serde::Serialize, serde::Deserialize)]
struct ImportedFile {
    path: String,
    before: Option<String>,
    after: String,
}

impl SqlitePluginConfigurationAuthority {
    pub(crate) fn import_coding_profiles(&self, expected: &str) -> anyhow::Result<String> {
        self.with_operation(|connection| {
            let current = self.reconcile(connection)?;
            if current.revision().as_str() != expected {
                bail!("Profile import revision conflict");
            }
            let _fence = PluginMutationLinearization::acquire(&self.root, &self.root)
                .map_err(anyhow::Error::msg)?;
            if lenso_agent_host::snapshot_desired_plugin_root_for_home(&self.root, &self.root, None).map_err(anyhow::Error::msg)?.plugin_root_revision() != expected {
                bail!("Profile import revision conflict");
            }
            let staged = StagedHome::new(&self.root).map_err(anyhow::Error::msg)?;
            let mut files = Vec::new();
            for (path, after) in coding_profile_files() {
                let before = read_file(&self.root, &path)?;
                if before.as_deref().is_some_and(|source| source != after
                    && !previous_official_profile(&path, source, &after)) {
                    bail!("refusing to overwrite customized coding Profile file: {path}");
                }
                // Do not disable an existing, explicitly enabled Instance.
                if let Some(instance) = path.strip_suffix(".disabled")
                    && before.is_none() && self.root.join(format!("{instance}.toml")).exists() {
                    continue;
                }
                atomic_write(&staged.home.join(&path), after.as_bytes()).map_err(anyhow::Error::msg)?;
                files.push(ImportedFile { path, before, after });
            }
            for profile in [None, Some("plan"), Some("code"), Some("code-sandbox")] {
                lenso_agent_host::snapshot_desired_plugin_root_for_home(&staged.home, &self.root, profile)
                    .map_err(anyhow::Error::msg)?;
            }
            let candidate = lenso_app_authoring::inspect_plugin_root(&staged.home)?.revision().to_string();
            let id = uuid::Uuid::new_v4().to_string();
            connection.execute(
                "INSERT INTO profile_imports(id, base_revision, candidate_revision, files, phase) VALUES (?1, ?2, ?3, ?4, 'materializing')",
                params![id, expected, candidate, serde_json::to_string(&files)?],
            )?;
            let result = (|| -> anyhow::Result<()> {
                for file in &files {
                    atomic_write(&self.root.join(&file.path), file.after.as_bytes()).map_err(anyhow::Error::msg)?;
                }
                if lenso_agent_host::snapshot_desired_plugin_root_for_home(&self.root, &self.root, None).map_err(anyhow::Error::msg)?.plugin_root_revision() != candidate {
                    bail!("Profile import materialized revision mismatch");
                }
                let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if transaction.execute("UPDATE authority_state SET desired_revision = ?1 WHERE singleton = 1 AND desired_revision = ?2", params![candidate, expected])? != 1 {
                    bail!("Profile import lost its revision fence");
                }
                transaction.execute("UPDATE profile_imports SET phase = 'published' WHERE id = ?1", [&id])?;
                transaction.commit()?;
                Ok(())
            })();
            if let Err(error) = result {
                undo_files(&self.root, &files)?;
                connection.execute("UPDATE profile_imports SET phase = 'aborted' WHERE id = ?1", [&id])?;
                return Err(error);
            }
            Ok(candidate)
        })
    }

    pub(crate) fn validate_managed_profile(&self, profile: Option<&str>) -> anyhow::Result<()> {
        self.with_operation(|connection| {
            self.reconcile(connection)?;
            let Some(profile) = profile else { return Ok(()); };
            if !["plan", "code", "code-sandbox"].contains(&profile) {
                bail!("SQLite Profile selection requires an imported coding Profile");
            }
            let bytes: Option<String> = connection.query_row(
                "SELECT files FROM profile_imports WHERE phase = 'published' ORDER BY rowid DESC LIMIT 1",
                [], |row| row.get(0),
            ).optional()?;
            let files: Vec<ImportedFile> = serde_json::from_str(&bytes.context("Import coding Profiles before selecting a managed Profile")?)?;
            let path = format!("profiles/{profile}.toml");
            let recorded = files.iter().find(|file| file.path == path).context("Profile is absent from the import record")?;
            if read_file(&self.root, &path)?.as_deref() != Some(&recorded.after) {
                bail!("Profile file diverges from its managed import");
            }
            Ok(())
        })
    }

    pub(super) fn recover_profile_imports(&self, connection: &Connection) -> anyhow::Result<()> {
        let pending = connection.prepare("SELECT id, base_revision, files FROM profile_imports WHERE phase = 'materializing'")?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        if pending.is_empty() {
            return Ok(());
        }
        if pending.len() != 1 {
            bail!("ambiguous Profile import intent");
        }
        let _fence = PluginMutationLinearization::acquire(&self.root, &self.root)
            .map_err(anyhow::Error::msg)?;
        let (id, base, bytes) = &pending[0];
        let desired: String = connection.query_row(
            "SELECT desired_revision FROM authority_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if &desired != base {
            bail!("Profile import recovery lost its revision fence");
        }
        let files = serde_json::from_str::<Vec<ImportedFile>>(bytes)?;
        undo_files(&self.root, &files)?;
        if lenso_agent_host::snapshot_desired_plugin_root_for_home(&self.root, &self.root, None)
            .map_err(anyhow::Error::msg)?
            .plugin_root_revision()
            != base
        {
            bail!("Profile import recovery does not explain the Plugin Root");
        }
        connection.execute(
            "UPDATE profile_imports SET phase = 'aborted' WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }
}

fn read_file(root: &Path, relative: &str) -> anyhow::Result<Option<String>> {
    // Only compiled preset paths are admitted, including during recovery.
    if !coding_profile_files()
        .iter()
        .any(|(path, _)| path == relative)
    {
        bail!("unknown Profile import path");
    }
    let path = root.join(relative);
    for ancestor in path.ancestors().take_while(|path| *path != root) {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("Profile import refuses symlinks")
            }
            Ok(metadata) if ancestor == path && metadata.len() > 1024 * 1024 => {
                bail!("Profile import file exceeds 1 MiB")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    match fs::read_to_string(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn undo_files(root: &Path, files: &[ImportedFile]) -> anyhow::Result<()> {
    for file in files {
        let current = read_file(root, &file.path)?;
        if current != file.before && current.as_deref() != Some(&file.after) {
            bail!(
                "Profile import recovery found an unexplained edit: {}",
                file.path
            );
        }
    }
    for file in files.iter().rev() {
        if let Some(before) = &file.before {
            atomic_write(&root.join(&file.path), before.as_bytes()).map_err(anyhow::Error::msg)?;
        } else if root.join(&file.path).exists() {
            fs::remove_file(root.join(&file.path))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration_store::PluginConfigurationStoreConfig;
    use lenso_app_authoring::PluginConfigurationAuthority;

    fn fixture() -> (tempfile::TempDir, SqlitePluginConfigurationAuthority) {
        let root = tempfile::tempdir().unwrap();
        crate::configure_test_fixture_model(root.path());
        lenso_agent_host::AgentHost::builder()
            .plugins(lenso_agent_default_plugins::link)
            .agent_home(root.path())
            .unwrap()
            .surface(crate::WebSurface::browser())
            .build()
            .unwrap()
            .prepare_authoring()
            .unwrap();
        let authority = SqlitePluginConfigurationAuthority::open(
            root.path(),
            PluginConfigurationStoreConfig::new(root.path().join("config.sqlite3"), "test/app"),
        )
        .unwrap();
        (root, authority)
    }

    #[test]
    fn managed_coding_import_is_idempotent_and_survives_restart() {
        let (root, authority) = fixture();
        let base = authority.inspect().unwrap().revision().to_string();
        assert!(authority.validate_managed_profile(Some("plan")).is_err());
        let imported = authority.import_coding_profiles(&base).unwrap();
        assert_ne!(base, imported);
        assert_eq!(authority.inspect().unwrap().revision().as_str(), imported);
        assert_eq!(
            authority.import_coding_profiles(&imported).unwrap(),
            imported
        );
        assert!(authority.import_coding_profiles(&base).is_err());
        let database = authority.database().to_path_buf();
        drop(authority);
        let reopened = SqlitePluginConfigurationAuthority::open(
            root.path(),
            PluginConfigurationStoreConfig::new(database, "test/app"),
        )
        .unwrap();
        reopened.validate_managed_profile(Some("plan")).unwrap();
        fs::write(root.path().join("profiles/plan.toml"), "instances = []").unwrap();
        assert!(reopened.validate_managed_profile(Some("plan")).is_err());
    }

    #[test]
    fn managed_import_preserves_custom_files() {
        let (root, authority) = fixture();
        let path = root.path().join("profiles/plan.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "instances = []").unwrap();
        let base = authority.inspect().unwrap().revision().to_string();
        assert!(
            authority
                .import_coding_profiles(&base)
                .unwrap_err()
                .to_string()
                .contains("customized")
        );
        assert!(
            !root
                .path()
                .join("plugins/lenso.agent.workspace-edit/default.toml")
                .exists()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "instances = []");
    }

    #[test]
    fn import_does_not_disable_an_existing_enabled_instance() {
        let (root, authority) = fixture();
        let files = coding_profile_files();
        let (_, source) = files
            .iter()
            .find(|(path, _)| path == "plugins/lenso.agent.workspace-instructions/default.toml")
            .unwrap();
        let base = authority.inspect().unwrap().revision().clone();
        let proposal = authority
            .propose(
                &base,
                "lenso.agent.workspace-instructions",
                "default",
                source.as_bytes(),
            )
            .unwrap();
        authority.publish(&proposal).unwrap();
        let base = authority.inspect().unwrap().revision().to_string();
        authority.import_coding_profiles(&base).unwrap();
        assert!(
            !root
                .path()
                .join("plugins/lenso.agent.workspace-instructions/default.disabled")
                .exists()
        );
    }

    #[test]
    fn interrupted_import_refuses_unexplained_edits_without_partial_undo() {
        let (root, authority) = fixture();
        let files = vec![
            ImportedFile {
                path: "profiles/code.toml".to_owned(),
                before: None,
                after: "instances = []".to_owned(),
            },
            ImportedFile {
                path: "profiles/plan.toml".to_owned(),
                before: None,
                after: "instances = []".to_owned(),
            },
        ];
        atomic_write(&root.path().join(&files[0].path), files[0].after.as_bytes()).unwrap();
        atomic_write(&root.path().join(&files[1].path), b"unexplained").unwrap();
        let base = authority.inspect().unwrap().revision().to_string();
        authority.with_operation(|connection| {
            connection.execute("INSERT INTO profile_imports VALUES ('interrupted', ?1, 'unused', ?2, 'materializing')", params![base, serde_json::to_string(&files)?])?;
            Ok(())
        }).unwrap();
        assert!(
            authority
                .inspect()
                .unwrap_err()
                .to_string()
                .contains("unexplained")
        );
        assert_eq!(
            fs::read_to_string(root.path().join(&files[0].path)).unwrap(),
            files[0].after
        );
        assert_eq!(
            fs::read_to_string(root.path().join(&files[1].path)).unwrap(),
            "unexplained"
        );
    }

    #[test]
    fn interrupted_import_restores_exact_previous_bytes_before_inspection() {
        let (root, authority) = fixture();
        let base = authority.inspect().unwrap().revision().to_string();
        let original = "description = \"original\"\ninstances = []\n";
        let files = vec![
            ImportedFile {
                path: "profiles/plan.toml".to_owned(),
                before: Some(original.to_owned()),
                after: "instances = []".to_owned(),
            },
            ImportedFile {
                path: "plugins/lenso.agent.workspace-instructions/default.toml".to_owned(),
                before: None,
                after: "working_directory = \".\"\n".to_owned(),
            },
        ];
        for file in &files {
            atomic_write(&root.path().join(&file.path), file.after.as_bytes()).unwrap();
        }
        authority.with_operation(|connection| {
            connection.execute("INSERT INTO profile_imports VALUES ('interrupted', ?1, 'unused', ?2, 'materializing')", params![base, serde_json::to_string(&files)?])?;
            Ok(())
        }).unwrap();
        let database = authority.database().to_path_buf();
        drop(authority);
        let recovered = SqlitePluginConfigurationAuthority::open(
            root.path(),
            PluginConfigurationStoreConfig::new(database, "test/app"),
        )
        .unwrap();
        assert_eq!(recovered.inspect().unwrap().revision().as_str(), base);
        assert_eq!(
            fs::read_to_string(root.path().join("profiles/plan.toml")).unwrap(),
            original
        );
        assert!(
            !root
                .path()
                .join("plugins/lenso.agent.workspace-instructions/default.toml")
                .exists()
        );
    }
}
