//! Live authorization snapshot shared by proxy opens and established leases.

use uuid::Uuid;

use crate::utils::enums::{ChallengeReviewStatus, ParticipationStatus, Role};

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
     WHERE game.id = $2
       AND game.deletion_pending = FALSE
       AND participation.status = $5
       AND team.deletion_pending = FALSE
       AND account.role <> $6
       AND challenge.is_enabled = TRUE
       AND challenge.deletion_pending = FALSE
       AND challenge.review_status = $7
)"#;

/// Fail closed if any mutable owner of a player proxy is being removed or is
/// no longer eligible. Event time is deliberately absent: finished games may
/// expose their containers for practice until an organizer disables them.
pub(super) async fn game_proxy_scope_is_valid(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
) -> bool {
    sqlx::query_scalar::<_, bool>(GAME_PROXY_SCOPE_SQL)
        .bind(user_id)
        .bind(game_id)
        .bind(participation_id)
        .bind(challenge_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(Role::Banned as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .fetch_one(pool)
        .await
        .unwrap_or(false)
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
        ] {
            assert!(GAME_PROXY_SCOPE_SQL.contains(gate), "missing gate: {gate}");
        }
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
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, role SMALLINT NOT NULL);
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, status SMALLINT NOT NULL
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL, game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              is_enabled BOOLEAN NOT NULL, deletion_pending BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let user_id = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Games" VALUES (1, FALSE)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" VALUES (2, FALSE)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, $2)"#)
            .bind(user_id)
            .bind(Role::User as i16)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" VALUES (3, 1, 2, $1)"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "UserParticipations" VALUES ($1, 1, 3)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "GameChallenges" VALUES (4, 1, TRUE, FALSE, $1)"#)
            .bind(ChallengeReviewStatus::Active as i16)
            .execute(&pool)
            .await
            .unwrap();
        assert!(game_proxy_scope_is_valid(&pool, user_id, 1, 3, 4).await);

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
            assert!(!game_proxy_scope_is_valid(&pool, user_id, 1, 3, 4).await);
            sqlx::query(restore).execute(&pool).await.unwrap();
            assert!(game_proxy_scope_is_valid(&pool, user_id, 1, 3, 4).await);
        }

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
