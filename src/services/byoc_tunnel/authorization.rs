//! Read-only authorization leases for established BYOC agent tunnels.

use crate::app_state::SharedState;
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus, Role};

/// One statement deliberately returns only the two secrets needed to verify the
/// bearer. PostgreSQL evaluates every mutable eligibility gate against the same
/// statement snapshot; `LIMIT 1` also keeps the result bounded if constraints in
/// a legacy database are weaker than current migrations.
const LIVE_TUNNEL_AUTHORIZATION_SQL: &str = concat!(
    r#"
    SELECT game.private_key AS game_secret,
           team.invite_token AS team_secret
      FROM "Participations" participation
      JOIN "Games" game
        ON game.id = participation.game_id
      JOIN "Teams" team
        ON team.id = participation.team_id
      JOIN "GameChallenges" challenge
        ON challenge.id = $3
       AND challenge.game_id = game.id
     WHERE participation.id = $2
       AND participation.game_id = $1
       AND participation.status = $4
       AND game.start_time_utc <= statement_timestamp()
       AND statement_timestamp() <= game.end_time_utc
       AND "#,
    crate::services::ad::roster::shared_credential_team_predicate_sql!("team", "$5"),
    r#"
       AND challenge."Type" = $6
       AND challenge.ad_self_hosted = TRUE
       AND challenge.is_enabled = TRUE
       AND challenge.review_status = $7
     LIMIT 1
    "#
);

#[derive(sqlx::FromRow)]
struct AuthorizationSecrets {
    game_secret: String,
    team_secret: String,
}

async fn load_authorization_snapshot(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
) -> Result<Option<AuthorizationSecrets>, sqlx::Error> {
    sqlx::query_as::<_, AuthorizationSecrets>(LIVE_TUNNEL_AUTHORIZATION_SQL)
        .bind(game_id)
        .bind(participation_id)
        .bind(challenge_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(Role::Banned as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .fetch_optional(pool)
        .await
}

async fn live_tunnel_authorized_on(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    token: &str,
) -> bool {
    let Ok(Some(secrets)) =
        load_authorization_snapshot(pool, game_id, participation_id, challenge_id).await
    else {
        return false;
    };
    let expected = crate::controllers::game::ad::byoc_token(
        "adbyocagent:",
        &secrets.game_secret,
        &secrets.team_secret,
        participation_id,
        challenge_id,
    );
    crate::utils::crypto_utils::ct_eq(&expected, token)
}

/// Re-resolve every mutable grant behind an established BYOC tunnel. Mutation
/// handlers disconnect eagerly; this single-round-trip lease is the fail-safe
/// for game-window expiry and database changes that bypass those callbacks.
pub(super) async fn live_tunnel_authorized(
    st: &SharedState,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    token: &str,
) -> bool {
    live_tunnel_authorized_on(st.pg(), game_id, participation_id, challenge_id, token).await
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use chrono::{Duration, Utc};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn snapshot_query_is_single_bounded_and_fail_closed() {
        assert!(!LIVE_TUNNEL_AUTHORIZATION_SQL.contains(';'));
        assert!(LIVE_TUNNEL_AUTHORIZATION_SQL.contains("LIMIT 1"));
        for gate in [
            "participation.game_id = $1",
            "participation.status = $4",
            "game.start_time_utc <= statement_timestamp()",
            "statement_timestamp() <= game.end_time_utc",
            "NOT team.deletion_pending",
            "account.id IS NULL OR account.role = $5",
            "challenge.game_id = game.id",
            "challenge.\"Type\" = $6",
            "challenge.ad_self_hosted = TRUE",
            "challenge.is_enabled = TRUE",
            "challenge.review_status = $7",
        ] {
            assert!(
                LIVE_TUNNEL_AUTHORIZATION_SQL.contains(gate),
                "authorization query lost gate: {gate}"
            );
        }
    }

    async fn test_pool() -> (sqlx::PgPool, sqlx::PgPool, String) {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_byoc_lease_{}", Uuid::new_v4().simple());
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
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, role SMALLINT NOT NULL);
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
              invite_token TEXT NOT NULL, deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, status SMALLINT NOT NULL
            );
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, private_key TEXT NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, "Type" SMALLINT NOT NULL,
              ad_self_hosted BOOLEAN NOT NULL, is_enabled BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        (admin, pool, schema)
    }

    async fn cleanup(admin: sqlx::PgPool, pool: sqlx::PgPool, schema: String) {
        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    fn token(team_secret: &str) -> String {
        crate::controllers::game::ad::byoc_token("adbyocagent:", "game-secret", team_secret, 11, 13)
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn snapshot_enforces_every_mutable_tunnel_grant() {
        let (admin, pool, schema) = test_pool().await;
        let captain = Uuid::new_v4();
        let member = Uuid::new_v4();
        for user_id in [captain, member] {
            sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, $2)"#)
                .bind(user_id)
                .bind(Role::User as i16)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query(r#"INSERT INTO "Teams" VALUES (7, $1, 'team-secret', FALSE)"#)
            .bind(captain)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "TeamMembers" VALUES (7, $1)"#)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        let start = Utc::now() - Duration::hours(1);
        let end = Utc::now() + Duration::hours(1);
        sqlx::query(r#"INSERT INTO "Games" VALUES (3, 'game-secret', $1, $2)"#)
            .bind(start)
            .bind(end)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" VALUES (11, 3, 7, $1)"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "GameChallenges" VALUES (13, 3, $1, TRUE, TRUE, $2)"#)
            .bind(ChallengeType::AttackDefense as i16)
            .bind(ChallengeReviewStatus::Active as i16)
            .execute(&pool)
            .await
            .unwrap();

        let original = token("team-secret");
        assert!(live_tunnel_authorized_on(&pool, 3, 11, 13, &original).await);
        assert!(!live_tunnel_authorized_on(&pool, 3, 11, 13, "wrong").await);
        assert!(!live_tunnel_authorized_on(&pool, 4, 11, 13, &original).await);
        assert!(!live_tunnel_authorized_on(&pool, 3, 12, 13, &original).await);
        assert!(!live_tunnel_authorized_on(&pool, 3, 11, 14, &original).await);

        sqlx::query(r#"UPDATE "Participations" SET status = $1 WHERE id = 11"#)
            .bind(ParticipationStatus::Suspended as i16)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!live_tunnel_authorized_on(&pool, 3, 11, 13, &original).await);
        sqlx::query(r#"UPDATE "Participations" SET status = $1 WHERE id = 11"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(r#"UPDATE "Games" SET end_time_utc = $1 WHERE id = 3"#)
            .bind(Utc::now() - Duration::minutes(1))
            .execute(&pool)
            .await
            .unwrap();
        assert!(!live_tunnel_authorized_on(&pool, 3, 11, 13, &original).await);
        sqlx::query(r#"UPDATE "Games" SET end_time_utc = $1 WHERE id = 3"#)
            .bind(end)
            .execute(&pool)
            .await
            .unwrap();

        // Authorization time is evaluated by PostgreSQL after a connection is
        // acquired. A request queued before the boundary must not carry an old
        // application timestamp across the end of the game.
        sqlx::query(r#"UPDATE "Games" SET end_time_utc = $1 WHERE id = 3"#)
            .bind(Utc::now() + Duration::milliseconds(150))
            .execute(&pool)
            .await
            .unwrap();
        let first_connection = pool.acquire().await.unwrap();
        let second_connection = pool.acquire().await.unwrap();
        let queued_pool = pool.clone();
        let queued_token = original.clone();
        let queued = tokio::spawn(async move {
            live_tunnel_authorized_on(&queued_pool, 3, 11, 13, &queued_token).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        drop(first_connection);
        drop(second_connection);
        assert!(!queued.await.unwrap());
        sqlx::query(r#"UPDATE "Games" SET end_time_utc = $1 WHERE id = 3"#)
            .bind(end)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(r#"UPDATE "Teams" SET deletion_pending = TRUE WHERE id = 7"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!live_tunnel_authorized_on(&pool, 3, 11, 13, &original).await);
        sqlx::query(r#"UPDATE "Teams" SET deletion_pending = FALSE WHERE id = 7"#)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(r#"UPDATE "AspNetUsers" SET role = $1 WHERE id = $2"#)
            .bind(Role::Banned as i16)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!live_tunnel_authorized_on(&pool, 3, 11, 13, &original).await);
        sqlx::query(r#"UPDATE "AspNetUsers" SET role = $1 WHERE id = $2"#)
            .bind(Role::User as i16)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"DELETE FROM "AspNetUsers" WHERE id = $1"#)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!live_tunnel_authorized_on(&pool, 3, 11, 13, &original).await);
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, $2)"#)
            .bind(member)
            .bind(Role::User as i16)
            .execute(&pool)
            .await
            .unwrap();

        for mutation in [
            r#"UPDATE "GameChallenges" SET "Type" = 5 WHERE id = 13"#,
            r#"UPDATE "GameChallenges" SET ad_self_hosted = FALSE WHERE id = 13"#,
            r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 13"#,
            r#"UPDATE "GameChallenges" SET review_status = 1 WHERE id = 13"#,
        ] {
            sqlx::query(mutation).execute(&pool).await.unwrap();
            assert!(!live_tunnel_authorized_on(&pool, 3, 11, 13, &original).await);
            sqlx::query(
                r#"UPDATE "GameChallenges"
                      SET "Type" = $1, ad_self_hosted = TRUE,
                          is_enabled = TRUE, review_status = $2
                    WHERE id = 13"#,
            )
            .bind(ChallengeType::AttackDefense as i16)
            .bind(ChallengeReviewStatus::Active as i16)
            .execute(&pool)
            .await
            .unwrap();
        }

        sqlx::query(r#"UPDATE "Teams" SET invite_token = 'rotated' WHERE id = 7"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!live_tunnel_authorized_on(&pool, 3, 11, 13, &original).await);
        assert!(live_tunnel_authorized_on(&pool, 3, 11, 13, &token("rotated")).await);

        cleanup(admin, pool, schema).await;
    }
}
