use super::*;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Page {
    after_team: Option<i32>,
    after_revision: Option<i32>,
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
struct PublishedRevision {
    team_id: i32,
    team_name: String,
    revision: i32,
    #[sqlx(json)]
    changes: Vec<PatchChange>,
}

pub(crate) async fn archive(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge)): Path<(i32, i32)>,
    axum::extract::Query(page): axum::extract::Query<Page>,
) -> AppResult<Response> {
    let (team, revision) = match (page.after_team, page.after_revision) {
        (None, None) => (0, 0),
        (Some(t), Some(r)) if t > 0 && r > 0 => (t, r),
        _ => return Err(AppError::bad_request("Invalid archive cursor")),
    };
    let who = caller(&st, &user, game).await?;
    let (mut roster, _) = lock_team_service(st.pg(), who, challenge).await?;
    let db = &mut **roster.transaction_mut();
    let open = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM "Games" g
        JOIN "AdPatchActivityPolicies" policy ON policy.game_id=g.id AND policy.enabled
        WHERE g.id=$1 AND NOT g.practice_mode AND g.end_time_utc<=clock_timestamp()
        AND NOT EXISTS (SELECT 1 FROM "AdRounds" r WHERE r.game_id=g.id AND NOT r.finalized))"#,
    )
    .bind(game)
    .fetch_one(&mut *db)
    .await
    .map_err(revisions::db_error)?;
    if !open {
        return Err(AppError::not_found("Patch archive is unavailable"));
    }
    let mut rows = sqlx::query_as::<_, PublishedRevision>(
        r#"SELECT t.id AS team_id,t.name AS team_name,r.revision,r.changes
        FROM "AdTeamServices" s JOIN "AdSharedPatchChanges" r ON r.service_id=s.id AND r.published
        JOIN "Participations" p ON p.id=s.participation_id AND p.game_id=s.game_id
        JOIN "Participations" viewer ON viewer.id=$3 AND viewer.game_id=s.game_id
        JOIN "Teams" t ON t.id=p.team_id AND NOT t.deletion_pending
        WHERE s.game_id=$1 AND s.challenge_id=$2 AND p.status=$4
          AND p.division_id IS NOT DISTINCT FROM viewer.division_id AND (t.id,r.revision)>($5,$6)
        ORDER BY t.id,r.revision LIMIT 21"#,
    )
    .bind(game)
    .bind(challenge)
    .bind(who.participation_id)
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .bind(team)
    .bind(revision)
    .fetch_all(db)
    .await
    .map_err(revisions::db_error)?;
    let more = rows.len() > 20;
    rows.truncate(20);
    roster.release().await?;
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(serde_json::json!({"revisions":rows,"hasMore":more})),
    )
        .into_response())
}

#[derive(Serialize, sqlx::FromRow)]
struct RoundActivity {
    round: i32,
    deployments: i64,
}

pub(crate) async fn list(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let (mut roster, _, starter) = lock_source(st.pg(), who, challenge).await?;
    starter.ok_or_else(missing)?;
    let db = &mut **roster.transaction_mut();
    let since=sqlx::query_scalar::<_,chrono::DateTime<chrono::Utc>>(
        r#"SELECT changed_at FROM "AdPatchActivityPolicies" WHERE game_id=$1 AND enabled FOR SHARE"#,
    ).bind(game).fetch_optional(&mut *db).await.map_err(revisions::db_error)?;
    let activity = if let Some(since) = since {
        // One subsequent completed round must pass. A paused scheduler cannot
        // release activity merely because the wall clock advanced.
        Some(sqlx::query_as::<_,RoundActivity>(r#"
            SELECT r.number AS round,
                (SELECT COUNT(*) FROM "ControlPlaneJobs" j
                 WHERE j.game_id=$1 AND j.challenge_id=$2 AND j.kind='AdActivate' AND j.status=2
                   AND j.result @> '{"reset":true}'::jsonb
                   AND j.created_at_utc >= $3 AND j.finished_at_utc >= r.start_time_utc
                   AND j.finished_at_utc < r.end_time_utc) AS deployments
            FROM (SELECT number,start_time_utc,end_time_utc FROM "AdRounds"
                  WHERE game_id=$1 AND finalized AND start_time_utc >= $3 AND end_time_utc <= clock_timestamp()
                  ORDER BY number DESC LIMIT 12 OFFSET 1) r
            ORDER BY r.number DESC"#)
            .bind(game).bind(challenge).bind(since).fetch_all(db).await.map_err(revisions::db_error)?)
    } else {
        None
    };
    roster.release().await?;
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(activity),
    )
        .into_response())
}
