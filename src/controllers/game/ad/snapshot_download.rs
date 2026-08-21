//! Final authorization boundary for player A&D snapshot downloads.

use crate::services::live_roster::LiveParticipationIdentity;
use crate::utils::error::{AppError, AppResult};
use axum::http::header;
use axum::response::{IntoResponse, Response};

pub(super) struct SnapshotResponseGrant {
    pub(super) team_service_id: i32,
    pub(super) snapshot_id: i64,
    pub(super) hash: String,
    pub(super) filename: String,
}

pub(super) async fn finish_snapshot_response(
    pool: &sqlx::PgPool,
    caller: LiveParticipationIdentity<'_>,
    grant: SnapshotResponseGrant,
    archive: Vec<u8>,
) -> AppResult<Response> {
    // Blob storage may be slow and must never run while a pool connection is
    // retained. Revalidate only after the archive is ready, then build the
    // response under the exact roster/account fence so a completed kick or
    // stamp rotation discards the prepared bytes.
    let mut roster = crate::services::live_roster::try_acquire_participation_fence(
        pool,
        caller.user_id,
        caller.expected_security_stamp,
        caller.game_id,
        caller.team_id,
        caller.participation_id,
        true,
    )
    .await?
    .ok_or(AppError::Forbidden)?;
    // The early phase intentionally performs storage I/O without a database
    // connection. Re-prove that those exact bytes still belong to this caller's
    // hosted service and that the live operator policy still permits a
    // post-game download. The row locks keep a concurrent policy toggle,
    // snapshot expiry/delete, or blob-reference release behind response
    // construction; no pool checkout occurs while they are held.
    let still_available = sqlx::query_scalar::<_, bool>(
        r#"SELECT TRUE
             FROM "AdTeamServices" service
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
             JOIN "Games" game ON game.id = service.game_id
             JOIN "AdServiceSnapshots" snapshot
               ON snapshot.team_service_id = service.id
             JOIN "Files" file ON file.id = snapshot.local_file_id
            WHERE service.id = $1
              AND service.game_id = $2
              AND service.participation_id = $3
              AND snapshot.id = $4
              AND file.hash = $5
              AND file.name = $6
              AND file.file_size = $7
              AND file.reference_count > 0
              AND (snapshot.expires_at_utc IS NULL
                   OR snapshot.expires_at_utc > clock_timestamp())
              AND challenge.ad_self_hosted = FALSE
              AND challenge.deletion_pending = FALSE
              AND game.ad_allow_snapshot_download = TRUE
              AND game.end_time_utc <= clock_timestamp()
            FOR SHARE OF service, challenge, game, snapshot, file"#,
    )
    .bind(grant.team_service_id)
    .bind(caller.game_id)
    .bind(caller.participation_id)
    .bind(grant.snapshot_id)
    .bind(&grant.hash)
    .bind(&grant.filename)
    .bind(i64::try_from(archive.len()).unwrap_or(i64::MAX))
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(false);
    if !still_available {
        roster.release().await?;
        return Err(AppError::not_found("Snapshot is no longer available"));
    }
    let response = (
        [
            (
                header::CONTENT_TYPE,
                crate::services::ad::snapshots::SNAPSHOT_CONTENT_TYPE.to_string(),
            ),
            (header::CONTENT_LENGTH, archive.len().to_string()),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
            (header::PRAGMA, "no-cache".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", grant.filename),
            ),
        ],
        archive,
    )
        .into_response();
    roster.release().await?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn final_snapshot_fence_works_with_one_connection_and_kick_wins() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("snapshot_response_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, deletion_pending BOOLEAN NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL,
              ad_allow_snapshot_download BOOLEAN NOT NULL
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
              deletion_pending BOOLEAN NOT NULL
            );
            CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY, role SMALLINT NOT NULL,
              email_confirmed BOOLEAN NOT NULL, security_stamp TEXT
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, status SMALLINT NOT NULL
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, participation_id INTEGER NOT NULL
            );
            CREATE TABLE "IdentityObservations" (
              user_id UUID NOT NULL, game_id INTEGER,
              team_id INTEGER, participation_id INTEGER,
              observed_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              ad_self_hosted BOOLEAN NOT NULL, deletion_pending BOOLEAN NOT NULL
            );
            CREATE TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL
            );
            CREATE TABLE "Files" (
              id BIGINT PRIMARY KEY, hash TEXT NOT NULL, name TEXT NOT NULL,
              file_size BIGINT NOT NULL, reference_count INTEGER NOT NULL
            );
            CREATE TABLE "AdServiceSnapshots" (
              id BIGINT PRIMARY KEY, team_service_id INTEGER NOT NULL,
              local_file_id BIGINT NOT NULL, expires_at_utc TIMESTAMPTZ
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let user_id = Uuid::new_v4();
        let captain_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "Games" VALUES
               (1, FALSE, clock_timestamp() - interval '2 hours',
                clock_timestamp() - interval '1 hour', TRUE)"#,
        )
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
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, 1, TRUE, 'stamp')"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" VALUES (3, 1, 2, 1)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "UserParticipations" VALUES ($1, 1, 2, 3)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"INSERT INTO "GameChallenges" VALUES (4, 1, FALSE, FALSE);
               INSERT INTO "AdTeamServices" VALUES (5, 1, 3, 4);
               INSERT INTO "Files" VALUES (6, 'snapshot-hash', 'snapshot.tar.zst', 3, 1);
               INSERT INTO "AdServiceSnapshots" VALUES (7, 5, 6, NULL);"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let grant = || SnapshotResponseGrant {
            team_service_id: 5,
            snapshot_id: 7,
            hash: "snapshot-hash".to_string(),
            filename: "snapshot.tar.zst".to_string(),
        };
        let caller = LiveParticipationIdentity {
            user_id,
            expected_security_stamp: "stamp",
            game_id: 1,
            team_id: 2,
            participation_id: 3,
        };

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            finish_snapshot_response(&pool, caller, grant(), vec![1, 2, 3]),
        )
        .await
        .expect("a one-connection pool deadlocked")
        .unwrap();

        // An operator can revoke the live download policy while storage is
        // returning the prepared archive. The final transaction must discard
        // it rather than relying on the early policy read.
        sqlx::query(r#"UPDATE "Games" SET ad_allow_snapshot_download = FALSE WHERE id = 1"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            finish_snapshot_response(&pool, caller, grant(), vec![1, 2, 3]).await,
            Err(AppError::NotFound(_))
        ));
        sqlx::query(r#"UPDATE "Games" SET ad_allow_snapshot_download = TRUE WHERE id = 1"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"UPDATE "GameChallenges" SET ad_self_hosted = TRUE WHERE id = 4"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            finish_snapshot_response(&pool, caller, grant(), vec![1, 2, 3]).await,
            Err(AppError::NotFound(_))
        ));
        sqlx::query(r#"UPDATE "GameChallenges" SET ad_self_hosted = FALSE WHERE id = 4"#)
            .execute(&pool)
            .await
            .unwrap();

        // Represents a kick that commits while slow storage is preparing the
        // archive. The historical participation remains, but the final phase
        // must discard those already-loaded bytes.
        sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = 2 AND user_id = $1"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            finish_snapshot_response(&pool, caller, grant(), vec![4, 5, 6]).await,
            Err(AppError::Forbidden)
        ));

        // A retained caller cannot use bytes loaded from a snapshot relation
        // that was detached before the final fence.
        sqlx::query(r#"INSERT INTO "TeamMembers" VALUES (2, $1)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"DELETE FROM "AdServiceSnapshots" WHERE id = 7"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            finish_snapshot_response(&pool, caller, grant(), vec![4, 5, 6]).await,
            Err(AppError::NotFound(_))
        ));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
