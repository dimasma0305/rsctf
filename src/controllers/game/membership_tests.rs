use super::*;

use std::str::FromStr;
use std::sync::Arc;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

async fn assert_combined_lock_bypasses_local_game_pool_cycle(pool: &sqlx::PgPool) {
    let user_id = Uuid::new_v4();
    let game_id = 900_000 + i32::from(user_id.as_bytes()[0]);
    let game_key = crate::services::ad_engine::game_lock_key(game_id);
    let (game_held_tx, game_held_rx) = tokio::sync::oneshot::channel();
    let (engine_continue_tx, engine_continue_rx) = tokio::sync::oneshot::channel();

    let engine = tokio::spawn({
        let pool = pool.clone();
        let game_key = game_key.clone();
        async move {
            let local = crate::utils::single_flight::coalesce(&game_key).await;
            game_held_tx.send(()).unwrap();
            engine_continue_rx.await.unwrap();
            let database = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, &game_key)
                .await
                .unwrap();
            database.release().await.unwrap();
            drop(local);
        }
    });
    game_held_rx.await.unwrap();

    let join = tokio::spawn({
        let pool = pool.clone();
        async move {
            let mut locks = MembershipMutationLocks::acquire(&pool, user_id, game_id, 777, true)
                .await
                .unwrap();
            locks.acquire_game_advisory().await.unwrap();
            locks.release().await.unwrap();
        }
    });

    // The engine has reserved the process-local game coalescer but not a DB
    // connection. A combined membership path must use the authoritative DB
    // advisory directly; waiting for the local optimization would deadlock if
    // it retained this one-connection pool.
    tokio::time::timeout(std::time::Duration::from_millis(500), join)
        .await
        .expect("combined join waited on the local game coalescer")
        .unwrap();
    engine_continue_tx.send(()).unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), engine)
        .await
        .expect("ordered engine/join locks must complete without pool deadlock")
        .expect("engine task failed");
}

async fn emulate_replica_join(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    game_id: i32,
    team_id: i32,
    barrier: Arc<tokio::sync::Barrier>,
) -> AppResult<i32> {
    // Skip the process-local coalescer intentionally: the two tasks represent
    // separate web replicas and must be serialized by PostgreSQL alone.
    barrier.wait().await;
    let membership_key = game_membership_lock_key(user_id, game_id);
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &membership_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .acquire_additional(&format!("team-roster:{team_id}"))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .acquire_additional(&crate::services::ad_engine::game_lock_key(game_id))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let token = format!("team-{team_id}-token");
    let persisted = persist_game_join_locked(
        control.transaction_mut(),
        JoinMutation {
            user_id,
            game_id,
            team_id,
            division_id: None,
            target_status: ParticipationStatus::Accepted,
            token: &token,
            member_limit: 0,
        },
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(persisted.participation_id)
}

async fn emulate_leave_replica(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    game_id: i32,
    team_id: i32,
    barrier: Arc<tokio::sync::Barrier>,
) -> AppResult<bool> {
    barrier.wait().await;
    let membership_key = game_membership_lock_key(user_id, game_id);
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &membership_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .acquire_additional(&format!("team-roster:{team_id}"))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let live: Option<(i32, i16)> = sqlx::query_as(
        r#"SELECT participation.id, participation.status
              FROM "UserParticipations" membership
              JOIN "Participations" participation
                ON participation.id = membership.participation_id
             WHERE membership.user_id = $1 AND membership.game_id = $2
             FOR UPDATE OF membership, participation"#,
    )
    .bind(user_id)
    .bind(game_id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let deleted = if let Some((participation_id, status)) = live {
        if matches!(
            status,
            value if value == ParticipationStatus::Pending as i16
                || value == ParticipationStatus::Rejected as i16
        ) {
            sqlx::query(
                r#"DELETE FROM "UserParticipations"
                    WHERE user_id = $1 AND game_id = $2 AND participation_id = $3"#,
            )
            .bind(user_id)
            .bind(game_id)
            .bind(participation_id)
            .execute(&mut **control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            sqlx::query(
                r#"DELETE FROM "Participations"
                    WHERE id = $1 AND status IN ($2, $3)"#,
            )
            .bind(participation_id)
            .bind(ParticipationStatus::Pending as i16)
            .bind(ParticipationStatus::Rejected as i16)
            .execute(&mut **control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            true
        } else {
            false
        }
    } else {
        false
    };
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(deleted)
}

async fn emulate_review_replica(
    pool: &sqlx::PgPool,
    participation_id: i32,
    game_id: i32,
    team_id: i32,
    barrier: Arc<tokio::sync::Barrier>,
) -> AppResult<bool> {
    barrier.wait().await;
    let team_key = format!("team-roster:{team_id}");
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &team_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .acquire_additional(&crate::services::ad_engine::game_lock_key(game_id))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let updated = sqlx::query(
        r#"UPDATE "Participations"
              SET status = $1
            WHERE id = $2 AND game_id = $3"#,
    )
    .bind(ParticipationStatus::Accepted as i16)
    .bind(participation_id)
    .bind(game_id)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected()
        == 1;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(updated)
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_cross_team_join_commits_one_link_and_no_orphan() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_join_race_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated test schema");

    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        // The accepted path must need only its retained membership transaction.
        // If it tries to nest a second pool connection for the game lock, these
        // two concurrent joins deadlock and the test's timeout fails.
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("connect isolated test pool");
    assert_combined_lock_bypasses_local_game_pool_cycle(&pool).await;
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          end_time_utc TIMESTAMPTZ NOT NULL,
          practice_mode BOOLEAN NOT NULL,
          accept_without_review BOOLEAN NOT NULL,
          invite_code TEXT,
          team_member_count_limit INTEGER NOT NULL
        );
        CREATE TABLE "Divisions" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          invite_code TEXT,
          default_permissions INTEGER NOT NULL
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          name TEXT NOT NULL,
          captain_id UUID NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "TeamMembers" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL
        );
        CREATE TABLE "Participations" (
          id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
          status SMALLINT NOT NULL,
          token TEXT NOT NULL,
          writeup_id INTEGER,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          division_id INTEGER,
          suspicion_score INTEGER NOT NULL
        );
        CREATE TABLE "UserParticipations" (
          user_id UUID NOT NULL,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          PRIMARY KEY (user_id, game_id)
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create membership tables");

    let user_id = Uuid::new_v4();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let first = tokio::spawn({
        let pool = pool.clone();
        let barrier = barrier.clone();
        async move { emulate_replica_join(&pool, user_id, 41, 101, barrier).await }
    });
    let second = tokio::spawn({
        let pool = pool.clone();
        let barrier = barrier.clone();
        async move { emulate_replica_join(&pool, user_id, 41, 202, barrier).await }
    });
    barrier.wait().await;

    let outcomes = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        [first.await.unwrap(), second.await.unwrap()]
    })
    .await
    .expect("single-connection accepted joins must not pool-deadlock");
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    let loser = outcomes
        .iter()
        .find_map(|result| result.as_ref().err())
        .expect("one cross-team join must lose");
    assert_eq!(loser.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(loser.to_string(), "Already participating in this game");

    let participation_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Participations""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    let membership_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "UserParticipations""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    let orphan_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
              FROM "Participations" participation
             WHERE NOT EXISTS (
                 SELECT 1 FROM "UserParticipations" membership
                  WHERE membership.participation_id = participation.id
             )"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(participation_count, 1);
    assert_eq!(membership_count, 1);
    assert_eq!(orphan_count, 0);

    // A request may have observed the old invite and auto-accept policy before
    // waiting. The authoritative resolver must use the post-edit values once it
    // owns the game lock, and division policy must take precedence immediately.
    sqlx::query(
        r#"INSERT INTO "Games"
             (id, end_time_utc, practice_mode, accept_without_review,
              invite_code, team_member_count_limit)
           VALUES (990, now() + interval '1 hour', FALSE, TRUE, 'old', 7)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "Games"
              SET invite_code = 'new', accept_without_review = FALSE
            WHERE id = 990"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let policy_user = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "Teams" (id, name, captain_id, deletion_pending)
           VALUES (991, 'deleting', $1, TRUE)"#,
    )
    .bind(policy_user)
    .execute(&pool)
    .await
    .unwrap();
    let mut deletion_locks = MembershipMutationLocks::acquire(&pool, policy_user, 990, 991, true)
        .await
        .unwrap();
    deletion_locks.acquire_game_advisory().await.unwrap();
    let deleting = load_join_team_locked(deletion_locks.transaction_mut(), 991, policy_user)
        .await
        .unwrap_err();
    assert_eq!(deleting.status(), axum::http::StatusCode::CONFLICT);
    deletion_locks.release().await.unwrap();
    sqlx::query(r#"UPDATE "Teams" SET deletion_pending = FALSE WHERE id = 991"#)
        .execute(&pool)
        .await
        .unwrap();

    let mut policy_locks = MembershipMutationLocks::acquire(&pool, policy_user, 990, 991, true)
        .await
        .unwrap();
    policy_locks.acquire_game_advisory().await.unwrap();
    assert_eq!(
        load_join_team_locked(policy_locks.transaction_mut(), 991, policy_user)
            .await
            .unwrap(),
        "deleting"
    );
    let stale = resolve_join_policy_locked(policy_locks.transaction_mut(), 990, None, Some("old"))
        .await
        .unwrap_err();
    assert_eq!(stale.to_string(), "Invalid invitation code");
    let live = resolve_join_policy_locked(policy_locks.transaction_mut(), 990, None, Some("new"))
        .await
        .unwrap();
    assert_eq!(live.target_status, ParticipationStatus::Pending);
    assert_eq!(live.member_limit, 7);
    policy_locks.release().await.unwrap();

    sqlx::query(
        r#"INSERT INTO "Divisions" (id, game_id, invite_code, default_permissions)
           VALUES (991, 990, 'division-new', $1)"#,
    )
    .bind(GamePermission::JOIN_GAME | GamePermission::REQUIRE_REVIEW)
    .execute(&pool)
    .await
    .unwrap();
    let mut policy_locks = MembershipMutationLocks::acquire(&pool, policy_user, 990, 991, true)
        .await
        .unwrap();
    policy_locks.acquire_game_advisory().await.unwrap();
    let missing_division =
        resolve_join_policy_locked(policy_locks.transaction_mut(), 990, None, Some("new"))
            .await
            .unwrap_err();
    assert_eq!(missing_division.to_string(), "A division must be selected");
    let stale_division =
        resolve_join_policy_locked(policy_locks.transaction_mut(), 990, Some(991), Some("new"))
            .await
            .unwrap_err();
    assert_eq!(stale_division.to_string(), "Invalid invitation code");
    let live_division = resolve_join_policy_locked(
        policy_locks.transaction_mut(),
        990,
        Some(991),
        Some("division-new"),
    )
    .await
    .unwrap();
    assert_eq!(live_division.division_id, Some(991));
    assert_eq!(live_division.target_status, ParticipationStatus::Pending);
    policy_locks.release().await.unwrap();

    sqlx::query(r#"TRUNCATE "UserParticipations", "Participations" RESTART IDENTITY"#)
        .execute(&pool)
        .await
        .unwrap();
    for (offset, existing_status) in [ParticipationStatus::Pending, ParticipationStatus::Suspended]
        .into_iter()
        .enumerate()
    {
        let game_id = 70 + offset as i32;
        let team_id = 300 + offset as i32;
        sqlx::query(
            r#"INSERT INTO "Participations"
                 (status, token, game_id, team_id, suspicion_score)
               VALUES ($1, 'existing', $2, $3, 0)"#,
        )
        .bind(existing_status as i16)
        .bind(game_id)
        .bind(team_id)
        .execute(&pool)
        .await
        .unwrap();

        let user_id = Uuid::new_v4();
        let mut locks = MembershipMutationLocks::acquire(&pool, user_id, game_id, team_id, true)
            .await
            .unwrap();
        let persisted = persist_game_join_locked(
            locks.transaction_mut(),
            JoinMutation {
                user_id,
                game_id,
                team_id,
                division_id: None,
                target_status: ParticipationStatus::Accepted,
                token: "new",
                member_limit: 0,
            },
        )
        .await
        .unwrap();
        assert_eq!(persisted.status, existing_status);
        assert!(!persisted.is_accepted());
        locks.release().await.unwrap();

        let stored_status: i16 = sqlx::query_scalar(
            r#"SELECT status FROM "Participations"
                WHERE game_id = $1 AND team_id = $2"#,
        )
        .bind(game_id)
        .bind(team_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored_status, existing_status as i16);
    }

    // Emulate leave and admin acceptance arriving on different replicas. The
    // only valid terminal states are an accepted linked participation or a
    // complete pending-row deletion; an accepted orphan is never permitted.
    for iteration in 0..16 {
        sqlx::query(r#"TRUNCATE "UserParticipations", "Participations" RESTART IDENTITY"#)
            .execute(&pool)
            .await
            .unwrap();
        let game_id = 500 + iteration;
        let team_id = 700 + iteration;
        let user_id = Uuid::new_v4();
        let participation_id: i32 = sqlx::query_scalar(
            r#"INSERT INTO "Participations"
                 (status, token, game_id, team_id, suspicion_score)
               VALUES ($1, 'race', $2, $3, 0)
               RETURNING id"#,
        )
        .bind(ParticipationStatus::Pending as i16)
        .bind(game_id)
        .bind(team_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "UserParticipations"
                 (user_id, game_id, team_id, participation_id)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(user_id)
        .bind(game_id)
        .bind(team_id)
        .bind(participation_id)
        .execute(&pool)
        .await
        .unwrap();

        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let leaving = tokio::spawn({
            let pool = pool.clone();
            let barrier = barrier.clone();
            async move { emulate_leave_replica(&pool, user_id, game_id, team_id, barrier).await }
        });
        let accepting = tokio::spawn({
            let pool = pool.clone();
            let barrier = barrier.clone();
            async move {
                emulate_review_replica(&pool, participation_id, game_id, team_id, barrier).await
            }
        });
        barrier.wait().await;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            leaving.await.unwrap().unwrap();
            accepting.await.unwrap().unwrap();
        })
        .await
        .expect("leave/review emulation pool-deadlocked");

        let terminal: Option<(i16, bool)> = sqlx::query_as(
            r#"SELECT participation.status,
                      EXISTS(SELECT 1 FROM "UserParticipations" membership
                              WHERE membership.participation_id = participation.id)
                 FROM "Participations" participation
                WHERE participation.id = $1"#,
        )
        .bind(participation_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(terminal.is_none_or(|(status, linked)| {
            status == ParticipationStatus::Accepted as i16 && linked
        }));
    }

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated test schema");
}
