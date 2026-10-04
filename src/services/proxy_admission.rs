use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use uuid::Uuid;

const MAX_PER_USER: usize = 4;
const MAX_PER_PARTICIPATION: usize = 16;
const MAX_PER_PREVIEW: usize = 8;
const MAX_PER_SSH_SCOPE: usize = 5;
const MAX_PER_WORKLOAD: usize = 64;
const MAX_PER_SOURCE: usize = 32;
const MAX_PER_EVENT: usize = 128;
const MAX_GLOBAL: usize = 256;
const PROCESS_BYTES_PER_SECOND: u64 = 64 * 1024 * 1024;
const PROCESS_FRAMES_PER_SECOND: u64 = 8_192;
const SESSION_BYTES_PER_SECOND: u64 = 8 * 1024 * 1024;
const SESSION_FRAMES_PER_SECOND: u64 = 1_024;
const SESSION_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const DISTRIBUTED_TRAFFIC_CREDIT_BYTES: u64 = 16 * 1024 * 1024;
const DISTRIBUTED_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Clone)]
pub struct ProxyAdmission {
    inner: Arc<Inner>,
}

struct Inner {
    global: Arc<AtomicUsize>,
    users: DashMap<Uuid, Arc<AtomicUsize>>,
    scopes: DashMap<AdmissionScope, Arc<AtomicUsize>>,
    sources: DashMap<IpAddr, Arc<AtomicUsize>>,
    events: DashMap<i32, Arc<AtomicUsize>>,
    workloads: DashMap<Uuid, Arc<AtomicUsize>>,
    traffic: Arc<FixedWindow>,
    traffic_metrics: Arc<TrafficMetrics>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum AdmissionScope {
    Participation(i32),
    Exercise(i32),
    Preview(Uuid),
    Ssh(i32),
}

pub struct ProxyPermit {
    admission: ProxyAdmission,
    global: Arc<AtomicUsize>,
    user: (Uuid, Arc<AtomicUsize>),
    scope: (AdmissionScope, Arc<AtomicUsize>),
    source: (IpAddr, Arc<AtomicUsize>),
    event: Option<(i32, Arc<AtomicUsize>)>,
    workload: (Uuid, Arc<AtomicUsize>),
    _distributed: Option<DistributedProxyPermit>,
}

struct DistributedProxyPermit {
    pool: sqlx::PgPool,
    lease_id: Uuid,
}

impl Drop for DistributedProxyPermit {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let lease_id = self.lease_id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            drop(handle.spawn(async move {
                // Best-effort early release. A crashed replica or slow pool is
                // reclaimed by the durable expiry on the next admitted open;
                // never retain an unbounded cleanup waiter here.
                let _ = tokio::time::timeout(
                    DISTRIBUTED_ADMISSION_TIMEOUT,
                    sqlx::query(r#"DELETE FROM "ProxyTunnelLeases" WHERE lease_id = $1"#)
                        .bind(lease_id)
                        .execute(&pool),
                )
                .await;
            }));
        }
    }
}

#[derive(Default)]
struct FixedWindow {
    /// Each counter atomically packs a 32-bit Unix-second epoch and a 32-bit
    /// value. Keeping reset and reservation in one CAS prevents a rollover
    /// racing another frame from erasing already-admitted work.
    bytes: AtomicU64,
    frames: AtomicU64,
}

#[derive(Clone)]
pub struct ProxyTrafficPermit {
    process: Arc<FixedWindow>,
    session: Arc<FixedWindow>,
    total_bytes: Arc<AtomicU64>,
    metrics: Arc<TrafficMetrics>,
    distributed: Arc<DistributedTrafficCredit>,
}

struct DistributedTrafficCredit {
    subject: Uuid,
    scope: String,
    source: IpAddr,
    workload: Uuid,
    remaining: AtomicU64,
    refill: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct TrafficMetrics {
    accepted_bytes: AtomicU64,
    accepted_frames: AtomicU64,
    accepted_control_frames: AtomicU64,
    rejected_frames: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProxyTrafficMetrics {
    pub accepted_bytes: u64,
    pub accepted_frames: u64,
    pub accepted_control_frames: u64,
    pub rejected_frames: u64,
}

impl ProxyTrafficPermit {
    pub async fn reserve(&self, bytes: usize) -> bool {
        self.reserve_frame(bytes, false).await
    }

    pub async fn reserve_control(&self, bytes: usize) -> bool {
        self.reserve_frame(bytes, true).await
    }

    async fn reserve_frame(&self, bytes: usize, control: bool) -> bool {
        if !self.reserve_distributed(bytes).await {
            self.record_rejection();
            return false;
        }
        self.try_reserve_frame(bytes, control)
    }

    async fn reserve_distributed(&self, bytes: usize) -> bool {
        let Ok(bytes) = u64::try_from(bytes) else {
            return false;
        };
        if bytes == 0 {
            return true;
        }
        if take_credit(&self.distributed.remaining, bytes) {
            return true;
        }
        let _refill = self.distributed.refill.lock().await;
        if take_credit(&self.distributed.remaining, bytes) {
            return true;
        }
        let leased = DISTRIBUTED_TRAFFIC_CREDIT_BYTES.max(bytes);
        let Ok(leased_usize) = usize::try_from(leased) else {
            return false;
        };
        if crate::middlewares::rate_limiter::admit_proxy_traffic_credit(
            self.distributed.subject,
            &self.distributed.scope,
            self.distributed.source,
            self.distributed.workload,
            leased_usize,
        )
        .await
        .is_err()
        {
            return false;
        }
        self.distributed
            .remaining
            .fetch_add(leased, Ordering::AcqRel);
        take_credit(&self.distributed.remaining, bytes)
    }

    fn try_reserve_frame(&self, bytes: usize, control: bool) -> bool {
        let Ok(bytes) = u64::try_from(bytes) else {
            self.record_rejection();
            return false;
        };
        if self
            .total_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= SESSION_TOTAL_BYTES)
            })
            .is_err()
        {
            self.record_rejection();
            return false;
        }
        let now = unix_second();
        if !self.session.try_reserve(
            now,
            bytes,
            SESSION_BYTES_PER_SECOND,
            SESSION_FRAMES_PER_SECOND,
        ) {
            self.total_bytes.fetch_sub(bytes, Ordering::AcqRel);
            self.record_rejection();
            return false;
        }
        if !self.process.try_reserve(
            now,
            bytes,
            PROCESS_BYTES_PER_SECOND,
            PROCESS_FRAMES_PER_SECOND,
        ) {
            self.session.release(now, bytes);
            self.total_bytes.fetch_sub(bytes, Ordering::AcqRel);
            self.record_rejection();
            return false;
        }
        self.metrics
            .accepted_bytes
            .fetch_add(bytes, Ordering::Relaxed);
        self.metrics.accepted_frames.fetch_add(1, Ordering::Relaxed);
        if control {
            self.metrics
                .accepted_control_frames
                .fetch_add(1, Ordering::Relaxed);
        }
        true
    }

    fn record_rejection(&self) {
        let rejected = self
            .metrics
            .rejected_frames
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if rejected.is_power_of_two() {
            tracing::warn!(rejected, "proxy traffic work budget rejected frames");
        }
    }
}

fn take_credit(credit: &AtomicU64, bytes: u64) -> bool {
    credit
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
            remaining.checked_sub(bytes)
        })
        .is_ok()
}

impl FixedWindow {
    fn try_reserve(&self, now: u64, bytes: u64, byte_limit: u64, frame_limit: u64) -> bool {
        let epoch = now as u32;
        if !reserve_window_counter(&self.frames, epoch, 1, frame_limit) {
            return false;
        }
        if !reserve_window_counter(&self.bytes, epoch, bytes, byte_limit) {
            release_window_counter(&self.frames, epoch, 1);
            return false;
        }
        true
    }

    fn release(&self, epoch_second: u64, bytes: u64) {
        let epoch = epoch_second as u32;
        release_window_counter(&self.bytes, epoch, bytes);
        release_window_counter(&self.frames, epoch, 1);
    }
}

fn reserve_window_counter(value: &AtomicU64, epoch: u32, amount: u64, limit: u64) -> bool {
    let Ok(amount) = u32::try_from(amount) else {
        return false;
    };
    let Ok(limit) = u32::try_from(limit) else {
        return false;
    };
    value
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |packed| {
            let observed_epoch = (packed >> 32) as u32;
            let current = if observed_epoch == epoch {
                packed as u32
            } else {
                0
            };
            current
                .checked_add(amount)
                .filter(|next| *next <= limit)
                .map(|next| (u64::from(epoch) << 32) | u64::from(next))
        })
        .is_ok()
}

fn release_window_counter(value: &AtomicU64, epoch: u32, amount: u64) {
    let Ok(amount) = u32::try_from(amount) else {
        return;
    };
    let _ = value.fetch_update(Ordering::AcqRel, Ordering::Acquire, |packed| {
        ((packed >> 32) as u32 == epoch).then(|| {
            let next = (packed as u32).saturating_sub(amount);
            (u64::from(epoch) << 32) | u64::from(next)
        })
    });
}

fn unix_second() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

impl ProxyAdmission {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                global: Arc::new(AtomicUsize::new(0)),
                users: DashMap::new(),
                scopes: DashMap::new(),
                sources: DashMap::new(),
                events: DashMap::new(),
                workloads: DashMap::new(),
                traffic: Arc::new(FixedWindow::default()),
                traffic_metrics: Arc::new(TrafficMetrics::default()),
            }),
        }
    }

    pub fn try_acquire(
        &self,
        user_id: Uuid,
        participation_id: i32,
        game_id: i32,
        workload_id: Uuid,
        source_ip: IpAddr,
    ) -> Option<ProxyPermit> {
        self.try_acquire_scope(
            user_id,
            AdmissionScope::Participation(participation_id),
            Some(game_id),
            workload_id,
            source_ip,
            (MAX_PER_PARTICIPATION, MAX_PER_USER),
        )
    }

    pub fn try_acquire_exercise(
        &self,
        user_id: Uuid,
        exercise_instance_id: i32,
        workload_id: Uuid,
        source_ip: IpAddr,
    ) -> Option<ProxyPermit> {
        self.try_acquire_scope(
            user_id,
            AdmissionScope::Exercise(exercise_instance_id),
            None,
            workload_id,
            source_ip,
            (MAX_PER_PARTICIPATION, MAX_PER_USER),
        )
    }

    pub fn try_acquire_preview(
        &self,
        user_id: Uuid,
        container_id: Uuid,
        source_ip: IpAddr,
    ) -> Option<ProxyPermit> {
        self.try_acquire_scope(
            user_id,
            AdmissionScope::Preview(container_id),
            None,
            container_id,
            source_ip,
            (MAX_PER_PREVIEW, MAX_PER_USER),
        )
    }

    pub async fn try_acquire_ssh_distributed(
        &self,
        pool: &sqlx::PgPool,
        subject_id: Uuid,
        participation_id: i32,
        game_id: i32,
        workload_id: Uuid,
        source_ip: IpAddr,
    ) -> Option<ProxyPermit> {
        let permit = self.try_acquire_scope(
            subject_id,
            AdmissionScope::Ssh(participation_id),
            Some(game_id),
            workload_id,
            source_ip,
            (MAX_PER_SSH_SCOPE, MAX_PER_SSH_SCOPE),
        )?;
        attach_distributed(
            pool,
            permit,
            subject_id,
            3,
            participation_id.to_string(),
            Some(game_id),
            workload_id,
            source_ip,
            MAX_PER_SSH_SCOPE,
            MAX_PER_SSH_SCOPE,
        )
        .await
    }

    pub async fn try_acquire_distributed(
        &self,
        pool: &sqlx::PgPool,
        user_id: Uuid,
        participation_id: i32,
        game_id: i32,
        workload_id: Uuid,
        source_ip: IpAddr,
    ) -> Option<ProxyPermit> {
        let permit =
            self.try_acquire(user_id, participation_id, game_id, workload_id, source_ip)?;
        attach_distributed(
            pool,
            permit,
            user_id,
            0,
            participation_id.to_string(),
            Some(game_id),
            workload_id,
            source_ip,
            MAX_PER_PARTICIPATION,
            MAX_PER_USER,
        )
        .await
    }

    pub async fn try_acquire_exercise_distributed(
        &self,
        pool: &sqlx::PgPool,
        user_id: Uuid,
        exercise_instance_id: i32,
        workload_id: Uuid,
        source_ip: IpAddr,
    ) -> Option<ProxyPermit> {
        let permit =
            self.try_acquire_exercise(user_id, exercise_instance_id, workload_id, source_ip)?;
        attach_distributed(
            pool,
            permit,
            user_id,
            1,
            exercise_instance_id.to_string(),
            None,
            workload_id,
            source_ip,
            MAX_PER_PARTICIPATION,
            MAX_PER_USER,
        )
        .await
    }

    pub async fn try_acquire_preview_distributed(
        &self,
        pool: &sqlx::PgPool,
        user_id: Uuid,
        container_id: Uuid,
        source_ip: IpAddr,
    ) -> Option<ProxyPermit> {
        let permit = self.try_acquire_preview(user_id, container_id, source_ip)?;
        attach_distributed(
            pool,
            permit,
            user_id,
            2,
            container_id.to_string(),
            None,
            container_id,
            source_ip,
            MAX_PER_PREVIEW,
            MAX_PER_USER,
        )
        .await
    }

    fn try_acquire_scope(
        &self,
        user_id: Uuid,
        scope: AdmissionScope,
        event_id: Option<i32>,
        workload_id: Uuid,
        source_ip: IpAddr,
        limits: (usize, usize),
    ) -> Option<ProxyPermit> {
        let (scope_limit, user_limit) = limits;
        let global = increment_counter(&self.inner.global, MAX_GLOBAL)?;
        let user = increment(&self.inner.users, user_id, user_limit)?;
        let source = match increment(&self.inner.sources, source_ip, MAX_PER_SOURCE) {
            Some(counter) => counter,
            None => {
                release(&self.inner.users, user_id, &user);
                release_counter(&global);
                return None;
            }
        };
        let scope_counter = match increment(&self.inner.scopes, scope, scope_limit) {
            Some(counter) => counter,
            None => {
                release(&self.inner.sources, source_ip, &source);
                release(&self.inner.users, user_id, &user);
                release_counter(&global);
                return None;
            }
        };
        let event = match event_id {
            Some(event_id) => match increment(&self.inner.events, event_id, MAX_PER_EVENT) {
                Some(counter) => Some((event_id, counter)),
                None => {
                    release(&self.inner.scopes, scope, &scope_counter);
                    release(&self.inner.sources, source_ip, &source);
                    release(&self.inner.users, user_id, &user);
                    release_counter(&global);
                    return None;
                }
            },
            None => None,
        };
        let workload = match increment(&self.inner.workloads, workload_id, MAX_PER_WORKLOAD) {
            Some(counter) => counter,
            None => {
                if let Some((event_id, counter)) = &event {
                    release(&self.inner.events, *event_id, counter);
                }
                release(&self.inner.scopes, scope, &scope_counter);
                release(&self.inner.sources, source_ip, &source);
                release(&self.inner.users, user_id, &user);
                release_counter(&global);
                return None;
            }
        };
        Some(ProxyPermit {
            admission: self.clone(),
            global,
            user: (user_id, user),
            scope: (scope, scope_counter),
            source: (source_ip, source),
            event,
            workload: (workload_id, workload),
            _distributed: None,
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn attach_distributed(
    pool: &sqlx::PgPool,
    mut permit: ProxyPermit,
    user_id: Uuid,
    scope_kind: i16,
    scope_id: String,
    event_id: Option<i32>,
    workload_id: Uuid,
    source_ip: IpAddr,
    scope_limit: usize,
    user_limit: usize,
) -> Option<ProxyPermit> {
    tokio::time::timeout(DISTRIBUTED_ADMISSION_TIMEOUT, async {
        let lease_id = Uuid::new_v4();
        let source_ip = source_ip.to_string();
        let mut transaction = pool.begin().await.ok()?;
        let locked = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock($1)")
            .bind(-1_489_361_103_i64)
            .fetch_one(&mut *transaction)
            .await
            .ok()?;
        if !locked {
            return None;
        }
        sqlx::query(r#"DELETE FROM "ProxyTunnelLeases" WHERE expires_at_utc <= clock_timestamp()"#)
            .execute(&mut *transaction)
            .await
            .ok()?;
        sqlx::query(
            r#"DELETE FROM "ProxyOpenBudgets"
                WHERE bucket_start_utc < clock_timestamp() - INTERVAL '2 minutes'"#,
        )
        .execute(&mut *transaction)
        .await
        .ok()?;
        let global_open = sqlx::query_scalar::<_, i32>(
            r#"INSERT INTO "ProxyOpenBudgets" (bucket_start_utc, source_key, open_count)
               VALUES (date_trunc('second', clock_timestamp()), '*', 1)
               ON CONFLICT (bucket_start_utc, source_key) DO UPDATE
                 SET open_count = "ProxyOpenBudgets".open_count + 1
               WHERE "ProxyOpenBudgets".open_count < 128
            RETURNING open_count"#,
        )
        .fetch_optional(&mut *transaction)
        .await
        .ok()?
        .is_some();
        let source_open = sqlx::query_scalar::<_, i32>(
            r#"INSERT INTO "ProxyOpenBudgets" (bucket_start_utc, source_key, open_count)
               VALUES (date_trunc('second', clock_timestamp()), $1, 1)
               ON CONFLICT (bucket_start_utc, source_key) DO UPDATE
                 SET open_count = "ProxyOpenBudgets".open_count + 1
               WHERE "ProxyOpenBudgets".open_count < 32
            RETURNING open_count"#,
        )
        .bind(&source_ip)
        .fetch_optional(&mut *transaction)
        .await
        .ok()?
        .is_some();
        if !(global_open && source_open) {
            return None;
        }
        let counts: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            r#"SELECT COUNT(*)::bigint,
                  COUNT(*) FILTER (WHERE user_id = $1)::bigint,
                  COUNT(*) FILTER (WHERE scope_kind = $2 AND scope_id = $3)::bigint,
                  COUNT(*) FILTER (WHERE source_ip = $4)::bigint,
                  COUNT(*) FILTER (WHERE event_id = $5)::bigint,
                  COUNT(*) FILTER (WHERE workload_id = $6)::bigint
                 FROM "ProxyTunnelLeases""#,
        )
        .bind(user_id)
        .bind(scope_kind)
        .bind(&scope_id)
        .bind(&source_ip)
        .bind(event_id)
        .bind(workload_id)
        .fetch_one(&mut *transaction)
        .await
        .ok()?;
        if counts.0 >= MAX_GLOBAL as i64
            || counts.1 >= user_limit as i64
            || counts.2 >= scope_limit as i64
            || counts.3 >= MAX_PER_SOURCE as i64
            || (event_id.is_some() && counts.4 >= MAX_PER_EVENT as i64)
            || counts.5 >= MAX_PER_WORKLOAD as i64
        {
            return None;
        }
        sqlx::query(
            r#"INSERT INTO "ProxyTunnelLeases"
               (lease_id, user_id, scope_kind, scope_id, source_ip,
                event_id, workload_id, expires_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6, $7,
                   clock_timestamp() + INTERVAL '31 minutes')"#,
        )
        .bind(lease_id)
        .bind(user_id)
        .bind(scope_kind)
        .bind(scope_id)
        .bind(source_ip)
        .bind(event_id)
        .bind(workload_id)
        .execute(&mut *transaction)
        .await
        .ok()?;
        transaction.commit().await.ok()?;
        permit._distributed = Some(DistributedProxyPermit {
            pool: pool.clone(),
            lease_id,
        });
        Some(permit)
    })
    .await
    .ok()
    .flatten()
}

impl Default for ProxyAdmission {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ProxyPermit {
    fn drop(&mut self) {
        release(
            &self.admission.inner.workloads,
            self.workload.0,
            &self.workload.1,
        );
        if let Some((event_id, counter)) = &self.event {
            release(&self.admission.inner.events, *event_id, counter);
        }
        release(&self.admission.inner.scopes, self.scope.0, &self.scope.1);
        release(&self.admission.inner.sources, self.source.0, &self.source.1);
        release(&self.admission.inner.users, self.user.0, &self.user.1);
        release_counter(&self.global);
    }
}

impl ProxyPermit {
    pub fn traffic(&self) -> ProxyTrafficPermit {
        ProxyTrafficPermit {
            process: Arc::clone(&self.admission.inner.traffic),
            session: Arc::new(FixedWindow::default()),
            total_bytes: Arc::new(AtomicU64::new(0)),
            metrics: Arc::clone(&self.admission.inner.traffic_metrics),
            distributed: Arc::new(DistributedTrafficCredit {
                subject: self.user.0,
                scope: match self.scope.0 {
                    AdmissionScope::Participation(id) => format!("participation:{id}"),
                    AdmissionScope::Exercise(id) => format!("exercise:{id}"),
                    AdmissionScope::Preview(id) => format!("preview:{id}"),
                    AdmissionScope::Ssh(id) => format!("ssh:{id}"),
                },
                source: self.source.0,
                workload: self.workload.0,
                remaining: AtomicU64::new(0),
                refill: tokio::sync::Mutex::new(()),
            }),
        }
    }
}

impl ProxyAdmission {
    pub fn traffic_metrics(&self) -> ProxyTrafficMetrics {
        ProxyTrafficMetrics {
            accepted_bytes: self
                .inner
                .traffic_metrics
                .accepted_bytes
                .load(Ordering::Relaxed),
            accepted_frames: self
                .inner
                .traffic_metrics
                .accepted_frames
                .load(Ordering::Relaxed),
            accepted_control_frames: self
                .inner
                .traffic_metrics
                .accepted_control_frames
                .load(Ordering::Relaxed),
            rejected_frames: self
                .inner
                .traffic_metrics
                .rejected_frames
                .load(Ordering::Relaxed),
        }
    }
}

fn increment_counter(counter: &Arc<AtomicUsize>, limit: usize) -> Option<Arc<AtomicUsize>> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            (value < limit).then_some(value + 1)
        })
        .ok()
        .map(|_| Arc::clone(counter))
}

fn release_counter(counter: &Arc<AtomicUsize>) {
    counter.fetch_sub(1, Ordering::AcqRel);
}

fn increment<K>(
    map: &DashMap<K, Arc<AtomicUsize>>,
    key: K,
    limit: usize,
) -> Option<Arc<AtomicUsize>>
where
    K: Eq + std::hash::Hash + Copy,
{
    let counter = map
        .entry(key)
        .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
        .clone();
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            (value < limit).then_some(value + 1)
        })
        .ok()
        .map(|_| counter)
}

fn release<K>(map: &DashMap<K, Arc<AtomicUsize>>, key: K, counter: &Arc<AtomicUsize>)
where
    K: Eq + std::hash::Hash + Copy,
{
    if counter.fetch_sub(1, Ordering::AcqRel) != 1 {
        return;
    }
    if let Entry::Occupied(entry) = map.entry(key) {
        if Arc::ptr_eq(entry.get(), counter)
            && counter.load(Ordering::Acquire) == 0
            && Arc::strong_count(counter) == 2
        {
            entry.remove();
        }
    }
}

#[cfg(test)]
#[path = "proxy_admission/tests.rs"]
mod tests;
