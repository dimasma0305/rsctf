use super::*;

/// `GET /api/edit/games/{id}/pendingchallenges` — Pending + Rejected rows.
pub async fn list_pending_challenges(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<PendingChallengeModel>>> {
    manager_or_admin(&st, &user, id).await?;
    load_game(&st, id).await?;
    let rows = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(id))
        .filter(game_challenge::Column::ReviewStatus.ne(ChallengeReviewStatus::Active))
        // RSCTF orders by ReviewStatus ASC first, then SubmittedAtUtc DESC.
        .order_by_asc(game_challenge::Column::ReviewStatus)
        .order_by_desc(game_challenge::Column::SubmittedAtUtc)
        .all(&st.db)
        .await?;

    // Resolve submittedByUserName via a single batched join on `user`.
    let user_ids: Vec<Uuid> = rows.iter().filter_map(|c| c.submitted_by_user_id).collect();
    let user_names = load_user_names(&st, user_ids).await?;

    let data = rows
        .iter()
        .map(|c| {
            let mut m = PendingChallengeModel::from_challenge(c);
            m.submitted_by_user_name = c
                .submitted_by_user_id
                .and_then(|uid| user_names.get(&uid).cloned());
            m
        })
        .collect();
    Ok(RequestResponse::ok(data))
}

/// `POST /api/edit/games/{id}/challenges/{cId}/approve` — void.
pub async fn approve_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    // Review activation/deactivation changes runtime eligibility. Retain the
    // challenge-wide transition through checker/build publication or teardown
    // so a concurrent approve/reject cannot overtake stale cleanup.
    let runtime_transition =
        crate::services::challenge_workloads::acquire_runtime_transition_lock(st.pg(), c_id)
            .await?;
    let mut challenge = load_challenge(&st, id, c_id).await?;
    deletion::reject_pending_mutation(st.pg(), id, c_id).await?;
    if challenge.review_status == ChallengeReviewStatus::Active {
        runtime_transition
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(MessageResponse::ok(""));
    }
    if let Some(spec) = challenge.workload_spec.clone() {
        crate::services::challenge_workloads::validate_json_for_challenge(
            challenge.challenge_type,
            spec,
        )?;
    }
    let mut engine_control = if challenge.challenge_type.uses_ad_engine() {
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?)
    } else {
        None
    };
    if challenge.challenge_type.uses_ad_engine()
        && ad_epoch_scoring_started_locked(
            engine_control
                .as_mut()
                .expect("engine challenge holds the game control lock")
                .transaction_mut(),
            id,
        )
        .await?
    {
        return Err(AppError::bad_request(
            "A&D/KotH challenge review state is locked after epoch scoring has started.",
        ));
    }

    // A submitted archive is immutable blob content. Prepare its reviewed
    // process checker into a unique revision while holding the same distributed
    // fence as checker GC. The row remains Pending/Rejected until the complete
    // directory exists on shared storage.
    let mut checker_artifact_guard = if challenge.challenge_type.uses_ad_engine() {
        Some(crate::services::git_sync::acquire_checker_artifact_guard(&st).await?)
    } else {
        None
    };
    let checker_path = if challenge.challenge_type.uses_ad_engine() {
        crate::services::git_sync::prepare_reviewed_checker(&st, &challenge).await?
    } else {
        challenge.ad_checker_image.clone()
    };

    // Pending local-container imports deliberately retain the complete reviewed
    // package and do not execute Docker. Before activation, publish the valid
    // checker path while the row is still inert, then build the immutable source
    // fingerprint using its persisted context-subdirectory selector.
    let requires_successful_build = challenge.workload_spec.is_none()
        && challenge.challenge_type.is_container()
        && challenge
            .container_image
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
    if challenge.build_context_subdir.is_some()
        && challenge
            .original_archive_blob_path
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(AppError::bad_request(
            "Challenge build definition is incomplete; it remains pending.",
        ));
    }
    let needs_build = requires_successful_build
        && crate::services::challenge_workloads::resolve_runtime(&st, &challenge).is_err();
    if needs_build {
        let staged = if let Some(control) = engine_control.as_mut() {
            sqlx::query(
                r#"UPDATE "GameChallenges"
                      SET ad_checker_image = $3,
                          build_status = $4,
                          build_image_digest = NULL
                    WHERE id = $1
                      AND game_id = $2
                      AND deletion_pending = FALSE
                      AND review_status <> $5
                      AND original_archive_blob_path IS NOT DISTINCT FROM $6
                      AND container_image IS NOT DISTINCT FROM $7
                      AND build_context_subdir IS NOT DISTINCT FROM $8"#,
            )
            .bind(c_id)
            .bind(id)
            .bind(checker_path.as_deref())
            .bind(ChallengeBuildStatus::Queued as i16)
            .bind(ChallengeReviewStatus::Active as i16)
            .bind(challenge.original_archive_blob_path.as_deref())
            .bind(challenge.container_image.as_deref())
            .bind(challenge.build_context_subdir.as_deref())
            .execute(&mut **control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .rows_affected()
        } else {
            sqlx::query(
                r#"UPDATE "GameChallenges"
                      SET ad_checker_image = $3,
                          build_status = $4,
                          build_image_digest = NULL
                    WHERE id = $1
                      AND game_id = $2
                      AND deletion_pending = FALSE
                      AND review_status <> $5
                      AND original_archive_blob_path IS NOT DISTINCT FROM $6
                      AND container_image IS NOT DISTINCT FROM $7
                      AND build_context_subdir IS NOT DISTINCT FROM $8"#,
            )
            .bind(c_id)
            .bind(id)
            .bind(checker_path.as_deref())
            .bind(ChallengeBuildStatus::Queued as i16)
            .bind(ChallengeReviewStatus::Active as i16)
            .bind(challenge.original_archive_blob_path.as_deref())
            .bind(challenge.container_image.as_deref())
            .bind(challenge.build_context_subdir.as_deref())
            .execute(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .rows_affected()
        };
        if staged != 1 {
            return Err(AppError::bad_request(
                "Challenge review state changed; reload and retry.",
            ));
        }

        // Commit checker reachability before invoking the coordinated builder,
        // which rechecks the exact image/archive/context triple under its own
        // cross-replica image lock. The challenge remains Pending throughout.
        if let Some(lock) = engine_control.take() {
            lock.release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        }
        if let Some(guard) = checker_artifact_guard.take() {
            guard
                .release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        }

        challenge.ad_checker_image = checker_path.clone();
        challenge.build_status = ChallengeBuildStatus::Queued;
        let (outcome, _) =
            crate::controllers::edit::run_challenge_build(&st, &challenge, "Approval", 1).await;
        if outcome.status != ChallengeBuildStatus::Success {
            return Err(AppError::bad_request(format!(
                "Challenge remains pending because its reviewed image did not build successfully: {}",
                outcome
                    .log
                    .as_deref()
                    .unwrap_or("build did not complete successfully")
            )));
        }
        challenge = load_challenge(&st, id, c_id).await?;
        if challenge.challenge_type.uses_ad_engine() {
            let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
            if ad_epoch_scoring_started_locked(control.transaction_mut(), id).await? {
                return Err(AppError::bad_request(
                    "The challenge image was built but remains pending because A&D/KotH epoch scoring started during approval.",
                ));
            }
            engine_control = Some(control);
        }
    }

    let updated = if let Some(control) = engine_control.as_mut() {
        sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET ad_checker_image = $3,
                      review_status = $4,
                      reviewed_at_utc = clock_timestamp()
                WHERE id = $1
                  AND game_id = $2
                  AND deletion_pending = FALSE
                  AND review_status <> $4
                  AND ($5 = FALSE OR (build_status = $6
                       AND build_image_digest IS NOT DISTINCT FROM $7))
                  AND original_archive_blob_path IS NOT DISTINCT FROM $8
                  AND container_image IS NOT DISTINCT FROM $9
                  AND build_context_subdir IS NOT DISTINCT FROM $10"#,
        )
        .bind(c_id)
        .bind(id)
        .bind(checker_path.as_deref())
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(requires_successful_build)
        .bind(ChallengeBuildStatus::Success as i16)
        .bind(challenge.build_image_digest.as_deref())
        .bind(challenge.original_archive_blob_path.as_deref())
        .bind(challenge.container_image.as_deref())
        .bind(challenge.build_context_subdir.as_deref())
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected()
    } else {
        sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET review_status = $3,
                      reviewed_at_utc = clock_timestamp()
                WHERE id = $1
                  AND game_id = $2
                  AND deletion_pending = FALSE
                  AND review_status <> $3
                  AND ($4 = FALSE OR (build_status = $5
                       AND build_image_digest IS NOT DISTINCT FROM $6))
                  AND original_archive_blob_path IS NOT DISTINCT FROM $7
                  AND container_image IS NOT DISTINCT FROM $8
                  AND build_context_subdir IS NOT DISTINCT FROM $9"#,
        )
        .bind(c_id)
        .bind(id)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(requires_successful_build)
        .bind(ChallengeBuildStatus::Success as i16)
        .bind(challenge.build_image_digest.as_deref())
        .bind(challenge.original_archive_blob_path.as_deref())
        .bind(challenge.container_image.as_deref())
        .bind(challenge.build_context_subdir.as_deref())
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected()
    };
    if updated != 1 {
        return Err(AppError::bad_request(
            "Challenge review state changed; reload and retry.",
        ));
    }
    if let Some(lock) = engine_control {
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if let Some(guard) = checker_artifact_guard.take() {
        guard
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    flush_game_scoreboards(&st, id).await;
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    runtime_transition
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(MessageResponse::ok(""))
}

/// `POST /api/edit/games/{id}/challenges/{cId}/reject` — void.
pub async fn reject_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
    Json(model): Json<RejectChallengeModel>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    let runtime_transition =
        crate::services::challenge_workloads::acquire_runtime_transition_lock(st.pg(), c_id)
            .await?;
    let challenge = load_challenge(&st, id, c_id).await?;
    deletion::reject_pending_mutation(st.pg(), id, c_id).await?;
    let mut engine_control = if challenge.challenge_type.uses_ad_engine() {
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?)
    } else {
        None
    };
    if challenge.challenge_type.uses_ad_engine()
        && ad_epoch_scoring_started_locked(
            engine_control
                .as_mut()
                .expect("engine challenge holds the game control lock")
                .transaction_mut(),
            id,
        )
        .await?
    {
        return Err(AppError::bad_request(
            "A&D/KotH challenge review state is locked after epoch scoring has started.",
        ));
    }
    let rejected = sqlx::query(
        r#"UPDATE "GameChallenges"
              SET review_status = $3,
                  reviewed_at_utc = clock_timestamp(),
                  review_note = $4
            WHERE id = $1 AND game_id = $2
              AND deletion_pending = FALSE"#,
    )
    .bind(c_id)
    .bind(id)
    .bind(ChallengeReviewStatus::Rejected as i16)
    .bind(model.note)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if rejected != 1 {
        return Err(AppError::conflict("Challenge is being deleted"));
    }
    if challenge.challenge_type == ChallengeType::KingOfTheHill {
        crate::services::ad_engine::clear_challenge_control(&st.db, id, c_id).await?;
    }
    if let Some(lock) = engine_control {
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    flush_game_scoreboards(&st, id).await;
    st.byoc.disconnect_challenge(&st.db, c_id).await?;
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    if challenge.challenge_type.is_container() {
        let _ = destroy_challenge_containers(&st, &challenge, true, false).await;
    }
    runtime_transition
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(MessageResponse::ok(""))
}
