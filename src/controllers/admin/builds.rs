//! Challenge build history / image pipeline endpoints.

use super::*;

mod images;
pub use images::{build_images, delete_build_image, prune_images};

/// RSCTF `PruneResultModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneResultModel {
    pub removed: i32,
    pub messages: Vec<String>,
}

// ─── Challenge build history / pipeline ─────────────────────────────────────
//
// Backed by the `BuildRecords` table (`build_record`). `run_challenge_build`
// in `edit.rs` writes one row per build attempt, so these endpoints surface the
// real history + live in-progress strip the admin Builds page polls.

/// RSCTF `ChallengeBuildAuditModel` — one row of the Builds history table.
/// `durationMs` / `errorMessage` are derived from the persisted timestamps and
/// log tail; every other field maps straight off the `BuildRecords` row.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeBuildAuditModel {
    pub id: i32,
    pub challenge_id: i32,
    pub game_id: i32,
    pub challenge_title: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub enqueued_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub started_at_utc: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub finished_at_utc: Option<DateTime<Utc>>,
    /// BuildTrigger: "Import" | "Manual" | "AutoRetry" | "Bulk".
    pub trigger: String,
    /// ChallengeBuildKind: "Challenge" | "Checker".
    pub kind: String,
    pub attempt: i32,
    pub status: ChallengeBuildStatus,
    pub digest: Option<String>,
    pub image_ref: Option<String>,
    pub log_tail: Option<String>,
    pub error_message: Option<String>,
    pub duration_ms: i64,
}

impl From<build_record::Model> for ChallengeBuildAuditModel {
    fn from(b: build_record::Model) -> Self {
        // Wall-clock build time, when both endpoints are recorded.
        let duration_ms = match (b.started_at_utc, b.finished_at_utc) {
            (Some(s), Some(f)) => (f - s).num_milliseconds().max(0),
            _ => 0,
        };
        // On a failed build, surface the last meaningful log line as the error;
        // the full log stays in `log_tail` (shown in the log modal).
        let error_message = matches!(
            b.status,
            ChallengeBuildStatus::Failed | ChallengeBuildStatus::MissingDockerfile
        )
        .then(|| {
            b.log_tail
                .as_deref()
                .and_then(|log| log.lines().rev().map(str::trim).find(|l| !l.is_empty()))
                .map(|l| l.chars().take(300).collect::<String>())
        })
        .flatten();

        Self {
            id: b.id,
            challenge_id: b.challenge_id,
            game_id: b.game_id,
            challenge_title: b.challenge_title,
            enqueued_at_utc: b.enqueued_at_utc,
            started_at_utc: b.started_at_utc,
            finished_at_utc: b.finished_at_utc,
            trigger: b.trigger,
            kind: b.kind,
            attempt: b.attempt,
            status: b.status,
            digest: b.digest,
            image_ref: b.image_ref,
            log_tail: b.log_tail,
            error_message,
            duration_ms,
        }
    }
}

/// RSCTF `ChallengeBuildInProgressModel` — one row of the live in-progress strip.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeBuildInProgressModel {
    pub audit_id: i32,
    pub challenge_id: i32,
    pub game_id: i32,
    pub slug: String,
    pub attempt: i32,
    pub trigger: String,
    pub kind: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub started_at_utc: DateTime<Utc>,
}

impl From<build_record::Model> for ChallengeBuildInProgressModel {
    fn from(b: build_record::Model) -> Self {
        Self {
            audit_id: b.id,
            challenge_id: b.challenge_id,
            game_id: b.game_id,
            slug: b.challenge_title,
            attempt: b.attempt,
            trigger: b.trigger,
            kind: b.kind,
            // A queued row may not have started yet — fall back to the enqueue time.
            started_at_utc: b.started_at_utc.unwrap_or(b.enqueued_at_utc),
        }
    }
}

/// RSCTF `BuildImageModel` — one `rsctf/*` image on the local docker daemon.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildImageModel {
    pub id: String,
    pub tags: Vec<String>,
    pub size_bytes: i64,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub created_utc: Option<DateTime<Utc>>,
    pub referenced: bool,
    pub referenced_by: Vec<String>,
    pub is_checker: bool,
}

/// `GET /api/admin/builds` query (`?count=&skip=&status=&gameId=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildsListQuery {
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
    #[serde(default)]
    pub status: Option<ChallengeBuildStatus>,
    #[serde(default)]
    pub game_id: Option<i32>,
}

/// `DELETE /api/admin/builds/images` query (`?tag=&force=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteImageQuery {
    pub tag: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct BulkRebuildCandidate {
    challenge_id: i32,
}

fn bulk_rebuild_lock_key(game_id: i32) -> String {
    format!("admin-bulk-build:{game_id}")
}

/// Snapshot eligible work only after the caller owns the session-level batch
/// lease. No row is preclaimed: cancellation leaves every unstarted challenge
/// in its original retryable state.
async fn bulk_rebuild_candidates(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<Vec<BulkRebuildCandidate>> {
    let deletion_pending =
        sqlx::query_scalar::<_, bool>(r#"SELECT deletion_pending FROM "Games" WHERE id = $1"#)
            .bind(game_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    match deletion_pending {
        None => return Err(AppError::not_found("Game not found")),
        Some(true) => return Err(AppError::conflict("Game is being deleted")),
        Some(false) => {}
    }

    let candidates = sqlx::query_as::<_, BulkRebuildCandidate>(
        r#"SELECT challenge.id AS challenge_id
             FROM "GameChallenges" challenge
            WHERE challenge.game_id = $1
              AND challenge.deletion_pending = FALSE
              AND challenge.build_status IN ($2, $3)
            ORDER BY challenge.id"#,
    )
    .bind(game_id)
    .bind(ChallengeBuildStatus::Failed as i16)
    .bind(ChallengeBuildStatus::MissingDockerfile as i16)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(candidates)
}

/// `POST /api/admin/games/{gameId}/bulkrebuild` — retry every failed or
/// missing-Dockerfile build in the game through the same coordinated build
/// seam as an interactive rebuild. Challenges already fenced for deletion are
/// reported as skipped and never reach Docker.
pub async fn bulk_rebuild(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<BulkRebuildResultModel>> {
    let mut batch_lock = crate::utils::single_flight::PgSessionAdvisoryLock::acquire_build_batch(
        st.pg(),
        &bulk_rebuild_lock_key(game_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let candidates = bulk_rebuild_candidates(batch_lock.connection_mut(), game_id).await?;
    let mut enqueued = 0;
    let mut skipped = 0;
    let mut messages = Vec::new();

    for candidate in candidates {
        let eligible = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS (
                   SELECT 1
                     FROM "GameChallenges" challenge
                     JOIN "Games" game ON game.id = challenge.game_id
                    WHERE challenge.id = $1
                      AND challenge.game_id = $2
                      AND challenge.deletion_pending = FALSE
                      AND game.deletion_pending = FALSE
                      AND challenge.build_status IN ($3, $4)
               )"#,
        )
        .bind(candidate.challenge_id)
        .bind(game_id)
        .bind(ChallengeBuildStatus::Failed as i16)
        .bind(ChallengeBuildStatus::MissingDockerfile as i16)
        .fetch_one(batch_lock.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !eligible {
            skipped += 1;
            messages.push(format!(
                "challenge #{}: skipped because it changed, completed, or entered deletion after the batch snapshot",
                candidate.challenge_id
            ));
            continue;
        }

        // The existing build seam accepts the canonical entity model. Loading
        // it here avoids duplicating its large, evolving schema in a raw SQL
        // projection; eligibility and attempt calculation remain raw sqlx.
        let Some(challenge) = game_challenge::Entity::find_by_id(candidate.challenge_id)
            .one(&st.db)
            .await?
            .filter(|challenge| {
                challenge.game_id == game_id
                    && matches!(
                        challenge.build_status,
                        ChallengeBuildStatus::Failed | ChallengeBuildStatus::MissingDockerfile
                    )
            })
        else {
            skipped += 1;
            messages.push(format!(
                "challenge #{}: skipped because it changed, completed, or entered deletion after the batch snapshot",
                candidate.challenge_id
            ));
            continue;
        };

        let attempt = sqlx::query_scalar::<_, i32>(
            r#"SELECT COALESCE(MAX(attempt), 0) + 1
                 FROM "BuildRecords"
                WHERE challenge_id = $1"#,
        )
        .bind(challenge.id)
        .fetch_one(batch_lock.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

        enqueued += 1;
        let (outcome, _record) =
            crate::controllers::edit::run_challenge_build(&st, &challenge, "Bulk", attempt).await;
        let detail = outcome
            .log
            .unwrap_or_else(|| "build returned no detail".to_string());
        messages.push(format!(
            "challenge #{} ({}): {:?}: {}",
            challenge.id, challenge.title, outcome.status, detail
        ));
    }

    if enqueued == 0 && skipped == 0 {
        messages.push("No failed or missing-Dockerfile builds require rebuilding.".to_string());
    }

    batch_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(RequestResponse::ok(BulkRebuildResultModel {
        enqueued,
        skipped,
        messages,
    }))
}

/// `GET /api/admin/builds` — paginated build history, newest first. Optional
/// `status` / `gameId` filters mirror the generated client; the page ships the
/// raw `ChallengeBuildAuditModel[]` (the UI filters/paginates in-memory).
pub async fn list_builds(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<BuildsListQuery>,
) -> AppResult<RequestResponse<Vec<ChallengeBuildAuditModel>>> {
    let count = q.count.clamp(1, 500);
    let mut base = build_record::Entity::find();
    if let Some(status) = q.status {
        base = base.filter(build_record::Column::Status.eq(status));
    }
    if let Some(game_id) = q.game_id {
        base = base.filter(build_record::Column::GameId.eq(game_id));
    }
    let rows = base
        .order_by_desc(build_record::Column::EnqueuedAtUtc)
        .order_by_desc(build_record::Column::Id)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;
    Ok(RequestResponse::ok(
        rows.into_iter().map(Into::into).collect(),
    ))
}

/// `GET /api/admin/builds/inprogress` — builds still pending/running
/// (`Queued` / `Building`), newest first.
pub async fn builds_in_progress(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<Vec<ChallengeBuildInProgressModel>>> {
    let rows = build_record::Entity::find()
        .filter(
            Condition::any()
                .add(build_record::Column::Status.eq(ChallengeBuildStatus::Building))
                .add(build_record::Column::Status.eq(ChallengeBuildStatus::Queued)),
        )
        .order_by_desc(build_record::Column::EnqueuedAtUtc)
        .order_by_desc(build_record::Column::Id)
        .all(&st.db)
        .await?;
    Ok(RequestResponse::ok(
        rows.into_iter().map(Into::into).collect(),
    ))
}

/// `DELETE /api/admin/builds/{auditId}` — drop a single audit row.
pub async fn delete_build(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(audit_id): Path<i32>,
) -> AppResult<MessageResponse> {
    build_record::Entity::delete_by_id(audit_id)
        .exec(&st.db)
        .await?;
    Ok(MessageResponse::ok(""))
}

/// `POST /api/admin/builds/prunefailed` — drop every `Failed` audit row.
pub async fn prune_failed_builds(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<PruneResultModel>> {
    let res = build_record::Entity::delete_many()
        .filter(build_record::Column::Status.eq(ChallengeBuildStatus::Failed))
        .exec(&st.db)
        .await?;
    Ok(RequestResponse::ok(PruneResultModel {
        removed: res.rows_affected as i32,
        messages: Vec::new(),
    }))
}

/// `POST /api/admin/builds/bulkdelete` — drop an explicit list of audit-row ids.
pub async fn bulk_delete_builds(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Json(ids): Json<Vec<i32>>,
) -> AppResult<RequestResponse<PruneResultModel>> {
    if ids.is_empty() {
        return Ok(RequestResponse::ok(PruneResultModel {
            removed: 0,
            messages: Vec::new(),
        }));
    }
    let res = build_record::Entity::delete_many()
        .filter(build_record::Column::Id.is_in(ids))
        .exec(&st.db)
        .await?;
    Ok(RequestResponse::ok(PruneResultModel {
        removed: res.rows_affected as i32,
        messages: Vec::new(),
    }))
}

/// `POST /api/admin/builds/{auditId}/reenqueue` — re-run the build for the
/// challenge owning this audit row (same seam as an interactive rebuild, tagged
/// `AutoRetry`) and echo back the freshly-recorded audit row. Missing/fenced
/// challenges and failed audit persistence are reported truthfully; no queued
/// row is fabricated because this binary has no background queue consumer.
pub async fn reenqueue_build(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(audit_id): Path<i32>,
) -> AppResult<RequestResponse<ChallengeBuildAuditModel>> {
    let record = build_record::Entity::find_by_id(audit_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Build record not found"))?;

    let attempt = record.attempt + 1;

    let challenge = game_challenge::Entity::find_by_id(record.challenge_id)
        .one(&st.db)
        .await?
        .ok_or_else(reenqueue_missing_challenge_error)?;
    let model = crate::controllers::edit::admin_reenqueue_build(&st, &challenge, attempt)
        .await?
        .ok_or_else(reenqueue_missing_audit_error)?;

    Ok(RequestResponse::ok(model.into()))
}

fn reenqueue_missing_challenge_error() -> AppError {
    AppError::not_found("The challenge for this build record no longer exists")
}

fn reenqueue_missing_audit_error() -> AppError {
    AppError::conflict(
        "The synchronous rebuild returned without a durable audit row; no queued work was created",
    )
}

#[cfg(test)]
mod bulk_rebuild_tests {
    use super::*;

    #[test]
    fn reenqueue_never_fabricates_unconsumable_queued_work() {
        assert!(matches!(
            reenqueue_missing_challenge_error(),
            AppError::NotFound(_)
        ));
        assert!(matches!(
            reenqueue_missing_audit_error(),
            AppError::Conflict(_)
        ));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn bulk_lease_serializes_requests_and_cancellation_strands_no_rows() {
        use std::str::FromStr;

        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("admin_bulk_build_{}", Uuid::new_v4().simple());
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
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              build_status SMALLINT NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "BuildRecords" (
              id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
              challenge_id INTEGER NOT NULL,
              attempt INTEGER NOT NULL
            );
            INSERT INTO "Games" (id, deletion_pending) VALUES (1, FALSE), (2, TRUE);
            INSERT INTO "GameChallenges" (id, game_id, build_status, deletion_pending) VALUES
              (10, 1, 2, FALSE),
              (11, 1, 6, FALSE),
              (12, 1, 1, FALSE),
              (13, 1, 2, TRUE);
            INSERT INTO "BuildRecords" (challenge_id, attempt) VALUES
              (10, 1), (10, 2), (11, 4);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut connection = pool.acquire().await.unwrap();
        let candidates = bulk_rebuild_candidates(&mut connection, 1).await.unwrap();
        assert_eq!(
            candidates,
            vec![
                BulkRebuildCandidate { challenge_id: 10 },
                BulkRebuildCandidate { challenge_id: 11 },
            ]
        );
        assert!(matches!(
            bulk_rebuild_candidates(&mut connection, 999).await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            bulk_rebuild_candidates(&mut connection, 2).await,
            Err(AppError::Conflict(_))
        ));
        drop(connection);

        let acquired = std::sync::Arc::new(tokio::sync::Notify::new());
        let owner = tokio::spawn({
            let pool = pool.clone();
            let acquired = acquired.clone();
            async move {
                let _lock =
                    crate::utils::single_flight::PgSessionAdvisoryLock::acquire_build_batch(
                        &pool,
                        &bulk_rebuild_lock_key(1),
                    )
                    .await
                    .unwrap();
                acquired.notify_one();
                std::future::pending::<()>().await;
            }
        });
        acquired.notified().await;
        let mut waiter = tokio::spawn({
            let pool = pool.clone();
            async move {
                crate::utils::single_flight::PgSessionAdvisoryLock::acquire_build_batch(
                    &pool,
                    &bulk_rebuild_lock_key(1),
                )
                .await
            }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut waiter)
                .await
                .is_err()
        );
        owner.abort();
        let _ = owner.await;
        let second_lock = tokio::time::timeout(std::time::Duration::from_secs(2), &mut waiter)
            .await
            .expect("cancelled owner must release the batch lease")
            .unwrap()
            .unwrap();
        second_lock.release().await.unwrap();

        assert_eq!(
            sqlx::query_as::<_, (i32, i16)>(
                r#"SELECT id, build_status FROM "GameChallenges" ORDER BY id"#,
            )
            .fetch_all(&pool)
            .await
            .unwrap(),
            vec![
                (10, ChallengeBuildStatus::Failed as i16),
                (11, ChallengeBuildStatus::MissingDockerfile as i16),
                (12, ChallengeBuildStatus::Success as i16),
                (13, ChallengeBuildStatus::Failed as i16),
            ]
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
