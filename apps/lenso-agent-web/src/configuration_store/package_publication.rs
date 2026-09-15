//! Durable revision handoff for atomically published Marketplace packages.
use super::{Context, SqlitePluginConfigurationAuthority, TransactionBehavior, bail, params};
use crate::plugin_control::{PluginMutationLinearization, StagedHome};

impl SqlitePluginConfigurationAuthority {
    /// The callback must validate the exact candidate and return its still-held
    /// Plugin mutation fence. Acquire the SQLite lease before any root fence.
    pub(crate) fn publish_package_change<T>(
        &self,
        id: &str,
        base: &str,
        candidate: &str,
        publish: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.with_operation(|connection| {
            let current = self.reconcile(connection)?;
            if current.revision().as_str() != base {
                bail!("managed package publication revision conflict; prepare again");
            }
            connection.execute(
                "INSERT INTO package_publications(id, base_revision, candidate_revision, phase) VALUES (?1, ?2, ?3, 'materializing')",
                params![id, base, candidate],
            )?;
            // On failure leave the durable intent intact. Recovery accepts only
            // the old root or the exact reviewed candidate, never another edit.
            let published = publish()?;
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if transaction.execute(
                "UPDATE authority_state SET desired_revision = ?1 WHERE singleton = 1 AND desired_revision = ?2",
                params![candidate, base],
            )? != 1 {
                bail!("managed package publication lost its revision fence");
            }
            transaction.execute("UPDATE package_publications SET phase = 'published' WHERE id = ?1", [id])?;
            transaction.commit().context("record managed package publication; recovery required on failure")?;
            Ok(published)
        })
    }

    pub(super) fn recover_package_changes(
        &self,
        connection: &mut rusqlite::Connection,
    ) -> anyhow::Result<()> {
        let pending = connection.prepare(
            "SELECT id, base_revision, candidate_revision FROM package_publications WHERE phase = 'materializing'",
        )?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        if pending.is_empty() {
            return Ok(());
        }
        let [(id, base, candidate)] = pending.as_slice() else {
            bail!("ambiguous managed package publication intent");
        };
        let _fence = PluginMutationLinearization::acquire(&self.root, &self.root)
            .map_err(anyhow::Error::msg)?;
        // Inspect a copied root to avoid recursively taking the authoring lock.
        let staged = StagedHome::new(&self.root).map_err(anyhow::Error::msg)?;
        let materialized = lenso_app_authoring::inspect_plugin_root(&staged.home)?;
        let revision = materialized.revision().as_str();
        let phase = if revision == candidate {
            "published"
        } else if revision == base {
            "aborted"
        } else {
            bail!("managed package recovery found an unexplained Plugin Root revision");
        };
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let desired =
            super::desired_revision(&transaction)?.context("missing managed desired revision")?;
        if desired != *base {
            bail!("managed package recovery lost its revision fence");
        }
        transaction.execute(
            "UPDATE authority_state SET desired_revision = ?1 WHERE singleton = 1",
            [revision],
        )?;
        transaction.execute(
            "UPDATE package_publications SET phase = ?1 WHERE id = ?2",
            params![phase, id],
        )?;
        transaction.commit()?;
        Ok(())
    }
}
