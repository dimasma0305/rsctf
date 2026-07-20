use std::path::{Component, Path, PathBuf};

use sea_orm::EntityTrait;

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};

/// Resolve only the durable repository identity. Same-title manual rows are
/// deliberately never adopted: a title is presentation, not ownership proof.
pub(super) async fn find_repository_challenge(
    st: &SharedState,
    game_id: i32,
    binding_id: Option<i32>,
    source_yaml_path: Option<&str>,
) -> AppResult<Option<game_challenge::Model>> {
    let Some(source_yaml_path) = source_yaml_path else {
        return Ok(None);
    };
    let legacy_relative =
        binding_id.and_then(|binding_id| relative_manifest_identity(binding_id, source_yaml_path));
    let candidates = sqlx::query_as::<_, (i32, String)>(
        r#"SELECT id, source_yaml_path
             FROM "GameChallenges"
            WHERE game_id = $1 AND source_yaml_path IS NOT NULL
            ORDER BY id"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let ids = candidates
        .into_iter()
        .filter_map(|(id, stored)| {
            (stored == source_yaml_path
                || binding_id.zip(legacy_relative.as_deref()).is_some_and(
                    |(binding_id, relative)| {
                        stored == relative
                            || legacy_repo_manifest_matches(&stored, binding_id, relative)
                    },
                ))
            .then_some(id)
        })
        .collect::<Vec<_>>();
    match ids.as_slice() {
        [] => Ok(None),
        [challenge_id] => game_challenge::Entity::find_by_id(*challenge_id)
            .one(&st.db)
            .await
            .map_err(Into::into),
        _ => Err(AppError::bad_request(
            "repository manifest is already linked to multiple challenges",
        )),
    }
}

pub(super) fn scoped_manifest_identity(binding_id: i32, relative: &str) -> String {
    format!(
        "binding/{binding_id}/{}",
        relative.trim_start_matches('/').replace('\\', "/")
    )
}

fn relative_manifest_identity(binding_id: i32, stored: &str) -> Option<String> {
    stored
        .strip_prefix(&format!("binding/{binding_id}/"))
        .filter(|relative| !relative.is_empty())
        .map(str::to_owned)
}

/// Legacy releases persisted the replica-local absolute checkout path. Match
/// only the exact managed `repos/<binding>/<relative manifest>` suffix so a
/// different storage-root prefix can migrate the row without title adoption.
fn legacy_repo_manifest_matches(stored: &str, binding_id: i32, relative: &str) -> bool {
    let normalized = stored.replace('\\', "/");
    let is_absolute = normalized.starts_with('/')
        || normalized
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':');
    is_absolute
        && normalized.ends_with(&format!(
            "/repos/{binding_id}/{}",
            relative.trim_start_matches('/')
        ))
}

/// Resolve a stored binding-relative manifest below one checkout. Absolute
/// values remain accepted only for the legacy migration/read path.
pub(crate) fn manifest_candidate_in_checkout(
    checkout: &Path,
    binding_id: Option<i32>,
    stored: &str,
) -> Option<PathBuf> {
    let stored_path = Path::new(stored);
    if let Some(binding_id) = binding_id {
        if let Some(relative) = relative_manifest_identity(binding_id, stored) {
            return manifest_candidate_in_checkout(checkout, None, &relative);
        }
    }
    let normalized = stored.replace('\\', "/");
    let legacy_absolute = normalized.starts_with('/')
        || normalized
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':');
    if legacy_absolute {
        if let Some(binding_id) = binding_id {
            let marker = format!("/repos/{binding_id}/");
            if let Some((_, relative)) = normalized.rsplit_once(&marker) {
                return manifest_candidate_in_checkout(checkout, None, relative);
            }
        }
        return stored_path.is_absolute().then(|| stored_path.to_path_buf());
    }
    if stored_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(checkout.join(stored_path))
}

/// Disable repository-owned rows that disappeared from one completely
/// successful event scan. Identity/history stay intact. The caller holds its
/// per-game control lock only through this transaction; definition locks are
/// released at commit before any provisioning teardown begins.
pub(crate) async fn tombstone_missing_challenges(
    st: &SharedState,
    game_id: i32,
    seen_ids: &[i32],
) -> AppResult<Vec<i32>> {
    let stale = sqlx::query_scalar::<_, i32>(
        r#"SELECT id
             FROM "GameChallenges"
            WHERE game_id = $1
              AND source_yaml_path IS NOT NULL
              AND NOT (id = ANY($2))
            ORDER BY id"#,
    )
    .bind(game_id)
    .bind(seen_ids)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if stale.is_empty() {
        return Ok(stale);
    }

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let game_deletion_pending: Option<bool> =
        sqlx::query_scalar(r#"SELECT deletion_pending FROM "Games" WHERE id = $1 FOR SHARE"#)
            .bind(game_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    match game_deletion_pending {
        Some(false) => {}
        Some(true) => {
            transaction
                .rollback()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Err(AppError::conflict("Game is being deleted"));
        }
        None => {
            transaction
                .rollback()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Err(AppError::not_found("Game not found"));
        }
    }
    for challenge_id in &stale {
        let key = crate::services::challenge_workloads::definition_lock_key(game_id, *challenge_id);
        let acquired = crate::utils::single_flight::try_acquire_transaction_advisory_lock(
            &mut transaction,
            &key,
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !acquired {
            transaction
                .rollback()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Err(AppError::conflict(
                "a removed challenge definition is busy; retry the repository scan",
            ));
        }
        crate::utils::scoring::lock_jeopardy_flags_exclusive(&mut transaction, *challenge_id)
            .await?;
    }

    let deletion_pending = sqlx::query_scalar::<_, i32>(
        r#"SELECT id
              FROM "GameChallenges"
             WHERE id = ANY($1) AND deletion_pending = TRUE
             ORDER BY id"#,
    )
    .bind(&stale)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !deletion_pending.is_empty() {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::conflict(format!(
            "challenge deletion is already pending for challenge(s) {}; repository reconciliation cannot mutate them",
            deletion_pending
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    let protected = sqlx::query_scalar::<_, i32>(
        r#"SELECT challenge.id
             FROM "GameChallenges" challenge
             JOIN "Games" game ON game.id = challenge.game_id
            WHERE challenge.id = ANY($1)
              AND challenge.is_enabled = TRUE
              AND challenge."Type" = ANY($2)
              AND (
                    game.end_time_utc >= clock_timestamp()
                    OR EXISTS (
                          SELECT 1 FROM "AdRounds" round
                           WHERE round.game_id = game.id
                             AND round.finalized = FALSE
                    )
              )
            ORDER BY challenge.id"#,
    )
    .bind(&stale)
    .bind(
        &[
            ChallengeType::AttackDefense as i16,
            ChallengeType::KingOfTheHill as i16,
        ][..],
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !protected.is_empty() {
        let message = format!(
            "removed live A&D/KotH challenge(s) {} must be disabled explicitly before repository removal; retry after disabling or after game closeout finalizes",
            protected
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::conflict(message));
    }
    let protected_jeopardy = sqlx::query_scalar::<_, i32>(
        r#"SELECT challenge.id
             FROM "GameChallenges" challenge
             JOIN "Games" game ON game.id = challenge.game_id
            WHERE challenge.id = ANY($1)
              AND challenge.is_enabled = TRUE
              AND challenge."Type" <> ALL($2)
              AND (
                    game.start_time_utc <= clock_timestamp()
                    OR challenge.accepted_count > 0
                    OR challenge.submission_count > 0
                    OR EXISTS (
                          SELECT 1 FROM "Submissions" submission
                           WHERE submission.challenge_id = challenge.id
                    )
                    OR EXISTS (
                          SELECT 1 FROM "FirstSolves" solve
                           WHERE solve.challenge_id = challenge.id
                    )
              )
            ORDER BY challenge.id"#,
    )
    .bind(&stale)
    .bind(
        &[
            ChallengeType::AttackDefense as i16,
            ChallengeType::KingOfTheHill as i16,
        ][..],
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !protected_jeopardy.is_empty() {
        let message = format!(
            "removed enabled Jeopardy challenge(s) {} retain grading and scoreboard state after game start or submission/solve evidence; disable explicitly before repository removal",
            protected_jeopardy
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::conflict(message));
    }
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = ANY($1)"#)
        .bind(&stale)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // The caller owns the per-game KotH control lock, so holder publication is
    // cleared in the same short tombstone transaction. Cleanup after commit
    // must never reacquire that game lock while holding a runtime lock.
    sqlx::query(
        r#"UPDATE "KothTargets"
              SET holder_participation_id = NULL, held_since = NULL
            WHERE game_id = $1 AND challenge_id = ANY($2)"#,
    )
    .bind(game_id)
    .bind(&stale)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_identity_matches_only_the_same_binding_relative_suffix() {
        assert!(legacy_repo_manifest_matches(
            "/old/root/repos/7/event/web/challenge.yaml",
            7,
            "event/web/challenge.yaml"
        ));
        assert!(legacy_repo_manifest_matches(
            r"C:\rsctf\repos\7\event\web\challenge.yaml",
            7,
            "event/web/challenge.yaml"
        ));
        assert!(!legacy_repo_manifest_matches(
            "/old/root/repos/8/event/web/challenge.yaml",
            7,
            "event/web/challenge.yaml"
        ));
        assert!(!legacy_repo_manifest_matches(
            "event/web/challenge.yaml",
            7,
            "event/web/challenge.yaml"
        ));
    }

    #[test]
    fn relative_manifest_candidates_cannot_escape_the_checkout() {
        let checkout = Path::new("/srv/repos/7");
        assert_eq!(
            manifest_candidate_in_checkout(checkout, Some(7), "binding/7/event/web/challenge.yaml"),
            Some(checkout.join("event/web/challenge.yaml"))
        );
        assert_eq!(
            manifest_candidate_in_checkout(
                checkout,
                Some(7),
                r"C:\old\repos\7\event\web\challenge.yaml"
            ),
            Some(checkout.join("event/web/challenge.yaml"))
        );
        assert_eq!(
            manifest_candidate_in_checkout(
                checkout,
                Some(7),
                "/old/root/repos/7/event/web/challenge.yaml"
            ),
            Some(checkout.join("event/web/challenge.yaml"))
        );
        assert!(manifest_candidate_in_checkout(checkout, Some(7), "../challenge.yaml").is_none());
    }
}
