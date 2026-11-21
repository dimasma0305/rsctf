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
    #[serde(flatten)]
    build: CurrentBuild,
}

#[derive(sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentBuild {
    status: Option<String>,
    attempt: Option<Uuid>,
    #[serde(skip)]
    profile: Option<String>,
    #[serde(skip)]
    runtime_profile: Option<String>,
    staged: bool,
    can_activate: bool,
    active: bool,
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
    let mut build = sqlx::query_as::<_, CurrentBuild>(r#"
        SELECT b.status,b.attempt,(SELECT profile FROM "AdPatchOnlineBuilders" WHERE challenge_id=$4) AS profile,
               ch.runtime_profile,
               (c.registry_image_ref IS NOT NULL AND b.status='Ready') AS staged,
               EXISTS(SELECT 1 FROM "AdPatchActivationTargets" t
                      WHERE t.service_id=r.service_id AND t.revision=r.revision AND t.attempt=b.attempt) AS can_activate,
               COALESCE(c.registry_image_ref=s.published_image_ref AND s.published_image_backend=s.container_id
                        AND s.host<>'' AND s.port BETWEEN 1 AND 65535,false) AS active
        FROM "AdPatchRevisions" r LEFT JOIN "AdPatchBuilds" b USING(service_id,revision)
        JOIN "AdTeamServices" s ON s.id=r.service_id
        JOIN "GameChallenges" ch ON ch.id=s.challenge_id
        LEFT JOIN "AdPatchCandidates" c ON c.service_id=r.service_id AND c.revision=r.revision
        WHERE r.service_id=$1 AND r.revision=$2 AND r.commit_id=$3"#)
        .bind(service).bind(revision).bind(query.commit).bind(challenge)
        .fetch_optional(&mut *db).await.map_err(super::revisions::db_error)?
        .ok_or_else(|| AppError::not_found("Private revision not found"))?;
    let retry = matches!(build.status.as_deref(), Some("Failed" | "Replaced"))
        && query.previous_attempt == build.attempt;
    if mutate && (build.status.is_none() || retry) {
        let profile = build
            .profile
            .as_ref()
            .ok_or_else(|| AppError::unavailable("Patch builder is offline"))?;
        let new = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AdPatchBuilds"(service_id,revision,attempt,status,profile)
          VALUES($1,$2,$3,'Queued',$4) ON CONFLICT(service_id,revision) DO UPDATE
          SET attempt=excluded.attempt,status='Queued',profile=excluded.profile,claim=NULL,claimed_by=NULL,queued_at=clock_timestamp()
          WHERE "AdPatchBuilds".status IN ('Failed','Replaced') AND "AdPatchBuilds".attempt=$5"#)
          .bind(service).bind(revision).bind(new).bind(profile).bind(build.attempt)
          .execute(&mut *db).await.map_err(|e| if crate::utils::error::is_unique_violation(&e) {
              AppError::conflict("Another revision is building for this service")
          } else { super::revisions::db_error(e) })?;
        build.status = Some("Queued".into());
        build.attempt = Some(new);
        build.active = false;
    }
    roster.release().await?;
    build.can_activate &= crate::services::ad::reset::supports_patch_activation(
        &st,
        build.runtime_profile.as_deref(),
    );
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(BuildStatus {
            available: build.profile.is_some(),
            build,
        }),
    )
        .into_response())
}
