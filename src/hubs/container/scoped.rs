//! Game-scoped authorization for the organizer container terminal.
//!
//! The platform-wide hub stays Admin-only. This module is the narrower grant
//! used by `/hub/containerExec/games/{game_id}`: a live Admin or an exact
//! `GameManagers` member may connect, and every `Open` target is resolved back
//! to one unambiguous owner in that same game before any backend is touched.

use sqlx::PgPool;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::ChallengeType;

const GAME_ACCESS_SQL: &str = r#"
    SELECT EXISTS (
        SELECT 1
          FROM "Games" game
         WHERE game.id = $1
           AND game.deletion_pending = FALSE
    ) AND (
        $3 OR EXISTS (
            SELECT 1
              FROM "GameManagers" manager
             WHERE manager.game_id = $1
               AND manager.user_id = $2
        )
    )
"#;

/// Resolve public `Containers.id` values through every supported game owner.
/// Exactly one owner path must resolve. A stale direct FK, an exercise owner,
/// duplicate challenge references, or ownership through two domains denies the
/// target instead of guessing which association should win.
const CONTAINER_TARGET_SQL: &str = r#"
    WITH target AS (
        SELECT container.id,
               container.container_id,
               container.game_instance_id,
               container.exercise_instance_id,
               container.ad_team_service_id
          FROM "Containers" container
         WHERE container.id = $1
    ), ownership AS (
        SELECT 'instance'::text AS kind, participation.game_id
          FROM target
          JOIN "GameInstances" instance
            ON instance.id = target.game_instance_id
          JOIN "Participations" participation
            ON participation.id = instance.participation_id
          JOIN "GameChallenges" challenge
            ON challenge.id = instance.challenge_id
           AND challenge.game_id = participation.game_id
         WHERE challenge.deletion_pending = FALSE
        UNION ALL
        SELECT 'challenge-test'::text AS kind, challenge.game_id
          FROM target
          JOIN "GameChallenges" challenge
            ON challenge.test_container_id = target.id
         WHERE challenge.deletion_pending = FALSE
        UNION ALL
        SELECT 'challenge-shared'::text AS kind, challenge.game_id
          FROM target
          JOIN "GameChallenges" challenge
            ON challenge.shared_container_id = target.id
         WHERE challenge.deletion_pending = FALSE
        UNION ALL
        SELECT 'inspector'::text AS kind, service.game_id
          FROM target
          JOIN "AdTeamServices" service
            ON service.id = target.ad_team_service_id
          JOIN "Participations" participation
            ON participation.id = service.participation_id
           AND participation.game_id = service.game_id
          JOIN "GameChallenges" challenge
            ON challenge.id = service.challenge_id
           AND challenge.game_id = service.game_id
         WHERE challenge.deletion_pending = FALSE
    ), resolved AS (
        SELECT count(*) AS owner_count,
               min(ownership.game_id) AS game_id,
               min(ownership.kind) AS kind
          FROM ownership
    )
    SELECT target.container_id
      FROM target
      CROSS JOIN resolved
     WHERE target.exercise_instance_id IS NULL
       AND target.container_id <> ''
       AND resolved.owner_count = 1
       AND resolved.game_id = $2
       AND (target.game_instance_id IS NULL OR resolved.kind = 'instance')
       AND (target.ad_team_service_id IS NULL OR resolved.kind = 'inspector')
"#;

/// Raw runtime ids are accepted only when one current A&D service or KotH hill
/// resolves to the requested game. `UNION ALL` plus `count(*) = 1` makes a
/// duplicated/mixed runtime id fail closed. The connection lease checks the
/// game's deletion fence immediately before and after this ownership lookup;
/// this query independently checks the owning challenge's deletion fence.
const RAW_TARGET_SQL: &str = r#"
    WITH ownership AS (
        SELECT service.game_id, service.container_id
          FROM "AdTeamServices" service
          JOIN "Participations" participation
            ON participation.id = service.participation_id
           AND participation.game_id = service.game_id
          JOIN "GameChallenges" challenge
            ON challenge.id = service.challenge_id
           AND challenge.game_id = service.game_id
         WHERE service.container_id = $1
           AND service.container_id <> ''
           AND challenge."Type" = $3
           AND challenge.deletion_pending = FALSE
        UNION ALL
        SELECT target.game_id, target.container_id
          FROM "KothTargets" target
          JOIN "GameChallenges" challenge
            ON challenge.id = target.challenge_id
           AND challenge.game_id = target.game_id
         WHERE target.container_id = $1
           AND target.container_id <> ''
           AND challenge."Type" = $4
           AND challenge.deletion_pending = FALSE
    ), resolved AS (
        SELECT count(*) AS owner_count,
               min(ownership.game_id) AS game_id,
               min(ownership.container_id) AS container_id
          FROM ownership
    )
    SELECT resolved.container_id
      FROM resolved
     WHERE resolved.owner_count = 1
       AND resolved.game_id = $2
"#;

const BYOC_TARGET_SQL: &str = r#"
    SELECT EXISTS (
        SELECT 1
          FROM "Participations" participation
          JOIN "Games" game
            ON game.id = participation.game_id
           AND game.deletion_pending = FALSE
          JOIN "GameChallenges" challenge
            ON challenge.id = $2
           AND challenge.game_id = participation.game_id
         WHERE participation.id = $1
           AND participation.game_id = $3
           AND challenge."Type" = $4
           AND challenge.ad_self_hosted = TRUE
           AND challenge.is_enabled = TRUE
           AND challenge.deletion_pending = FALSE
    )
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ScopedExecTarget {
    Docker(String),
    Byoc {
        participation_id: i32,
        challenge_id: i32,
    },
}

pub(super) async fn game_access(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
) -> Result<bool, sqlx::Error> {
    game_access_on(st.pg(), user.id, user.is_admin(), game_id).await
}

async fn game_access_on(
    pool: &PgPool,
    user_id: Uuid,
    is_admin: bool,
    game_id: i32,
) -> Result<bool, sqlx::Error> {
    if game_id <= 0 {
        return Ok(false);
    }
    sqlx::query_scalar(GAME_ACCESS_SQL)
        .bind(game_id)
        .bind(user_id)
        .bind(is_admin)
        .fetch_one(pool)
        .await
}

pub(super) async fn authorize_target(
    st: &SharedState,
    game_id: i32,
    target: Option<&str>,
) -> Result<Option<ScopedExecTarget>, sqlx::Error> {
    authorize_target_on(st.pg(), game_id, target).await
}

async fn authorize_target_on(
    pool: &PgPool,
    game_id: i32,
    target: Option<&str>,
) -> Result<Option<ScopedExecTarget>, sqlx::Error> {
    if game_id <= 0 {
        return Ok(None);
    }
    let Some(target) = target.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    if let Some(rest) = target.strip_prefix("byoc:") {
        let Some((participation_id, challenge_id)) = parse_byoc_target(rest) else {
            return Ok(None);
        };
        let allowed: bool = sqlx::query_scalar(BYOC_TARGET_SQL)
            .bind(participation_id)
            .bind(challenge_id)
            .bind(game_id)
            .bind(ChallengeType::AttackDefense as i16)
            .fetch_one(pool)
            .await?;
        return Ok(allowed.then_some(ScopedExecTarget::Byoc {
            participation_id,
            challenge_id,
        }));
    }

    if let Ok(id) = Uuid::parse_str(target) {
        return sqlx::query_scalar::<_, String>(CONTAINER_TARGET_SQL)
            .bind(id)
            .bind(game_id)
            .fetch_optional(pool)
            .await
            .map(|runtime_id| runtime_id.map(ScopedExecTarget::Docker));
    }

    sqlx::query_scalar::<_, String>(RAW_TARGET_SQL)
        .bind(target)
        .bind(game_id)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(ChallengeType::KingOfTheHill as i16)
        .fetch_optional(pool)
        .await
        .map(|runtime_id| runtime_id.map(ScopedExecTarget::Docker))
}

fn parse_byoc_target(rest: &str) -> Option<(i32, i32)> {
    let (participation, challenge) = rest.split_once(':')?;
    if challenge.contains(':') {
        return None;
    }
    let participation_id = participation.parse::<i32>().ok()?;
    let challenge_id = challenge.parse::<i32>().ok()?;
    if participation_id <= 0
        || challenge_id <= 0
        || participation_id.to_string() != participation
        || challenge_id.to_string() != challenge
    {
        return None;
    }
    Some((participation_id, challenge_id))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn byoc_target_parser_accepts_only_two_canonical_positive_ids() {
        assert_eq!(parse_byoc_target("11:13"), Some((11, 13)));
        for malformed in [
            "", "11", "11:13:17", "0:13", "-1:13", "01:13", "11:+13", "x:13",
        ] {
            assert_eq!(parse_byoc_target(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn ownership_queries_are_bounded_and_fail_closed() {
        assert!(!GAME_ACCESS_SQL.contains(';'));
        assert!(!CONTAINER_TARGET_SQL.contains(';'));
        assert!(!RAW_TARGET_SQL.contains(';'));
        assert!(!BYOC_TARGET_SQL.contains(';'));
        for gate in [
            "resolved.owner_count = 1",
            "target.exercise_instance_id IS NULL",
            "'challenge-test'::text",
            "'challenge-shared'::text",
            "target.game_instance_id IS NULL OR resolved.kind = 'instance'",
            "target.ad_team_service_id IS NULL OR resolved.kind = 'inspector'",
        ] {
            assert!(CONTAINER_TARGET_SQL.contains(gate), "missing gate: {gate}");
        }
        assert!(RAW_TARGET_SQL.contains("resolved.owner_count = 1"));
        assert!(RAW_TARGET_SQL.contains("challenge.\"Type\" = $3"));
        assert!(RAW_TARGET_SQL.contains("challenge.\"Type\" = $4"));
        for gate in [
            "participation.game_id = $3",
            "challenge.game_id = participation.game_id",
            "challenge.ad_self_hosted = TRUE",
            "challenge.is_enabled = TRUE",
        ] {
            assert!(BYOC_TARGET_SQL.contains(gate), "missing gate: {gate}");
        }
    }

    async fn test_pool() -> (PgPool, PgPool, String) {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_scoped_exec_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "GameManagers" (
              game_id INTEGER NOT NULL, user_id UUID NOT NULL
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              test_container_id UUID, shared_container_id UUID,
              "Type" SMALLINT NOT NULL DEFAULT 0,
              ad_self_hosted BOOLEAN NOT NULL DEFAULT FALSE,
              is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "GameInstances" (
              id INTEGER PRIMARY KEY, challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            CREATE TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              container_id TEXT
            );
            CREATE TABLE "KothTargets" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, container_id TEXT
            );
            CREATE TABLE "Containers" (
              id UUID PRIMARY KEY, container_id TEXT NOT NULL,
              game_instance_id INTEGER, exercise_instance_id INTEGER,
              ad_team_service_id INTEGER
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        (admin, pool, schema)
    }

    async fn cleanup(admin: PgPool, pool: PgPool, schema: String) {
        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    async fn insert_container(
        pool: &PgPool,
        id: Uuid,
        runtime: &str,
        instance: Option<i32>,
        exercise: Option<i32>,
        inspector: Option<i32>,
    ) {
        sqlx::query(r#"INSERT INTO "Containers" VALUES ($1,$2,$3,$4,$5)"#)
            .bind(id)
            .bind(runtime)
            .bind(instance)
            .bind(exercise)
            .bind(inspector)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn exact_game_access_and_every_target_owner_are_enforced() {
        let (admin, pool, schema) = test_pool().await;
        let manager = Uuid::new_v4();
        sqlx::raw_sql(
            r#"
            INSERT INTO "Games" VALUES (1,FALSE),(2,FALSE),(3,TRUE);
            INSERT INTO "Participations" VALUES (11,1),(21,2);
            INSERT INTO "GameInstances" VALUES (31,13,11),(32,23,21);
            INSERT INTO "AdTeamServices" VALUES
              (41,1,11,13,'ad-game-1'),(42,2,21,23,'ad-game-2');
            INSERT INTO "KothTargets" VALUES (51,1,14,'koth-game-1');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        for (id, game_id, challenge_type, self_hosted) in [
            (13, 1, ChallengeType::AttackDefense, true),
            (23, 2, ChallengeType::AttackDefense, true),
            (14, 1, ChallengeType::KingOfTheHill, false),
            (24, 2, ChallengeType::KingOfTheHill, false),
        ] {
            sqlx::query(
                r#"INSERT INTO "GameChallenges"
                   (id,game_id,"Type",ad_self_hosted,is_enabled)
                   VALUES ($1,$2,$3,$4,TRUE)"#,
            )
            .bind(id)
            .bind(game_id)
            .bind(challenge_type as i16)
            .bind(self_hosted)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(r#"INSERT INTO "GameManagers" VALUES (1,$1)"#)
            .bind(manager)
            .execute(&pool)
            .await
            .unwrap();

        assert!(game_access_on(&pool, manager, false, 1).await.unwrap());
        assert!(!game_access_on(&pool, manager, false, 2).await.unwrap());
        assert!(game_access_on(&pool, manager, true, 2).await.unwrap());
        assert!(!game_access_on(&pool, manager, true, 3).await.unwrap());
        sqlx::query(r#"DELETE FROM "GameManagers" WHERE game_id=1 AND user_id=$1"#)
            .bind(manager)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!game_access_on(&pool, manager, false, 1).await.unwrap());
        sqlx::query(r#"INSERT INTO "GameManagers" VALUES (1,$1)"#)
            .bind(manager)
            .execute(&pool)
            .await
            .unwrap();

        let instance = Uuid::new_v4();
        insert_container(&pool, instance, "instance-1", Some(31), None, None).await;
        assert_eq!(
            authorize_target_on(&pool, 1, Some(&instance.to_string()))
                .await
                .unwrap(),
            Some(ScopedExecTarget::Docker("instance-1".into()))
        );
        assert_eq!(
            authorize_target_on(&pool, 2, Some(&instance.to_string()))
                .await
                .unwrap(),
            None
        );

        let test_container = Uuid::new_v4();
        insert_container(&pool, test_container, "test-1", None, None, None).await;
        sqlx::query(r#"UPDATE "GameChallenges" SET test_container_id=$1 WHERE id=13"#)
            .bind(test_container)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            authorize_target_on(&pool, 1, Some(&test_container.to_string()))
                .await
                .unwrap(),
            Some(ScopedExecTarget::Docker("test-1".into()))
        );

        let shared = Uuid::new_v4();
        insert_container(&pool, shared, "shared-1", None, None, None).await;
        sqlx::query(r#"UPDATE "GameChallenges" SET shared_container_id=$1 WHERE id=14"#)
            .bind(shared)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            authorize_target_on(&pool, 1, Some(&shared.to_string()))
                .await
                .unwrap(),
            Some(ScopedExecTarget::Docker("shared-1".into()))
        );

        let inspector = Uuid::new_v4();
        insert_container(&pool, inspector, "inspector-1", None, None, Some(41)).await;
        assert_eq!(
            authorize_target_on(&pool, 1, Some(&inspector.to_string()))
                .await
                .unwrap(),
            Some(ScopedExecTarget::Docker("inspector-1".into()))
        );

        let exercise = Uuid::new_v4();
        insert_container(&pool, exercise, "exercise", None, Some(9), None).await;
        assert_eq!(
            authorize_target_on(&pool, 1, Some(&exercise.to_string()))
                .await
                .unwrap(),
            None
        );

        let mixed = Uuid::new_v4();
        insert_container(&pool, mixed, "mixed", Some(31), None, None).await;
        sqlx::query(r#"UPDATE "GameChallenges" SET shared_container_id=$1 WHERE id=13"#)
            .bind(mixed)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            authorize_target_on(&pool, 1, Some(&mixed.to_string()))
                .await
                .unwrap(),
            None
        );

        assert_eq!(
            authorize_target_on(&pool, 1, Some("ad-game-1"))
                .await
                .unwrap(),
            Some(ScopedExecTarget::Docker("ad-game-1".into()))
        );
        assert_eq!(
            authorize_target_on(&pool, 1, Some("ad-game-2"))
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            authorize_target_on(&pool, 1, Some("koth-game-1"))
                .await
                .unwrap(),
            Some(ScopedExecTarget::Docker("koth-game-1".into()))
        );

        assert_eq!(
            authorize_target_on(&pool, 1, Some("byoc:11:13"))
                .await
                .unwrap(),
            Some(ScopedExecTarget::Byoc {
                participation_id: 11,
                challenge_id: 13,
            })
        );
        for denied in ["byoc:21:13", "byoc:11:23", "byoc:11:13:1", "foreign"] {
            assert_eq!(
                authorize_target_on(&pool, 1, Some(denied)).await.unwrap(),
                None,
                "{denied}"
            );
        }

        sqlx::query(r#"INSERT INTO "KothTargets" VALUES (52,2,24,'ad-game-1')"#)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            authorize_target_on(&pool, 1, Some("ad-game-1"))
                .await
                .unwrap(),
            None
        );

        cleanup(admin, pool, schema).await;
    }
}
