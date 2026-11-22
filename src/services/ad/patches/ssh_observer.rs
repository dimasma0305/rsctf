//! Sample organizer-registered files during a hosted team shell.
use super::{git_object, ssh_sources::SshSource};
use crate::app_state::SharedState;
use crate::services::ad::forensics;
use crate::services::byoc_tunnel::source::{FileIdentity, Files};
use crate::services::container::ContainerLiveness;
use crate::utils::error::{AppError, AppResult};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
mod recovery;
#[cfg(test)]
mod tests;

pub(crate) struct Session {
    pub st: SharedState,
    pub game: i32,
    pub participation: i32,
    pub challenge: i32,
    pub service: i32,
    /// Published, runtime-routed identity from this team's service row.
    pub backend: String,
}

// The same publication and starter must still be current when the observation
// is committed. A slow reader never writes results for a replacement service.
const CURRENT: &str = r#"SELECT p.tree_id, p.ssh_sources
    FROM "AdTeamServices" s JOIN "GameChallenges" c ON c.id=s.challenge_id AND c.game_id=s.game_id
    JOIN "Games" g ON g.id=s.game_id JOIN "AdPatchStarters" p ON p.challenge_id=c.id
    WHERE s.id=$1 AND s.game_id=$2 AND s.participation_id=$3 AND s.challenge_id=$4 AND s.container_id=$5
      AND NOT c.ad_self_hosted AND NOT c.deletion_pending AND NOT g.deletion_pending
      AND g.start_time_utc<=clock_timestamp() AND clock_timestamp()<g.end_time_utc
      AND p.ssh_sources<>'[]'::jsonb"#;

impl Session {
    pub async fn watch(self, closed: CancellationToken) {
        loop {
            let check = self.capture();
            tokio::select! {
                biased;
                _ = closed.cancelled() => break,
                configured = check => if !configured { return; },
            }
            tokio::select! {
                _ = closed.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
        }
        self.capture().await;
    }

    async fn capture(&self) -> bool {
        match tokio::time::timeout(Duration::from_secs(12), self.capture_current()).await {
            Ok(Ok(configured)) => configured,
            Ok(Err(error)) => {
                tracing::debug!(service=self.service,%error,"SSH source observation deferred");
                true
            }
            Err(_) => true,
        }
    }

    async fn capture_current(&self) -> AppResult<bool> {
        let policy = {
            let mut db = self.st.pg().acquire().await.map_err(db_error)?;
            self.policy(&mut db, false).await?
        };
        let Some((tree, paths)) = policy else {
            return Ok(false);
        };
        let mut permit =
            forensics::acquire(self.st.pg(), &self.backend, forensics::ForensicsWork::File).await?;
        let files = tokio::time::timeout(forensics::FILE_DEADLINE, self.sample(&paths))
            .await
            .ok()
            .and_then(Result::ok);
        let recovery = match files.as_ref() {
            Some(files)
                if paths.iter().any(|source| source.retain_content)
                    && !recovery::already_retained(
                        &mut permit.transaction,
                        self.service,
                        &tree,
                        files,
                    )
                    .await? =>
            {
                tokio::time::timeout(
                    Duration::from_secs(3),
                    recovery::collect(self, &paths, files),
                )
                .await
                .ok()
                .flatten()
            }
            _ => None,
        };
        // Exclusively lock the team service before allocating a revision.
        let current = self.policy(&mut permit.transaction, true).await?;
        if !current.is_some_and(|(id, policy)| id == tree && policy.0 == paths.0) {
            return Ok(false);
        }
        let revision = crate::services::byoc_tunnel::source::store_ssh(
            &mut permit.transaction,
            self.service,
            files,
        )
        .await?;
        if let Some(recovery) = recovery {
            recovery
                .save(&mut permit.transaction, self.service, revision, &tree)
                .await?;
        }
        permit.transaction.commit().await.map_err(db_error)?;
        Ok(true)
    }

    async fn policy(
        &self,
        db: &mut sqlx::PgConnection,
        lock: bool,
    ) -> AppResult<Option<(String, sqlx::types::Json<Vec<SshSource>>)>> {
        let sql = if lock {
            format!("{CURRENT} FOR NO KEY UPDATE OF s FOR SHARE OF p,c,g")
        } else {
            CURRENT.to_owned()
        };
        sqlx::query_as(&sql)
            .bind(self.service)
            .bind(self.game)
            .bind(self.participation)
            .bind(self.challenge)
            .bind(&self.backend)
            .fetch_optional(db)
            .await
            .map_err(db_error)
    }

    async fn sample(&self, paths: &[SshSource]) -> AppResult<Files> {
        let mut files = BTreeMap::new();
        for source in paths {
            let file = match self
                .st
                .containers
                .read_file(
                    &self.backend,
                    &source.container_path,
                    forensics::MAX_FILE_PREVIEW_BYTES,
                )
                .await
            {
                Ok(file) if !file.truncated => Some(FileIdentity {
                    blob: hex::encode(git_object("blob", &file.bytes)),
                    executable: file.executable,
                }),
                Ok(_) => {
                    return Err(AppError::bad_request(
                        "Registered source exceeds the observation size limit",
                    ))
                }
                Err(AppError::NotFound(_)) => None,
                Err(error) => return Err(error),
            };
            files.insert(source.source_path.clone(), file);
        }
        if self.st.containers.inspect_liveness(&self.backend).await? != ContainerLiveness::Running {
            return Err(AppError::not_found("Hosted service is no longer running"));
        }
        Ok(files)
    }
}

fn db_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}
