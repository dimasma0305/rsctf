use super::*;
use uuid::Uuid;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BuildQuery {
    service: i32,
    commit: String,
    previous_attempt: Option<Uuid>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildStatus {
    available: bool,
    status: Option<String>,
    attempt: Option<Uuid>,
    staged: bool,
}

#[derive(sqlx::FromRow)]
struct CurrentBuild {
    status: Option<String>,
    attempt: Option<Uuid>,
    profile: Option<String>,
    staged: bool,
}

pub async fn build(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge, revision)): Path<(i32, i32, i32)>,
    method: axum::http::Method,
    axum::extract::Query(query): axum::extract::Query<BuildQuery>,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let (mut roster, service, starter) = lock_source(st.pg(), who, challenge).await?;
    starter.ok_or_else(missing)?;
    if service != query.service {
        return Err(AppError::not_found("Private revision not found"));
    }
    let db = &mut **roster.transaction_mut();
    let mutate = method == axum::http::Method::POST;
    // Serialize admission across teammates and distinct revisions without upgrading
    // the shared roster lock (two concurrent lock upgrades can deadlock).
    if mutate {
        sqlx::query("SELECT pg_advisory_xact_lock(1734271012, $1)")
            .bind(service)
            .execute(&mut *db)
            .await
            .map_err(super::revisions::db_error)?;
    }
    let CurrentBuild { mut status, mut attempt, profile, mut staged } = sqlx::query_as::<_, CurrentBuild>(r#"
        SELECT b.status,b.attempt,(SELECT profile FROM "AdPatchOnlineBuilders" WHERE challenge_id=$4) AS profile,
               (c.registry_image_ref IS NOT NULL AND b.status='Ready') AS staged
        FROM "AdPatchRevisions" r LEFT JOIN "AdPatchBuilds" b USING(service_id,revision)
        LEFT JOIN "AdPatchCandidates" c ON c.service_id=r.service_id AND c.revision=r.revision
        WHERE r.service_id=$1 AND r.revision=$2 AND r.commit_id=$3"#)
        .bind(service).bind(revision).bind(query.commit).bind(challenge)
        .fetch_optional(&mut *db).await.map_err(super::revisions::db_error)?
        .ok_or_else(|| AppError::not_found("Private revision not found"))?;
    let retry = matches!(status.as_deref(), Some("Failed" | "Replaced"))
        && query.previous_attempt == attempt;
    if mutate && (status.is_none() || retry) {
        let profile = profile
            .as_ref()
            .ok_or_else(|| AppError::unavailable("Patch builder is offline"))?;
        let new = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AdPatchBuilds"(service_id,revision,attempt,status,profile)
          VALUES($1,$2,$3,'Queued',$4) ON CONFLICT(service_id,revision) DO UPDATE
          SET attempt=excluded.attempt,status='Queued',profile=excluded.profile,claim=NULL,claimed_by=NULL,queued_at=clock_timestamp()
          WHERE "AdPatchBuilds".status IN ('Failed','Replaced') AND "AdPatchBuilds".attempt=$5"#)
          .bind(service).bind(revision).bind(new).bind(profile).bind(attempt)
          .execute(&mut *db).await.map_err(|e| if crate::utils::error::is_unique_violation(&e) {
              AppError::conflict("Another revision is building for this service")
          } else { super::revisions::db_error(e) })?;
        status = Some("Queued".into());
        attempt = Some(new);
        staged = false;
    }
    roster.release().await?;
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(BuildStatus {
            available: profile.is_some(),
            status,
            attempt,
            staged,
        }),
    )
        .into_response())
}
