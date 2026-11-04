use super::*;

async fn policy_still_blocked(
    pool: &sqlx::PgPool,
    binding_id: i32,
    game_ids: &[i32],
) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT cardinality($2::integer[]) > 0 AND NOT EXISTS (
             SELECT id FROM unnest($2::integer[]) blocked(id)
              WHERE NOT EXISTS (SELECT 1 FROM "Games" game
                WHERE game.id = blocked.id AND game.repo_binding_id = $1
                  AND (game.ad_scoring_start_round IS NOT NULL
                       OR game.koth_scoring_start_round IS NOT NULL)))"#,
    )
    .bind(binding_id)
    .bind(game_ids)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// A metadata-only ref probe avoids downloading the same known-blocked tree.
/// A manual scan or binding edit explicitly clears this state. Policy changes
/// also reopen the same commit, without claiming a blocked import succeeded.
pub(super) async fn unchanged_block(
    st: &SharedState,
    binding: &repo_binding::Model,
    url: &str,
    git_ref: Option<&str>,
) -> AppResult<Option<RepoBindingScanExecution>> {
    let outcome = match binding.scan_outcome.as_str() {
        "PolicyBlocked" => RepoScanOutcome::PolicyBlocked,
        "PermanentFailure" => RepoScanOutcome::PermanentFailure,
        _ => return Ok(None),
    };
    let Some(blocked_sha) = binding.blocked_commit_sha.as_deref() else {
        return Ok(None);
    };
    if outcome == RepoScanOutcome::PolicyBlocked {
        let still_blocked =
            policy_still_blocked(st.pg(), binding.id, &binding.scan_blocked_game_ids).await?;
        if !still_blocked {
            return Ok(None);
        }
    }
    if crate::services::git_sync::remote_head_sha(url, git_ref)
        .await?
        .as_deref()
        != Some(blocked_sha)
    {
        return Ok(None);
    }
    let message = binding.last_scan_message.clone().unwrap_or_default();
    Ok(Some(RepoBindingScanExecution {
        result: RepoBindingScanResultModel {
            outcome,
            games_created: 0,
            games_updated: 0,
            challenges_imported: 0,
            challenges_updated: 0,
            failures: 0,
            messages: vec![message.clone()],
        },
        ran_at: Utc::now(),
        commit_sha: Some(blocked_sha.to_owned()),
        message,
        blocked_games: binding.scan_blocked_game_ids.clone(),
        unchanged_block: true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn policy_probe_reopens_when_any_blocked_game_is_unlocked_or_detached() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&std::env::var("RSCTF_TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        sqlx::raw_sql(r#"CREATE TEMP TABLE "Games" (id INTEGER, repo_binding_id INTEGER, ad_scoring_start_round INTEGER, koth_scoring_start_round INTEGER);
            INSERT INTO "Games" VALUES (45,24,1,NULL), (46,24,NULL,1), (47,25,NULL,1);"#)
            .execute(&pool).await.unwrap();
        assert!(policy_still_blocked(&pool, 24, &[45, 46]).await.unwrap());
        assert!(!policy_still_blocked(&pool, 24, &[]).await.unwrap());
        assert!(!policy_still_blocked(&pool, 24, &[47]).await.unwrap());
        assert!(!policy_still_blocked(&pool, 24, &[45, 999]).await.unwrap());
        sqlx::query(r#"UPDATE "Games" SET ad_scoring_start_round = NULL WHERE id = 45"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!policy_still_blocked(&pool, 24, &[45, 46]).await.unwrap());
        pool.close().await;
    }
}
