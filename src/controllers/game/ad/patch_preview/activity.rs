use super::*;

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
