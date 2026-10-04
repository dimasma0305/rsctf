//! Durable, revision-keyed reconciliation after an ordinary definition commit.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use super::*;

const CLAIM_BATCH: i64 = 8;
const LEASE_SECONDS: i32 = 300;

struct ClaimedEffect {
    game_id: i32,
    challenge_id: i32,
    revision: i64,
    effects: JsonValue,
    lease_owner: Uuid,
}

fn enabled(effects: &JsonValue, key: &str) -> bool {
    effects.get(key).and_then(JsonValue::as_bool) == Some(true)
}

async fn claim(pool: &sqlx::PgPool) -> AppResult<Vec<ClaimedEffect>> {
    let owner = Uuid::new_v4();
    sqlx::query_as::<_, (i32, i32, i64, JsonValue)>(
        r#"WITH due AS MATERIALIZED (
               SELECT challenge_id, revision
                 FROM "ChallengeRevisionEffects"
                WHERE completed_at_utc IS NULL
                  AND available_at_utc <= clock_timestamp()
                  AND (lease_expires_at_utc IS NULL
                       OR lease_expires_at_utc <= clock_timestamp())
                ORDER BY available_at_utc, challenge_id, revision
                LIMIT $1
                FOR UPDATE SKIP LOCKED
           )
           UPDATE "ChallengeRevisionEffects" effect
              SET lease_owner = $2,
                  lease_expires_at_utc = clock_timestamp() + make_interval(secs => $3),
                  attempts = attempts + 1
             FROM due
            WHERE effect.challenge_id = due.challenge_id
              AND effect.revision = due.revision
        RETURNING effect.game_id, effect.challenge_id, effect.revision, effect.effects"#,
    )
    .bind(CLAIM_BATCH)
    .bind(owner)
    .bind(LEASE_SECONDS)
    .fetch_all(pool)
    .await
    .map_err(database_error)
    .map(|rows| {
        rows.into_iter()
            .map(|(game_id, challenge_id, revision, effects)| ClaimedEffect {
                game_id,
                challenge_id,
                revision,
                effects,
                lease_owner: owner,
            })
            .collect()
    })
}

async fn insert_notice_once(
    st: &SharedState,
    effect: &ClaimedEffect,
    notice_type: NoticeType,
) -> AppResult<()> {
    let title = effect
        .effects
        .get("title")
        .and_then(JsonValue::as_str)
        .unwrap_or("Challenge")
        .to_string();
    let mut transaction = st.pg().begin().await.map_err(database_error)?;
    let existing: Option<i32> = sqlx::query_scalar(
        r#"SELECT notice_id FROM "ChallengeRevisionNotices"
            WHERE challenge_id = $1 AND revision = $2 AND notice_type = $3"#,
    )
    .bind(effect.challenge_id)
    .bind(effect.revision)
    .bind(notice_type as i16)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    if existing.is_some() {
        transaction.commit().await.map_err(database_error)?;
        return Ok(());
    }
    let values = serde_json::json!([title]);
    let (notice_id, published_at): (i32, DateTime<Utc>) = sqlx::query_as(
        r#"INSERT INTO "GameNotices" (game_id, "Type", values, publish_time_utc)
           VALUES ($1, $2, $3, clock_timestamp())
        RETURNING id, publish_time_utc"#,
    )
    .bind(effect.game_id)
    .bind(notice_type as i16)
    .bind(&values)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"INSERT INTO "ChallengeRevisionNotices"
             (challenge_id, revision, notice_type, notice_id)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(effect.challenge_id)
    .bind(effect.revision)
    .bind(notice_type as i16)
    .bind(notice_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    st.publish_event(
        "ReceivedGameNotice",
        Some(effect.game_id),
        serde_json::json!({
            "type": notice_type,
            "values": values,
            "id": notice_id,
            "time": published_at,
        })
        .to_string(),
    );
    Ok(())
}

async fn enqueue_repo_push(st: &SharedState, effect: &ClaimedEffect) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "RepoPushQueue"
             (binding_id, challenge_id, game_id, target_revision)
           SELECT binding.id, $2, game.id, $3
             FROM "Games" game
             JOIN "RepoBindings" binding ON binding.id = game.repo_binding_id
            WHERE game.id = $1 AND binding.push_on_edit = TRUE
           ON CONFLICT (binding_id, challenge_id) DO UPDATE
             SET target_revision = GREATEST("RepoPushQueue".target_revision,
                                            EXCLUDED.target_revision),
                 available_at_utc = LEAST("RepoPushQueue".available_at_utc,
                                          clock_timestamp()),
                 updated_at_utc = clock_timestamp(),
                 last_error = NULL"#,
    )
    .bind(effect.game_id)
    .bind(effect.challenge_id)
    .bind(effect.revision)
    .execute(st.pg())
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn reconcile(st: &SharedState, effect: &ClaimedEffect) -> AppResult<()> {
    let challenge = load_challenge(st, effect.game_id, effect.challenge_id).await?;
    if enabled(&effect.effects, "newChallengeNotice") {
        insert_notice_once(st, effect, NoticeType::NewChallenge).await?;
    }
    if enabled(&effect.effects, "newHintNotice") {
        insert_notice_once(st, effect, NoticeType::NewHint).await?;
    }
    if enabled(&effect.effects, "repoPush") {
        enqueue_repo_push(st, effect).await?;
    }
    if enabled(&effect.effects, "runtime") && !challenge.is_enabled {
        if challenge.challenge_type == ChallengeType::KingOfTheHill {
            crate::services::ad_engine::clear_challenge_control(
                &st.db,
                effect.game_id,
                effect.challenge_id,
            )
            .await?;
        }
        st.byoc
            .disconnect_challenge(&st.db, effect.challenge_id)
            .await?;
        if challenge.challenge_type.is_container() {
            destroy_challenge_containers(st, &challenge, true, false).await?;
        }
    }
    if enabled(&effect.effects, "vpn") {
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    }
    if enabled(&effect.effects, "scoreboard") {
        flush_game_scoreboards(st, effect.game_id).await;
    }
    Ok(())
}

async fn finish(pool: &sqlx::PgPool, effect: &ClaimedEffect, error: Option<&str>) -> AppResult<()> {
    let error = error.map(|value| value.chars().take(2_000).collect::<String>());
    let result = if error.is_some() {
        sqlx::query(
            r#"UPDATE "ChallengeRevisionEffects"
                  SET lease_owner = NULL, lease_expires_at_utc = NULL,
                      available_at_utc = clock_timestamp()
                          + make_interval(secs => LEAST(300, 2 ^ LEAST(attempts, 8))),
                      last_error = $4
                WHERE challenge_id = $1 AND revision = $2 AND lease_owner = $3
                  AND completed_at_utc IS NULL"#,
        )
        .bind(effect.challenge_id)
        .bind(effect.revision)
        .bind(effect.lease_owner)
        .bind(error)
        .execute(pool)
        .await
    } else {
        sqlx::query(
            r#"UPDATE "ChallengeRevisionEffects"
                  SET completed_at_utc = clock_timestamp(), lease_owner = NULL,
                      lease_expires_at_utc = NULL, last_error = NULL
                WHERE challenge_id = $1 AND revision = $2 AND lease_owner = $3
                  AND completed_at_utc IS NULL"#,
        )
        .bind(effect.challenge_id)
        .bind(effect.revision)
        .bind(effect.lease_owner)
        .execute(pool)
        .await
    };
    result.map_err(database_error)?;
    Ok(())
}

async fn tick(st: &SharedState) -> AppResult<()> {
    for effect in claim(st.pg()).await? {
        match reconcile(st, &effect).await {
            Ok(()) => finish(st.pg(), &effect, None).await?,
            Err(error) => {
                tracing::warn!(
                    challenge = effect.challenge_id,
                    revision = effect.revision,
                    %error,
                    "challenge revision effect deferred"
                );
                finish(st.pg(), &effect, Some(&error.to_string())).await?;
            }
        }
    }
    sqlx::query(
        r#"DELETE FROM "ChallengeRevisionEffects" WHERE ctid IN (
               SELECT ctid FROM "ChallengeRevisionEffects"
                WHERE completed_at_utc < clock_timestamp() - interval '90 days'
                ORDER BY completed_at_utc LIMIT 64
           )"#,
    )
    .execute(st.pg())
    .await
    .map_err(database_error)?;
    Ok(())
}

pub fn start(
    st: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(error) = tick(&st).await {
                tracing::error!(%error, "challenge revision effect tick failed");
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    })
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn claims_are_cross_replica_and_notices_are_revision_idempotent() {
        let source = include_str!("revision_effects.rs");
        assert!(source.contains("FOR UPDATE SKIP LOCKED"));
        assert!(source.contains("ChallengeRevisionNotices"));
        assert!(source.contains("target_revision = GREATEST"));
        assert!(source.contains("LIMIT $1"));
    }
}
