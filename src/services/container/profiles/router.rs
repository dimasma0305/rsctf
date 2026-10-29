use async_trait::async_trait;
use rsctf_worker_protocol::ValidatedWorkloadSpec;
use std::{collections::BTreeMap, sync::Arc};

use super::RuntimeProfile;
use crate::services::container::{
    ContainerBackendKind, ContainerExecAdmission, ContainerExecError, ContainerFile, ContainerInfo,
    ContainerLiveness, ContainerManager, ContainerSpec, ContainerStatus, FileChange,
};
use crate::utils::error::{AppError, AppResult};

const PREFIX: &str = "rsctf-profile:";

pub struct ProfileRouter {
    default: Arc<dyn ContainerManager>,
    entries: BTreeMap<String, (RuntimeProfile, Arc<dyn ContainerManager>)>,
}

impl ProfileRouter {
    pub fn new(
        default: Arc<dyn ContainerManager>,
        entries: Vec<(RuntimeProfile, Arc<dyn ContainerManager>)>,
    ) -> AppResult<Self> {
        let mut result = Self {
            default,
            entries: BTreeMap::new(),
        };
        for (profile, manager) in entries {
            if result
                .entries
                .insert(profile.id.clone(), (profile, manager))
                .is_some()
            {
                return Err(AppError::bad_request("duplicate runtime profile"));
            }
        }
        Ok(result)
    }

    fn selected(&self, id: Option<&str>) -> AppResult<&dyn ContainerManager> {
        match id {
            None => Ok(self.default.as_ref()),
            Some(id) => self.entries.get(id).filter(|(profile, _)| profile.enabled).map(|(_, manager)| manager.as_ref())
                .ok_or_else(|| AppError::unavailable(format!("Runtime profile {id:?} is unavailable; ask an administrator to restore its configuration."))),
        }
    }

    fn handle(profile: &RuntimeProfile, raw: &str) -> String {
        format!("{PREFIX}{}:{}:{raw}", profile.id, profile.identity())
    }

    fn operation(profile: &RuntimeProfile, operation: &str) -> String {
        format!(
            "profile:{}:{}:{}",
            profile.id,
            profile.identity(),
            crate::utils::codec::sha256_str(operation)
        )
    }

    fn route<'a>(&'a self, handle: &'a str) -> AppResult<(&'a dyn ContainerManager, &'a str)> {
        let Some(value) = handle.strip_prefix(PREFIX) else {
            return Ok((self.default.as_ref(), handle));
        };
        let mut parts = value.splitn(3, ':');
        let id = parts.next().unwrap_or("");
        let identity = parts.next().unwrap_or("");
        let raw = parts.next().unwrap_or("");
        let Some((profile, manager)) = self.entries.get(id) else {
            return Err(AppError::unavailable("This instance's original runtime profile is unavailable; restore that profile before operating on it."));
        };
        if raw.is_empty() || raw.len() > 512 || identity != profile.identity() {
            return Err(AppError::conflict("The instance's runtime profile identity does not match; existing profiles must not be repurposed."));
        }
        Ok((manager.as_ref(), raw))
    }

    pub(super) async fn validate_persisted_handles(&self, pool: &sqlx::PgPool) -> AppResult<()> {
        use futures::TryStreamExt;
        // Retain even an unrecorded create's owner across a process crash. A
        // profile may be disabled, but its ID/isolation cannot be removed or
        // repurposed. This also keeps orphan inventory scopes discoverable.
        let mut transaction = pool
            .begin()
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            &mut transaction,
            "runtime-profile-registration",
        )
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
        for (profile, _) in self.entries.values() {
            sqlx::query(r#"INSERT INTO "RuntimeProfileBindings" (id, identity) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING"#)
                .bind(&profile.id).bind(profile.identity()).execute(&mut *transaction).await.map_err(|e| AppError::internal(e.to_string()))?;
        }
        let bindings: Vec<(String, String)> =
            sqlx::query_as(r#"SELECT id, identity FROM "RuntimeProfileBindings" ORDER BY id"#)
                .fetch_all(&mut *transaction)
                .await
                .map_err(|e| AppError::internal(e.to_string()))?;
        for (id, identity) in bindings {
            if !self
                .entries
                .get(&id)
                .is_some_and(|(profile, _)| profile.identity() == identity)
            {
                return Err(AppError::conflict(format!("Runtime profile {id:?} was removed or repurposed. Restore its original definition; use enabled:false to retire it while retaining cleanup ownership.")));
            }
        }
        transaction
            .commit()
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
        let mut rows = sqlx::query_scalar::<_, String>(r#"
            SELECT container_id FROM "Containers" WHERE container_id LIKE 'rsctf-profile:%'
            UNION SELECT container_id FROM "AdTeamServices" WHERE container_id LIKE 'rsctf-profile:%'
            UNION SELECT container_id FROM "KothTargets" WHERE container_id LIKE 'rsctf-profile:%'
            UNION SELECT backend_id FROM "PlayerContainerOperations" WHERE backend_id LIKE 'rsctf-profile:%'
            UNION SELECT backend_id FROM "ManagedContainerReapOperations" WHERE backend_id LIKE 'rsctf-profile:%'
            UNION SELECT old_container_id FROM "KothCrownCycles" WHERE old_container_id LIKE 'rsctf-profile:%'
            UNION SELECT replacement_container_id FROM "KothCrownCycles" WHERE replacement_container_id LIKE 'rsctf-profile:%'
        "#).fetch(pool);
        while let Some(id) = rows
            .try_next()
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
        {
            self.route(&id)?;
        }
        Ok(())
    }
}

#[async_trait]
impl ContainerManager for ProfileRouter {
    fn backend_kind(&self) -> ContainerBackendKind {
        self.default.backend_kind()
    }
    fn supports_worker_workloads(&self) -> bool {
        self.default.supports_worker_workloads()
    }
    fn requires_proxy(&self) -> bool {
        self.default.requires_proxy()
    }
    fn managed_callback_routing_identity(&self) -> AppResult<Option<String>> {
        self.default.managed_callback_routing_identity()
    }
    fn runtime_profiles(&self) -> Vec<RuntimeProfile> {
        self.entries.values().map(|(p, _)| p.clone()).collect()
    }
    fn profile_backend_kind(&self, profile: Option<&str>) -> AppResult<ContainerBackendKind> {
        Ok(self.selected(profile)?.backend_kind())
    }
    fn runtime_backend_kind(&self, id: &str) -> AppResult<ContainerBackendKind> {
        Ok(self.route(id)?.0.backend_kind())
    }
    fn profile_requires_proxy(&self, profile: Option<&str>) -> AppResult<bool> {
        Ok(self.selected(profile)?.requires_proxy())
    }
    fn profile_callback_routing_identity(
        &self,
        profile: Option<&str>,
    ) -> AppResult<Option<String>> {
        self.selected(profile)?.managed_callback_routing_identity()
    }
    async fn storage_quota_enforced(&self) -> Option<bool> {
        self.default.storage_quota_enforced().await
    }
    async fn profile_storage_quota_enforced(&self, profile: Option<&str>) -> Option<bool> {
        self.selected(profile).ok()?.storage_quota_enforced().await
    }
    async fn image_exists(&self, image: &str) -> bool {
        self.default.image_exists(image).await
    }
    async fn profile_image_exists(&self, profile: Option<&str>, image: &str) -> bool {
        match self.selected(profile) {
            Ok(manager) => manager.image_exists(image).await,
            Err(_) => false,
        }
    }
    async fn pull_image(&self, image: &str) -> AppResult<()> {
        self.default.pull_image(image).await
    }
    async fn profile_pull_image(&self, profile: Option<&str>, image: &str) -> AppResult<()> {
        self.selected(profile)?.pull_image(image).await
    }

    async fn create(&self, mut spec: ContainerSpec) -> AppResult<ContainerInfo> {
        let Some(id) = spec.runtime_profile.take() else {
            return self.default.create(spec).await;
        };
        self.selected(Some(&id))?;
        let (profile, manager) = self
            .entries
            .get(&id)
            .ok_or_else(|| AppError::bad_request("Selected runtime profile is not configured"))?;
        // Profile scopes keep retry adoption and orphan inventory separate.
        // Operation keys are bounded and distinct; capacity remains host-wide.
        if let Some(operation) = spec.operation_id.as_mut() {
            *operation = Self::operation(profile, operation);
        }
        let mut info = manager.create(spec).await?;
        info.id = Self::handle(profile, &info.id);
        Ok(info)
    }

    async fn create_workload(
        &self,
        spec: ValidatedWorkloadSpec,
        operation_id: Option<String>,
        flag: Option<String>,
        proxy_only: bool,
    ) -> AppResult<ContainerInfo> {
        self.default
            .create_workload(spec, operation_id, flag, proxy_only)
            .await
    }

    async fn find_operation_runtime(&self, operation: &str) -> AppResult<Option<String>> {
        let mut found = self.default.find_operation_runtime(operation).await?;
        for (profile, manager) in self.entries.values() {
            let operation = Self::operation(profile, operation);
            if let Some(id) = manager.find_operation_runtime(&operation).await? {
                if found.is_some() {
                    return Err(AppError::conflict(
                        "A lifecycle operation has more than one runtime owner",
                    ));
                }
                found = Some(Self::handle(profile, &id));
            }
        }
        Ok(found)
    }

    async fn list_managed(&self) -> Vec<String> {
        let mut result = self.default.list_managed().await;
        for (profile, manager) in self.entries.values() {
            result.extend(
                manager
                    .list_managed()
                    .await
                    .into_iter()
                    .map(|id| Self::handle(profile, &id)),
            );
        }
        result
    }
    async fn ensure_network(&self, name: &str, subnet: &str) -> AppResult<()> {
        self.default.ensure_network(name, subnet).await?;
        for (_, manager) in self.entries.values() {
            manager.ensure_network(name, subnet).await?;
        }
        Ok(())
    }
    async fn destroy(&self, id: &str) -> AppResult<()> {
        let (m, id) = self.route(id)?;
        m.destroy(id).await
    }
    async fn query(&self, id: &str) -> AppResult<ContainerStatus> {
        let (m, raw) = self.route(id)?;
        let mut s = m.query(raw).await?;
        s.id = id.to_owned();
        Ok(s)
    }
    async fn inspect_liveness(&self, id: &str) -> AppResult<ContainerLiveness> {
        let (m, id) = self.route(id)?;
        m.inspect_liveness(id).await
    }
    async fn snapshot_changes(&self, id: &str) -> AppResult<Vec<FileChange>> {
        let (m, id) = self.route(id)?;
        m.snapshot_changes(id).await
    }
    async fn read_file(&self, id: &str, path: &str, limit: usize) -> AppResult<ContainerFile> {
        let (m, id) = self.route(id)?;
        m.read_file(id, path, limit).await
    }
    async fn exec(&self, id: &str, cmd: Vec<String>) -> AppResult<String> {
        let (m, id) = self.route(id)?;
        m.exec(id, cmd).await
    }
    async fn exec_classified(
        &self,
        id: &str,
        cmd: Vec<String>,
        admission: ContainerExecAdmission,
    ) -> Result<String, ContainerExecError> {
        let (m, id) = self.route(id).map_err(ContainerExecError::Platform)?;
        m.exec_classified(id, cmd, admission).await
    }
    async fn resolve_interactive_exec_target(&self, id: &str) -> AppResult<String> {
        let (m, id) = self.route(id)?;
        m.resolve_interactive_exec_target(id).await
    }
    async fn export(&self, id: &str) -> AppResult<Vec<u8>> {
        let (m, id) = self.route(id)?;
        m.export(id).await
    }
}
