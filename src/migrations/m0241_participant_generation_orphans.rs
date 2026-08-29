use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE OR REPLACE FUNCTION bump_participant_detail_generation(target_game INTEGER)
RETURNS VOID LANGUAGE SQL AS $$
  INSERT INTO "ParticipantDetailGenerations" (game_id, generation)
  SELECT game.id, 2
    FROM "Games" game
   WHERE game.id = target_game
  ON CONFLICT (game_id) DO UPDATE
     SET generation = "ParticipantDetailGenerations".generation + 1;
$$;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Production migrations are forward-only. Keeping the guarded helper
        // is safe for older binaries and avoids restoring the profile failure.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn generation_bump_is_guarded_by_the_authoritative_game_row() {
        assert!(UP_SQL.contains("SELECT game.id, 2"));
        assert!(UP_SQL.contains("FROM \"Games\" game"));
        assert!(UP_SQL.contains("WHERE game.id = target_game"));
        assert!(UP_SQL.contains("ON CONFLICT (game_id) DO UPDATE"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn stale_participation_cannot_abort_account_activity_updates() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!(
            "participant_generation_orphans_{}",
            uuid::Uuid::new_v4().simple()
        );
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
            CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
            CREATE TABLE "ParticipantDetailGenerations" (
                game_id INTEGER PRIMARY KEY REFERENCES "Games"(id) ON DELETE CASCADE,
                generation BIGINT NOT NULL DEFAULT 1
            );
            CREATE TABLE "AspNetUsers" (
                id UUID PRIMARY KEY,
                last_visited_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, captain_id UUID NOT NULL);
            CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
            -- This intentionally mirrors the legacy table: game_id had no direct
            -- Games foreign key, so upgraded databases can contain stale rows.
            CREATE TABLE "Participations" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL);

            CREATE FUNCTION bump_participant_detail_generation(target_game INTEGER)
            RETURNS VOID LANGUAGE SQL AS $$
              INSERT INTO "ParticipantDetailGenerations" (game_id, generation)
              VALUES (target_game, 2)
              ON CONFLICT (game_id) DO UPDATE
                 SET generation = "ParticipantDetailGenerations".generation + 1;
            $$;

            CREATE FUNCTION bump_participant_detail_from_account()
            RETURNS TRIGGER LANGUAGE plpgsql AS $$
            DECLARE event_id INTEGER;
            BEGIN
              FOR event_id IN
                SELECT DISTINCT participation.game_id
                  FROM "Participations" participation
                  JOIN "Teams" team ON team.id = participation.team_id
                 WHERE team.captain_id = COALESCE(NEW.id, OLD.id)
                    OR EXISTS (
                         SELECT 1 FROM "TeamMembers" member
                          WHERE member.team_id = team.id
                            AND member.user_id = COALESCE(NEW.id, OLD.id)
                       )
              LOOP
                PERFORM bump_participant_detail_generation(event_id);
              END LOOP;
              RETURN COALESCE(NEW, OLD);
            END;
            $$;

            CREATE TRIGGER tr_participant_detail_account
            AFTER UPDATE ON "AspNetUsers"
            FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_account();
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let user_id = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Games" VALUES (7)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "ParticipantDetailGenerations" VALUES (7, 1)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers" VALUES ($1, clock_timestamp() - interval '1 hour')"#,
        )
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" VALUES (11, $1)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" VALUES (70, 7, 11), (99, 999, 11)"#)
            .execute(&pool)
            .await
            .unwrap();

        let previous_error = sqlx::query(
            r#"UPDATE "AspNetUsers" SET last_visited_utc = clock_timestamp() WHERE id = $1"#,
        )
        .bind(user_id)
        .execute(&pool)
        .await
        .expect_err("the unguarded helper unexpectedly accepted an orphan game");
        let previous_code = previous_error
            .as_database_error()
            .and_then(|error| error.code())
            .map(|code| code.into_owned());
        assert_eq!(previous_code.as_deref(), Some("23503"));

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET last_visited_utc = clock_timestamp() WHERE id = $1"#,
        )
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("a stale participation aborted the account activity update");

        let generations: Vec<(i32, i64)> = sqlx::query_as(
            r#"SELECT game_id, generation FROM "ParticipantDetailGenerations" ORDER BY game_id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(generations, vec![(7, 2)]);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
