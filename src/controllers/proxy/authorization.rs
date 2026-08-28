//! Live authorization snapshot shared by proxy opens and established leases.

use std::net::Ipv4Addr;
use std::sync::LazyLock;
use std::time::Duration;

use uuid::Uuid;

use crate::services::live_roster::LiveParticipationIdentity;
use crate::utils::enums::{ChallengeReviewStatus, GamePermission, ParticipationStatus, Role};

mod exercise;
pub(super) mod lease_cache;
pub(super) use exercise::exercise_lease_is_valid;
#[cfg(test)]
pub(super) use exercise::EXERCISE_LEASE_FRESHNESS;
use lease_cache::LeaseCache;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct GameProxyTargetIdentity {
    pub(super) container_id: Uuid,
    pub(super) runtime_id: String,
    pub(super) ip: String,
    pub(super) port: i32,
    /// `Some` is an exact per-participation instance; `None` is the challenge's
    /// exact shared container.
    pub(super) game_instance_id: Option<i32>,
}

pub(super) const GAME_PROXY_SCOPE_SQL: &str = r#"SELECT EXISTS (
    SELECT 1
      FROM "Games" game
      JOIN "Participations" participation
        ON participation.game_id = game.id
       AND participation.id = $3
      JOIN "Teams" team ON team.id = participation.team_id
      JOIN "UserParticipations" membership
        ON membership.game_id = game.id
       AND membership.user_id = $1
       AND membership.participation_id = participation.id
      JOIN "AspNetUsers" account ON account.id = membership.user_id
      JOIN "GameChallenges" challenge
        ON challenge.game_id = game.id
       AND challenge.id = $4
      JOIN "Containers" container
        ON container.id = $9
     WHERE game.id = $2
       AND game.deletion_pending = FALSE
       AND participation.status = $5
       AND team.deletion_pending = FALSE
       AND account.role <> $6
       AND challenge.is_enabled = TRUE
       AND challenge.deletion_pending = FALSE
       AND challenge.review_status = $7
       AND container.is_proxy = TRUE
       AND container.container_id = $10
       AND container.ip = $11
       AND container.port = $12
       AND container.exercise_instance_id IS NULL
       AND (
            (
                $13::integer IS NULL
                AND container.game_instance_id IS NULL
                AND challenge.shared_container_id = container.id
            )
            OR (
                $13::integer IS NOT NULL
                AND container.game_instance_id = $13
                AND EXISTS (
                    SELECT 1
                      FROM "GameInstances" instance
                     WHERE instance.id = $13
                       AND instance.container_id = container.id
                       AND instance.participation_id = participation.id
                       AND instance.challenge_id = challenge.id
                       FOR SHARE OF instance
                )
            )
       )
       AND (
            participation.division_id IS NULL
            OR EXISTS (
                SELECT 1
                  FROM "Divisions" division
                 WHERE division.id = participation.division_id
                   AND division.game_id = game.id
                   AND (
                COALESCE(
                    (
                        SELECT permission.permissions
                          FROM "DivisionChallengeConfigs" permission
                         WHERE permission.division_id = division.id
                           AND permission.challenge_id = challenge.id
                    ),
                    division.default_permissions,
                    0
                ) & $8
                   ) = $8
                   FOR SHARE OF division
            )
       )
       FOR SHARE OF game, participation, team, membership, account, challenge, container
)"#;

/// A final player-proxy authorization fence retained only while the access
/// audit is committed and the backend stream is opened. Roster mutations and
/// challenge/division edits therefore linearize wholly before or after that
/// boundary; the database transaction is released before session streaming.
pub(super) struct GameProxyOpenFence {
    roster: crate::utils::single_flight::PgAdvisoryLock,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl GameProxyOpenFence {
    pub(super) fn transaction_mut(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Postgres> {
        self.roster.transaction_mut()
    }

    pub(super) async fn release(self) -> bool {
        self.roster.release().await.is_ok()
    }

    pub(super) async fn rollback(self) {
        let _ = self.roster.rollback().await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn game_proxy_scope_is_valid_on(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    target: &GameProxyTargetIdentity,
) -> crate::utils::error::AppResult<bool> {
    sqlx::query_scalar::<_, bool>(GAME_PROXY_SCOPE_SQL)
        .bind(user_id)
        .bind(game_id)
        .bind(participation_id)
        .bind(challenge_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(Role::Banned as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(GamePermission::VIEW_CHALLENGE)
        .bind(target.container_id)
        .bind(&target.runtime_id)
        .bind(&target.ip)
        .bind(target.port)
        .bind(target.game_instance_id)
        .fetch_one(connection)
        .await
        .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))
}

async fn try_acquire_game_proxy_scope_guard(
    pool: &sqlx::PgPool,
    caller: LiveParticipationIdentity<'_>,
    challenge_id: i32,
    target: &GameProxyTargetIdentity,
) -> crate::utils::error::AppResult<Option<crate::utils::single_flight::PgAdvisoryLock>> {
    let Some(mut roster) = crate::services::live_roster::try_acquire_participation_fence(
        pool,
        caller.user_id,
        caller.expected_security_stamp,
        caller.game_id,
        caller.team_id,
        caller.participation_id,
        true,
    )
    .await?
    else {
        return Ok(None);
    };
    let scope_valid = game_proxy_scope_is_valid_on(
        roster.transaction_mut(),
        caller.user_id,
        caller.game_id,
        caller.participation_id,
        challenge_id,
        target,
    )
    .await?;
    if !scope_valid {
        roster
            .release()
            .await
            .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
        return Ok(None);
    }
    Ok(Some(roster))
}

/// Reauthorize after the WebSocket upgrade and retain the bounded lock through
/// the access-evidence insert and backend stream open. A small shared permit
/// prevents slow or unreachable backends from consuming the PostgreSQL pool.
pub(super) async fn try_acquire_game_proxy_open_fence(
    pool: &sqlx::PgPool,
    caller: LiveParticipationIdentity<'_>,
    challenge_id: i32,
    target: &GameProxyTargetIdentity,
    source: Option<Ipv4Addr>,
    bypass_event_vpn: bool,
) -> Option<GameProxyOpenFence> {
    let permit = crate::utils::single_flight::roster_access_permit()
        .await
        .ok()?;
    // Specialized final-boundary order: take only the roster advisory first,
    // then the suspicion advisory, and only then row-lock live identity/scope.
    // Access-audit insertion continues on this same transaction, avoiding both
    // detector lock inversion and a nested pool checkout.
    let roster_key = crate::services::live_roster::lock_key(caller.team_id);
    let mut roster =
        crate::utils::single_flight::PgAdvisoryLock::try_acquire_shared(pool, &roster_key)
            .await
            .ok()??;
    if crate::services::suspicion::lock_participation_suspicion_writes(
        roster.transaction_mut(),
        caller.participation_id,
    )
    .await
    .is_err()
    {
        let _ = roster.rollback().await;
        return None;
    }
    let live = crate::services::live_roster::participation_caller_is_live_on(
        roster.transaction_mut().as_mut(),
        caller.user_id,
        caller.expected_security_stamp,
        caller.game_id,
        caller.team_id,
        caller.participation_id,
        true,
    )
    .await;
    if !matches!(live, Ok(true)) {
        let _ = roster.rollback().await;
        return None;
    }
    let scope_valid = game_proxy_scope_is_valid_on(
        roster.transaction_mut(),
        caller.user_id,
        caller.game_id,
        caller.participation_id,
        challenge_id,
        target,
    )
    .await;
    if !matches!(scope_valid, Ok(true)) {
        let _ = roster.rollback().await;
        return None;
    }
    if !bypass_event_vpn
        && crate::services::event_security::require_event_vpn_source_on(
            roster.transaction_mut().as_mut(),
            caller.game_id,
            caller.user_id,
            caller.participation_id,
            source,
        )
        .await
        .is_err()
    {
        let _ = roster.rollback().await;
        return None;
    }
    Some(GameProxyOpenFence {
        roster,
        _permit: permit,
    })
}

pub(super) async fn game_proxy_session_is_valid(
    pool: &sqlx::PgPool,
    caller: LiveParticipationIdentity<'_>,
    challenge_id: i32,
    target: &GameProxyTargetIdentity,
    source: Option<Ipv4Addr>,
    bypass_event_vpn: bool,
) -> bool {
    let key = GameLeaseKey {
        user_id: caller.user_id,
        security_stamp: caller.expected_security_stamp.to_owned(),
        game_id: caller.game_id,
        team_id: caller.team_id,
        participation_id: caller.participation_id,
        challenge_id,
        target: target.clone(),
        source,
        bypass_event_vpn,
    };
    GAME_LEASES
        .validate(key, || {
            game_proxy_session_is_valid_authoritative(
                pool,
                caller,
                challenge_id,
                target,
                source,
                bypass_event_vpn,
            )
        })
        .await
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GameLeaseKey {
    user_id: Uuid,
    security_stamp: String,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
    challenge_id: i32,
    target: GameProxyTargetIdentity,
    source: Option<Ipv4Addr>,
    bypass_event_vpn: bool,
}

static GAME_LEASES: LazyLock<LeaseCache<GameLeaseKey>> =
    LazyLock::new(|| LeaseCache::new(8_192, Duration::from_millis(250)));

async fn game_proxy_session_is_valid_authoritative(
    pool: &sqlx::PgPool,
    caller: LiveParticipationIdentity<'_>,
    challenge_id: i32,
    target: &GameProxyTargetIdentity,
    source: Option<Ipv4Addr>,
    bypass_event_vpn: bool,
) -> bool {
    let Ok(Some(mut roster)) =
        try_acquire_game_proxy_scope_guard(pool, caller, challenge_id, target).await
    else {
        return false;
    };
    if !bypass_event_vpn
        && crate::services::event_security::require_event_vpn_source_on(
            roster.transaction_mut().as_mut(),
            caller.game_id,
            caller.user_id,
            caller.participation_id,
            source,
        )
        .await
        .is_err()
    {
        let _ = roster.rollback().await;
        return false;
    }
    roster.release().await.is_ok()
}

/// Fail closed if any mutable owner of a player proxy is being removed or is
/// no longer eligible. Event time is deliberately absent: finished games may
/// expose their containers for practice until an organizer disables them.
pub(super) async fn game_proxy_scope_is_valid(
    pool: &sqlx::PgPool,
    caller: LiveParticipationIdentity<'_>,
    challenge_id: i32,
    target: &GameProxyTargetIdentity,
) -> bool {
    let Ok(Some(roster)) =
        try_acquire_game_proxy_scope_guard(pool, caller, challenge_id, target).await
    else {
        return false;
    };
    roster.release().await.is_ok()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn game_proxy_scope_keeps_every_revocation_gate_in_one_snapshot() {
        for gate in [
            "game.deletion_pending = FALSE",
            "participation.status = $5",
            "team.deletion_pending = FALSE",
            "account.role <> $6",
            "challenge.is_enabled = TRUE",
            "challenge.deletion_pending = FALSE",
            "challenge.review_status = $7",
            "membership.participation_id = participation.id",
            "permission.permissions",
            "division.default_permissions",
            ") & $8",
            "container.id = $9",
            "container.container_id = $10",
            "container.ip = $11",
            "container.port = $12",
            "challenge.shared_container_id = container.id",
            "instance.container_id = container.id",
            "instance.participation_id = participation.id",
            "instance.challenge_id = challenge.id",
        ] {
            assert!(GAME_PROXY_SCOPE_SQL.contains(gate), "missing gate: {gate}");
        }
        let source = include_str!("authorization.rs");
        let final_boundary = source
            .find("pub(super) async fn try_acquire_game_proxy_open_fence")
            .unwrap();
        let final_boundary = &source[final_boundary
            ..source
                .find("pub(super) async fn game_proxy_scope_is_valid")
                .unwrap()];
        let roster = final_boundary.find("try_acquire_shared").unwrap();
        let suspicion = final_boundary
            .find("lock_participation_suspicion_writes")
            .unwrap();
        let live_rows = final_boundary
            .find("participation_caller_is_live_on")
            .unwrap();
        assert!(roster < suspicion && suspicion < live_rows);
    }

    async fn fixture_scope_is_valid(
        pool: &sqlx::PgPool,
        user_id: Uuid,
        target: &GameProxyTargetIdentity,
    ) -> bool {
        game_proxy_scope_is_valid(
            pool,
            LiveParticipationIdentity {
                user_id,
                expected_security_stamp: "stamp",
                game_id: 1,
                team_id: 2,
                participation_id: 3,
            },
            4,
            target,
        )
        .await
    }

    async fn fixture_open_fence(
        pool: &sqlx::PgPool,
        user_id: Uuid,
        target: &GameProxyTargetIdentity,
    ) -> Option<GameProxyOpenFence> {
        try_acquire_game_proxy_open_fence(
            pool,
            LiveParticipationIdentity {
                user_id,
                expected_security_stamp: "stamp",
                game_id: 1,
                team_id: 2,
                participation_id: 3,
            },
            4,
            target,
            None,
            true,
        )
        .await
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn game_proxy_scope_revokes_initial_and_lease_authorization() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("proxy_scope_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              start_time_utc TIMESTAMPTZ NOT NULL DEFAULT
                  (CURRENT_TIMESTAMP + interval '1 hour'),
              end_time_utc TIMESTAMPTZ NOT NULL DEFAULT
                  (CURRENT_TIMESTAMP + interval '2 hours')
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              captain_id UUID NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "TeamMembers" (
              team_id INTEGER NOT NULL, user_id UUID NOT NULL,
              PRIMARY KEY (team_id, user_id)
            );
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY, role SMALLINT NOT NULL,
              email_confirmed BOOLEAN NOT NULL, security_stamp TEXT
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, status SMALLINT NOT NULL,
              division_id INTEGER
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, participation_id INTEGER NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              is_enabled BOOLEAN NOT NULL, deletion_pending BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL, shared_container_id UUID
            );
            CREATE TABLE "Containers" (
              id UUID PRIMARY KEY, container_id TEXT NOT NULL,
              is_proxy BOOLEAN NOT NULL, ip TEXT NOT NULL, port INTEGER NOT NULL,
              game_instance_id INTEGER, exercise_instance_id INTEGER
            );
            CREATE TABLE "GameInstances" (
              id INTEGER PRIMARY KEY, challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, container_id UUID
            );
            CREATE TABLE "Divisions" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              default_permissions INTEGER NOT NULL
            );
            CREATE TABLE "DivisionChallengeConfigs" (
              division_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              permissions INTEGER NOT NULL,
              PRIMARY KEY (division_id, challenge_id)
            );
            CREATE TABLE "IdentityObservations" (
              user_id UUID NOT NULL, game_id INTEGER,
              team_id INTEGER, participation_id INTEGER,
              observed_at_utc TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let user_id = uuid::Uuid::new_v4();
        let captain_id = uuid::Uuid::new_v4();
        let container_id = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Games" (id, deletion_pending) VALUES (1, FALSE)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" VALUES (2, $1, FALSE)"#)
            .bind(captain_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "TeamMembers" VALUES (2, $1)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, $2, TRUE, 'stamp')"#)
            .bind(user_id)
            .bind(Role::User as i16)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations" (id, game_id, team_id, status)
               VALUES (3, 1, 2, $1)"#,
        )
        .bind(ParticipationStatus::Accepted as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "UserParticipations" VALUES ($1, 1, 2, 3)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
                   (id, game_id, is_enabled, deletion_pending, review_status)
               VALUES (4, 1, TRUE, FALSE, $1)"#,
        )
        .bind(ChallengeReviewStatus::Active as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "Containers"
                   (id, container_id, is_proxy, ip, port, game_instance_id)
               VALUES ($1, 'runtime-1', TRUE, '127.0.0.1', 31337, 5)"#,
        )
        .bind(container_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameInstances"
                   (id, challenge_id, participation_id, container_id)
               VALUES (5, 4, 3, $1)"#,
        )
        .bind(container_id)
        .execute(&pool)
        .await
        .unwrap();
        let target = GameProxyTargetIdentity {
            container_id,
            runtime_id: "runtime-1".to_string(),
            ip: "127.0.0.1".to_string(),
            port: 31337,
            game_instance_id: Some(5),
        };
        assert!(fixture_scope_is_valid(&pool, user_id, &target).await);

        // The request resolver captured this exact runtime/instance endpoint.
        // A restart or reassignment during the HTTP upgrade invalidates the
        // final fence instead of opening and auditing the stale endpoint.
        sqlx::query(r#"UPDATE "Containers" SET container_id = 'runtime-2' WHERE id = $1"#)
            .bind(container_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(fixture_open_fence(&pool, user_id, &target).await.is_none());
        sqlx::query(r#"UPDATE "Containers" SET container_id = 'runtime-1' WHERE id = $1"#)
            .bind(container_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"UPDATE "GameInstances" SET participation_id = 99 WHERE id = 5"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(fixture_open_fence(&pool, user_id, &target).await.is_none());
        sqlx::query(r#"UPDATE "GameInstances" SET participation_id = 3 WHERE id = 5"#)
            .execute(&pool)
            .await
            .unwrap();

        // Shared instances use the other exact relation: challenge pointer to
        // an unowned proxy container. Detaching that pointer fails closed.
        sqlx::query(r#"UPDATE "Containers" SET game_instance_id = NULL WHERE id = $1"#)
            .bind(container_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"UPDATE "GameChallenges" SET shared_container_id = $1 WHERE id = 4"#)
            .bind(container_id)
            .execute(&pool)
            .await
            .unwrap();
        let shared_target = GameProxyTargetIdentity {
            game_instance_id: None,
            ..target.clone()
        };
        assert!(fixture_scope_is_valid(&pool, user_id, &shared_target).await);
        sqlx::query(r#"UPDATE "GameChallenges" SET shared_container_id = NULL WHERE id = 4"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(fixture_open_fence(&pool, user_id, &shared_target)
            .await
            .is_none());
        sqlx::query(r#"UPDATE "Containers" SET game_instance_id = 5 WHERE id = $1"#)
            .bind(container_id)
            .execute(&pool)
            .await
            .unwrap();

        // A detector may already own the participation suspicion advisory and
        // then lock the Participation row. The specialized final boundary must
        // wait on that advisory before taking any row locks, so this ordering
        // cannot form detector<->proxy deadlock.
        let mut detector = pool.begin().await.unwrap();
        let detector_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *detector)
            .await
            .unwrap();
        crate::services::suspicion::lock_participation_suspicion_writes(&mut detector, 3)
            .await
            .unwrap();
        let queued_open = tokio::spawn({
            let pool = pool.clone();
            let target = target.clone();
            async move { fixture_open_fence(&pool, user_id, &target).await }
        });
        let scheduling_timeout = std::time::Duration::from_secs(10);
        tokio::time::timeout(scheduling_timeout, async {
            loop {
                let waiting_on_detector: bool = sqlx::query_scalar(
                    r#"SELECT EXISTS (
                           SELECT 1
                             FROM pg_locks held
                             JOIN pg_locks waiting
                               ON waiting.locktype = held.locktype
                              AND waiting.database IS NOT DISTINCT FROM held.database
                              AND waiting.classid IS NOT DISTINCT FROM held.classid
                              AND waiting.objid IS NOT DISTINCT FROM held.objid
                              AND waiting.objsubid IS NOT DISTINCT FROM held.objsubid
                            WHERE held.pid = $1
                              AND held.locktype = 'advisory'
                              AND held.granted
                              AND NOT waiting.granted
                       )"#,
                )
                .bind(detector_pid)
                .fetch_one(&pool)
                .await
                .unwrap();
                if waiting_on_detector {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("proxy never reached the detector suspicion wait");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sqlx::query(r#"SELECT id FROM "Participations" WHERE id = 3 FOR UPDATE"#)
                .execute(&mut *detector),
        )
        .await
        .expect("proxy row locks inverted the detector suspicion order")
        .unwrap();
        detector.commit().await.unwrap();
        let queued_open = tokio::time::timeout(scheduling_timeout, queued_open)
            .await
            .expect("proxy did not resume after detector commit")
            .unwrap()
            .expect("proxy final boundary failed after detector released");
        queued_open.rollback().await;

        // Request-level resolution is only a preflight. A kick which commits
        // before the upgraded socket's final boundary must make that boundary
        // fail, even though the historical participation row remains.
        let preopen = fixture_open_fence(&pool, user_id, &target)
            .await
            .expect("initial upgraded-socket fence");
        let roster_key = crate::services::live_roster::lock_key(2);
        assert!(
            crate::utils::single_flight::PgAdvisoryLock::try_acquire(&pool, &roster_key)
                .await
                .unwrap()
                .is_none(),
            "kick interleaved with the bounded proxy-open fence"
        );
        let mut restart = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '100ms'")
            .execute(&mut *restart)
            .await
            .unwrap();
        let blocked = sqlx::query(r#"UPDATE "Containers" SET port = 31338 WHERE id = $1"#)
            .bind(container_id)
            .execute(&mut *restart)
            .await
            .expect_err("runtime restart interleaved with the bounded proxy-open fence");
        assert!(matches!(
            blocked,
            sqlx::Error::Database(ref database)
                if database.code().as_deref() == Some("55P03")
        ));
        restart.rollback().await.unwrap();
        assert!(preopen.release().await);
        let mut kick = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, &roster_key)
            .await
            .unwrap();
        sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = 2 AND user_id = $1"#)
            .bind(user_id)
            .execute(&mut **kick.transaction_mut())
            .await
            .unwrap();
        kick.release().await.unwrap();
        assert!(
            fixture_open_fence(&pool, user_id, &target).await.is_none(),
            "final upgrade boundary trusted the stale request-level resolution"
        );
        sqlx::query(r#"INSERT INTO "TeamMembers" VALUES (2, $1)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();

        // The same authoritative helper is used both before opening a proxy
        // and by every established lease tick. Changing the effective
        // division policy must therefore revoke either phase immediately.
        sqlx::query(
            r#"INSERT INTO "Divisions" (id, game_id, default_permissions)
               VALUES (8, 1, 0)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"UPDATE "Participations" SET division_id = 8 WHERE id = 3"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !fixture_scope_is_valid(&pool, user_id, &target).await,
            "division default denial did not prevent proxy open"
        );
        sqlx::query(r#"UPDATE "Divisions" SET default_permissions = $1 WHERE id = 8"#)
            .bind(GamePermission::VIEW_CHALLENGE)
            .execute(&pool)
            .await
            .unwrap();
        assert!(fixture_scope_is_valid(&pool, user_id, &target).await);

        sqlx::query(
            r#"INSERT INTO "DivisionChallengeConfigs"
                   (division_id, challenge_id, permissions)
               VALUES (8, 4, 0)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            !fixture_scope_is_valid(&pool, user_id, &target).await,
            "challenge override revocation did not invalidate an existing lease"
        );
        sqlx::query(
            r#"UPDATE "DivisionChallengeConfigs" SET permissions = $1
                WHERE division_id = 8 AND challenge_id = 4"#,
        )
        .bind(GamePermission::VIEW_CHALLENGE)
        .execute(&pool)
        .await
        .unwrap();
        // An explicit challenge override wins over a denying division default.
        sqlx::query(r#"UPDATE "Divisions" SET default_permissions = 0 WHERE id = 8"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(fixture_scope_is_valid(&pool, user_id, &target).await);

        // Authorized policy writes lock the Division parent exclusively. The
        // open fence retains its shared parent row lock through backend-open,
        // preventing an effective permission change from interleaving there.
        let preopen = fixture_open_fence(&pool, user_id, &target)
            .await
            .expect("granted override should reach final boundary");
        let mut policy = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '100ms'")
            .execute(&mut *policy)
            .await
            .unwrap();
        let blocked = sqlx::query(r#"SELECT id FROM "Divisions" WHERE id = 8 FOR UPDATE"#)
            .execute(&mut *policy)
            .await
            .expect_err("division policy writer bypassed the proxy-open fence");
        assert!(matches!(
            blocked,
            sqlx::Error::Database(ref database)
                if database.code().as_deref() == Some("55P03")
        ));
        policy.rollback().await.unwrap();
        assert!(preopen.release().await);
        sqlx::query(
            r#"UPDATE "DivisionChallengeConfigs" SET permissions = 0
                WHERE division_id = 8 AND challenge_id = 4"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            fixture_open_fence(&pool, user_id, &target).await.is_none(),
            "policy revoked between resolve and upgrade still opened the proxy"
        );
        sqlx::query(
            r#"UPDATE "DivisionChallengeConfigs" SET permissions = $1
                WHERE division_id = 8 AND challenge_id = 4"#,
        )
        .bind(GamePermission::VIEW_CHALLENGE)
        .execute(&pool)
        .await
        .unwrap();

        // A dangling or cross-game division fails closed even when a stale
        // challenge override row would otherwise grant access.
        sqlx::query(r#"UPDATE "Divisions" SET game_id = 99 WHERE id = 8"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !fixture_scope_is_valid(&pool, user_id, &target).await,
            "cross-game division override authorized the proxy"
        );
        sqlx::query(r#"UPDATE "Divisions" SET game_id = 1 WHERE id = 8"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(fixture_scope_is_valid(&pool, user_id, &target).await);

        for (revoke, restore) in [
            (
                r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#,
                r#"UPDATE "Games" SET deletion_pending = FALSE WHERE id = 1"#,
            ),
            (
                r#"UPDATE "Teams" SET deletion_pending = TRUE WHERE id = 2"#,
                r#"UPDATE "Teams" SET deletion_pending = FALSE WHERE id = 2"#,
            ),
            (
                r#"UPDATE "GameChallenges" SET deletion_pending = TRUE WHERE id = 4"#,
                r#"UPDATE "GameChallenges" SET deletion_pending = FALSE WHERE id = 4"#,
            ),
            (
                r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 4"#,
                r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE id = 4"#,
            ),
        ] {
            sqlx::query(revoke).execute(&pool).await.unwrap();
            assert!(!fixture_scope_is_valid(&pool, user_id, &target).await);
            sqlx::query(restore).execute(&pool).await.unwrap();
            assert!(fixture_scope_is_valid(&pool, user_id, &target).await);
        }

        sqlx::query(r#"UPDATE "AspNetUsers" SET security_stamp = 'rotated' WHERE id = $1"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!fixture_scope_is_valid(&pool, user_id, &target).await);
        sqlx::query(
            r#"UPDATE "AspNetUsers"
                  SET security_stamp = 'stamp', email_confirmed = FALSE
                WHERE id = $1"#,
        )
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(!fixture_scope_is_valid(&pool, user_id, &target).await);
        sqlx::query(r#"UPDATE "AspNetUsers" SET email_confirmed = TRUE WHERE id = $1"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = 2 AND user_id = $1"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(SELECT 1 FROM "UserParticipations" WHERE user_id = $1)"#,
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap());
        assert!(!fixture_scope_is_valid(&pool, user_id, &target).await);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
