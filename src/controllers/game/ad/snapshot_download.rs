//! Final authorization boundary for player A&D snapshot downloads.

use crate::services::live_roster::LiveParticipationIdentity;
use crate::utils::error::{AppError, AppResult};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::ops::Range;
use std::sync::Arc;

pub(crate) struct SnapshotResponseGrant {
    pub(crate) team_service_id: i32,
    pub(crate) snapshot_id: i64,
    pub(crate) hash: String,
    pub(crate) filename: String,
    pub(crate) file_size: i64,
}

pub(crate) enum SnapshotPreparation {
    Ready(PreparedSnapshot),
    Response(Response),
}

pub(crate) struct PreparedSnapshot {
    stream: crate::storage::BlobByteStream,
    permit: Arc<crate::services::bulk_export::BulkExportPermit>,
    size: u64,
    range: Range<u64>,
    partial: bool,
    etag: String,
}

fn parse_byte_range(value: &str, size: u64) -> Result<Range<u64>, ()> {
    let value = value.strip_prefix("bytes=").ok_or(())?;
    if value.is_empty() || value.contains(',') || size == 0 {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        return Ok(size.saturating_sub(suffix)..size);
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= size {
        return Err(());
    }
    let end = if end.is_empty() {
        size
    } else {
        let inclusive = end.parse::<u64>().map_err(|_| ())?;
        if inclusive < start {
            return Err(());
        }
        inclusive.saturating_add(1).min(size)
    };
    Ok(start..end)
}

fn range_not_satisfiable(size: u64, etag: &str) -> Response {
    let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response.headers_mut().insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{size}")).expect("valid content range"),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("snapshot hash is a valid ETag"),
    );
    response
}

/// Admit before opening storage and stream only the selected immutable range.
pub(crate) async fn prepare_snapshot_stream(
    st: &crate::app_state::SharedState,
    headers: &HeaderMap,
    grant: &SnapshotResponseGrant,
) -> AppResult<SnapshotPreparation> {
    let size = u64::try_from(grant.file_size)
        .map_err(|_| AppError::not_found("Snapshot has an invalid stored size"))?;
    if size > crate::services::ad::snapshots::MAX_STORED_SNAPSHOT_BYTES as u64 {
        return Err(AppError::payload_too_large("Snapshot exceeds 128 MiB"));
    }
    let etag = format!("\"{}\"", grant.hash);
    let requested_range = if headers
        .get(header::IF_RANGE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|validator| validator != etag)
    {
        None
    } else {
        match headers.get(header::RANGE) {
            Some(value) => match value
                .to_str()
                .map_err(|_| ())
                .and_then(|value| parse_byte_range(value, size))
            {
                Ok(range) => Some(range),
                Err(()) => {
                    return Ok(SnapshotPreparation::Response(range_not_satisfiable(
                        size, &etag,
                    )))
                }
            },
            None => None,
        }
    };
    let permit = match st
        .bulk_export_admission
        .try_acquire(
            Arc::clone(&st.cache),
            usize::try_from(size).unwrap_or(usize::MAX),
        )
        .await
    {
        Ok(permit) => Arc::new(permit),
        Err(_) => {
            return Ok(SnapshotPreparation::Response(
                crate::services::bulk_export::overload_response(),
            ))
        }
    };
    let range = requested_range.clone().unwrap_or(0..size);
    let stream = st.storage.stream_range(&grant.hash, range.clone()).await?;
    Ok(SnapshotPreparation::Ready(PreparedSnapshot {
        stream,
        permit,
        size,
        range,
        partial: requested_range.is_some(),
        etag,
    }))
}

impl PreparedSnapshot {
    pub(crate) fn into_response(self, filename: &str) -> AppResult<Response> {
        let length = self.range.end - self.range.start;
        let mut response = Response::new(crate::services::bulk_export::permitted_stream_body(
            self.stream,
            self.permit,
        ));
        *response.status_mut() = if self.partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(crate::services::ad::snapshots::SNAPSHOT_CONTENT_TYPE),
        );
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).expect("u64 content length is ASCII"),
        );
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        headers.insert(
            header::ETAG,
            HeaderValue::from_str(&self.etag).expect("snapshot hash is a valid ETag"),
        );
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&crate::utils::content_disposition::attachment(filename))
                .map_err(|_| AppError::bad_request("Invalid snapshot filename"))?,
        );
        if self.partial {
            headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!(
                    "bytes {}-{}/{}",
                    self.range.start,
                    self.range.end - 1,
                    self.size
                ))
                .expect("valid snapshot content range"),
            );
        }
        Ok(response)
    }
}

async fn authorize_snapshot_response(
    pool: &sqlx::PgPool,
    caller: LiveParticipationIdentity<'_>,
    grant: &SnapshotResponseGrant,
) -> AppResult<()> {
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
    .bind(grant.file_size)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(false);
    if !still_available {
        roster.release().await?;
        return Err(AppError::not_found("Snapshot is no longer available"));
    }
    roster.release().await?;
    Ok(())
}

pub(crate) async fn finish_snapshot_response(
    pool: &sqlx::PgPool,
    caller: LiveParticipationIdentity<'_>,
    grant: SnapshotResponseGrant,
    prepared: PreparedSnapshot,
) -> AppResult<Response> {
    authorize_snapshot_response(pool, caller, &grant).await?;
    prepared.into_response(&grant.filename)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn retained_snapshot_ranges_are_single_and_bounded() {
        assert_eq!(parse_byte_range("bytes=10-19", 100), Ok(10..20));
        assert_eq!(parse_byte_range("bytes=90-", 100), Ok(90..100));
        assert_eq!(parse_byte_range("bytes=-10", 100), Ok(90..100));
        assert!(parse_byte_range("bytes=10-9", 100).is_err());
        assert!(parse_byte_range("bytes=0-1,4-5", 100).is_err());
        assert!(parse_byte_range("bytes=100-", 100).is_err());
    }

    #[test]
    fn retained_snapshot_admission_precedes_storage_open() {
        let source = include_str!("snapshot_download.rs");
        let handler = source
            .find("pub(crate) async fn prepare_snapshot_stream(")
            .unwrap();
        let body = &source[handler..];
        assert!(body.find("bulk_export_admission").unwrap() < body.find("stream_range").unwrap());
    }

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
            file_size: 3,
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
            authorize_snapshot_response(&pool, caller, &grant()),
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
            authorize_snapshot_response(&pool, caller, &grant()).await,
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
            authorize_snapshot_response(&pool, caller, &grant()).await,
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
            authorize_snapshot_response(&pool, caller, &grant()).await,
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
            authorize_snapshot_response(&pool, caller, &grant()).await,
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
