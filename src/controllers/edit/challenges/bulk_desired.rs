use super::*;

pub(super) const CLEAR_DISABLED_KOTH_TARGETS_SQL: &str = r#"
    UPDATE "KothTargets"
       SET holder_participation_id = NULL, held_since = NULL
     WHERE game_id = $1 AND challenge_id = ANY($2)
       AND (holder_participation_id IS NOT NULL OR held_since IS NOT NULL)
"#;

pub(super) fn disabled_koth_challenge_ids(
    desired: bool,
    changed_runtimes: &[(i32, i16, bool)],
) -> Vec<i32> {
    if desired {
        return Vec::new();
    }
    changed_runtimes
        .iter()
        .filter(|(_, challenge_type, _)| *challenge_type == ChallengeType::KingOfTheHill as i16)
        .map(|(challenge_id, _, _)| *challenge_id)
        .collect()
}

pub(super) async fn claim_desired_state_operation(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
    action: BulkChallengeAction,
) -> AppResult<Option<Uuid>> {
    let lease_token = Uuid::new_v4();
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !claim_desired_state_slot(&mut transaction, lease_token).await? {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(None);
    }
    let claimed = sqlx::query_scalar::<_, Uuid>(
        r#"UPDATE "BulkChallengeMutationOperations"
              SET state = 1, lease_token = $3,
                  lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
            WHERE game_id = $1 AND operation_id = $2 AND action = $4
              AND (state = 0 OR (state = 1 AND lease_expires_at_utc <= clock_timestamp()))
          RETURNING lease_token"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .bind(action.as_i16())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if claimed.is_none() {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(None);
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(claimed)
}

async fn claim_desired_state_slot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease_token: Uuid,
) -> AppResult<bool> {
    let claimed = sqlx::query_scalar::<_, i16>(
        r#"WITH candidate AS (
               SELECT slot_id FROM "BulkChallengeDesiredStateSlots"
                WHERE lease_token IS NULL OR expires_at_utc <= clock_timestamp()
                ORDER BY slot_id FOR UPDATE SKIP LOCKED LIMIT 1
           )
           UPDATE "BulkChallengeDesiredStateSlots" slot
              SET lease_token = $1,
                  expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
             FROM candidate
            WHERE slot.slot_id = candidate.slot_id
           RETURNING slot.slot_id"#,
    )
    .bind(lease_token)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(claimed.is_some())
}

pub(super) async fn expire_desired_state_lease(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) -> AppResult<()> {
    sqlx::query(
        r#"WITH operation AS (
               UPDATE "BulkChallengeMutationOperations"
                  SET lease_expires_at_utc = clock_timestamp()
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $3
           )
           UPDATE "BulkChallengeDesiredStateSlots"
              SET lease_token = NULL, expires_at_utc = NULL
            WHERE lease_token = $3"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn complete_desired_state(
    st: &SharedState,
    game_id: i32,
    request: &BulkChallengeMutationRequest,
    lease_token: Uuid,
    reconcile: bool,
) -> AppResult<BulkChallengeMutationResult> {
    let desired = request.action == BulkChallengeAction::Enable;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let game_state = sqlx::query_as::<_, (i64, bool, bool)>(
        r#"SELECT challenge_configuration_revision,
                  ad_scoring_start_round IS NOT NULL OR koth_scoring_start_round IS NOT NULL,
                  start_time_utc <= clock_timestamp() AND end_time_utc >= clock_timestamp()
             FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let stored = sqlx::query_as::<
        _,
        (
            i16,
            serde_json::Value,
            Option<i64>,
            Option<Uuid>,
            serde_json::Value,
            Vec<i32>,
            i16,
        ),
    >(
        r#"SELECT state, result, result_revision, lease_token, effects,
                  cleanup_completed_ids, effect_progress
             FROM "BulkChallengeMutationOperations"
            WHERE game_id = $1 AND operation_id = $2
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if stored.0 == 2 {
        let outcomes = serde_json::from_value(stored.1)
            .map_err(|error| AppError::internal(error.to_string()))?;
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(BulkChallengeMutationResult {
            operation_id: request.operation_id,
            state: "Complete",
            configuration_revision: stored.2.unwrap_or(game_state.0),
            outcomes,
        });
    }
    if stored.0 != 1 || stored.3 != Some(lease_token) {
        return Err(AppError::conflict(
            "This bulk mutation is owned by another recovery request",
        ));
    }
    if stored.2.is_some() {
        let outcomes = serde_json::from_value(stored.1)
            .map_err(|error| AppError::internal(format!("Invalid bulk result: {error}")))?;
        let effects = serde_json::from_value(stored.4)
            .map_err(|error| AppError::internal(format!("Invalid bulk effects: {error}")))?;
        let result_revision = stored.2.unwrap_or(game_state.0);
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        if !reconcile {
            return Ok(BulkChallengeMutationResult {
                operation_id: request.operation_id,
                state: "Pending",
                configuration_revision: result_revision,
                outcomes,
            });
        }
        return reconcile_desired_state(
            st,
            game_id,
            request,
            lease_token,
            result_revision,
            outcomes,
            effects,
            stored.5,
            stored.6,
        )
        .await;
    }
    if game_state.0 != request.expected_revision {
        drop(control);
        abandon_claimed_operation(st, game_id, request.operation_id, lease_token).await;
        return Err(AppError::conflict(format!(
            "Challenge configuration changed; current revision is {}",
            game_state.0
        )));
    }

    let rows = sqlx::query_as::<_, SelectedChallenge>(
        r#"SELECT challenge.id, challenge.title, challenge."Type" AS challenge_type,
                  challenge.is_enabled, challenge.deletion_pending,
                  challenge.review_status,
                  EXISTS (SELECT 1 FROM "FlagContexts" flag
                           WHERE flag.challenge_id = challenge.id
                             AND flag.is_occupied = FALSE
                             AND OCTET_LENGTH(flag.flag) BETWEEN 1 AND $3
                             AND NOT rsctf_flag_has_boundary_whitespace(flag.flag)) AS has_flag,
                  EXISTS (SELECT 1 FROM "FlagContexts" flag
                           WHERE flag.challenge_id = challenge.id
                             AND flag.is_occupied = FALSE
                             AND NOT (
                                 OCTET_LENGTH(flag.flag) BETWEEN 1 AND $3
                                 AND NOT rsctf_flag_has_boundary_whitespace(flag.flag)
                             )) AS has_invalid_flag,
                  challenge.ad_self_hosted
             FROM "GameChallenges" challenge
            WHERE challenge.game_id = $1 AND challenge.id = ANY($2)
            ORDER BY challenge.id
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(&request.challenge_ids)
    .bind(
        i32::try_from(crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES)
            .expect("normal flag bound fits i32"),
    )
    .fetch_all(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let by_id = rows
        .iter()
        .map(|row| (row.id, row))
        .collect::<std::collections::HashMap<_, _>>();
    let mut changed_ids = Vec::new();
    let mut changed_titles = Vec::new();
    let mut changed_runtimes = Vec::new();
    let mut outcomes = Vec::with_capacity(request.challenge_ids.len());
    for challenge_id in &request.challenge_ids {
        let Some(row) = by_id.get(challenge_id) else {
            outcomes.push(BulkChallengeOutcome {
                challenge_id: *challenge_id,
                status: "Rejected".into(),
                message: Some("Challenge not found in this event".into()),
            });
            continue;
        };
        let engine_type = row.challenge_type == ChallengeType::AttackDefense as i16
            || row.challenge_type == ChallengeType::KingOfTheHill as i16;
        let static_type = row.challenge_type == ChallengeType::StaticAttachment as i16
            || row.challenge_type == ChallengeType::StaticContainer as i16;
        let rejection = if row.deletion_pending {
            Some("Challenge is being deleted")
        } else if row.review_status != ChallengeReviewStatus::Active as i16 {
            Some("Only active challenges can be changed")
        } else if engine_type && game_state.1 && row.is_enabled != desired {
            Some("Engine challenge state is locked after scoring started")
        } else if desired && static_type && row.has_invalid_flag {
            Some("Cannot enable a challenge with a non-canonical flag")
        } else if desired && static_type && !row.has_flag {
            Some("Cannot enable a static challenge without a flag")
        } else {
            None
        };
        if let Some(message) = rejection {
            outcomes.push(BulkChallengeOutcome {
                challenge_id: *challenge_id,
                status: "Rejected".into(),
                message: Some(message.into()),
            });
        } else if row.is_enabled == desired {
            outcomes.push(BulkChallengeOutcome {
                challenge_id: *challenge_id,
                status: "Unchanged".into(),
                message: None,
            });
        } else {
            changed_ids.push(*challenge_id);
            changed_titles.push(row.title.clone());
            changed_runtimes.push((row.id, row.challenge_type, row.ad_self_hosted));
            outcomes.push(BulkChallengeOutcome {
                challenge_id: *challenge_id,
                status: "Changed".into(),
                message: None,
            });
        }
    }

    if !changed_ids.is_empty() {
        let progress = sqlx::query(
            r#"UPDATE "GameChallenges" SET is_enabled = $3
                WHERE game_id = $1 AND id = ANY($2)"#,
        )
        .bind(game_id)
        .bind(&changed_ids)
        .bind(desired)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if progress.rows_affected() != changed_ids.len() as u64 {
            return Err(AppError::conflict(
                "A selected challenge changed during the bulk mutation",
            ));
        }
    }
    let disabled_koth_ids = disabled_koth_challenge_ids(desired, &changed_runtimes);
    if !disabled_koth_ids.is_empty() {
        sqlx::query(CLEAR_DISABLED_KOTH_TARGETS_SQL)
            .bind(game_id)
            .bind(&disabled_koth_ids)
            .execute(&mut **control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let result_revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT challenge_configuration_revision FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let result_json =
        serde_json::to_value(&outcomes).map_err(|error| AppError::internal(error.to_string()))?;
    let notice_id = if desired && game_state.2 && !changed_titles.is_empty() {
        let values = serde_json::json!(changed_titles);
        Some(
            sqlx::query_scalar::<_, i32>(
                r#"INSERT INTO "GameNotices"
                       (game_id, "Type", values, publish_time_utc, bulk_operation_id)
                   VALUES ($1, $2, $3, clock_timestamp(), $4)
                   ON CONFLICT (game_id, bulk_operation_id)
                     WHERE bulk_operation_id IS NOT NULL
                   DO UPDATE SET bulk_operation_id = EXCLUDED.bulk_operation_id
                   RETURNING id"#,
            )
            .bind(game_id)
            .bind(NoticeType::NewChallenge as i16)
            .bind(values)
            .bind(request.operation_id)
            .fetch_one(&mut **control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?,
        )
    } else {
        None
    };
    let effects = DesiredStateEffects {
        changed_runtimes: changed_runtimes
            .into_iter()
            .map(
                |(challenge_id, challenge_type, ad_self_hosted)| DesiredRuntimeEffect {
                    challenge_id,
                    challenge_type,
                    ad_self_hosted,
                },
            )
            .collect(),
        notice_id,
    };
    let effects_json =
        serde_json::to_value(&effects).map_err(|error| AppError::internal(error.to_string()))?;
    let persisted = sqlx::query(
        r#"UPDATE "BulkChallengeMutationOperations"
              SET result = $3, result_revision = $4, effects = $5,
                  lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
            WHERE game_id = $1 AND operation_id = $2 AND state = 1
              AND lease_token = $6"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .bind(result_json)
    .bind(result_revision)
    .bind(effects_json)
    .bind(lease_token)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if persisted.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Bulk challenge operation changed while it was running",
        ));
    }
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !reconcile {
        return Ok(BulkChallengeMutationResult {
            operation_id: request.operation_id,
            state: "Pending",
            configuration_revision: result_revision,
            outcomes,
        });
    }
    reconcile_desired_state(
        st,
        game_id,
        request,
        lease_token,
        result_revision,
        outcomes,
        effects,
        Vec::new(),
        0,
    )
    .await
}

async fn renew_desired_state_lease(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) -> AppResult<bool> {
    let renewed = sqlx::query_scalar::<_, i64>(
        r#"WITH slot AS (
               UPDATE "BulkChallengeDesiredStateSlots"
                  SET expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE lease_token = $3
              RETURNING 1
           ), operation AS (
               UPDATE "BulkChallengeMutationOperations"
                  SET lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $3 AND EXISTS (SELECT 1 FROM slot)
              RETURNING 1
           ) SELECT COUNT(*)::bigint FROM operation"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(renewed == 1)
}

async fn record_desired_cleanup(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
    challenge_id: i32,
) -> AppResult<bool> {
    let recorded = sqlx::query_scalar::<_, i64>(
        r#"WITH slot AS (
               UPDATE "BulkChallengeDesiredStateSlots"
                  SET expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE lease_token = $3
              RETURNING 1
           ), operation AS (
               UPDATE "BulkChallengeMutationOperations"
                  SET cleanup_completed_ids = CASE
                          WHEN $4 = ANY(cleanup_completed_ids) THEN cleanup_completed_ids
                          ELSE array_append(cleanup_completed_ids, $4)
                      END,
                      lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $3 AND EXISTS (SELECT 1 FROM slot)
              RETURNING 1
           ) SELECT COUNT(*)::bigint FROM operation"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .bind(challenge_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(recorded == 1)
}

async fn record_desired_effect(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
    effect: i16,
) -> AppResult<bool> {
    let recorded = sqlx::query_scalar::<_, i64>(
        r#"WITH slot AS (
               UPDATE "BulkChallengeDesiredStateSlots"
                  SET expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE lease_token = $3
              RETURNING 1
           ), operation AS (
               UPDATE "BulkChallengeMutationOperations"
                  SET effect_progress = effect_progress | $4,
                      lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $3 AND EXISTS (SELECT 1 FROM slot)
              RETURNING 1
           ) SELECT COUNT(*)::bigint FROM operation"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .bind(effect)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(recorded == 1)
}

fn effect_is_container(effect: &DesiredRuntimeEffect) -> bool {
    matches!(
        effect.challenge_type,
        value if value == ChallengeType::StaticContainer as i16
            || value == ChallengeType::DynamicContainer as i16
            || value == ChallengeType::AttackDefense as i16
            || value == ChallengeType::KingOfTheHill as i16
    )
}

pub(super) fn effect_has_runtime(effect: &DesiredRuntimeEffect) -> bool {
    effect_is_container(effect) || effect.ad_self_hosted
}

pub(super) fn effect_needs_vpn_reconciliation(effect: &DesiredRuntimeEffect) -> bool {
    effect.ad_self_hosted
        || effect.challenge_type == ChallengeType::AttackDefense as i16
        || effect.challenge_type == ChallengeType::KingOfTheHill as i16
}

async fn reconcile_desired_state(
    st: &SharedState,
    game_id: i32,
    request: &BulkChallengeMutationRequest,
    lease_token: Uuid,
    result_revision: i64,
    outcomes: Vec<BulkChallengeOutcome>,
    effects: DesiredStateEffects,
    cleanup_completed_ids: Vec<i32>,
    mut effect_progress: i16,
) -> AppResult<BulkChallengeMutationResult> {
    let desired = request.action == BulkChallengeAction::Enable;
    let mut completed = cleanup_completed_ids
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    if !desired {
        for effect in &effects.changed_runtimes {
            if !effect_has_runtime(effect) || completed.contains(&effect.challenge_id) {
                continue;
            }
            if !renew_desired_state_lease(st.pg(), game_id, request.operation_id, lease_token)
                .await?
            {
                return Err(AppError::conflict(
                    "This bulk mutation is owned by another recovery request",
                ));
            }
            tokio::time::timeout(BULK_DELETE_STEP_BUDGET, async {
                if effect_is_container(effect) {
                    super::super::lifecycle::destroy_challenge_containers_by_id(
                        st,
                        game_id,
                        effect.challenge_id,
                        true,
                        false,
                        false,
                    )
                    .await?;
                }
                if effect.ad_self_hosted {
                    st.byoc
                        .disconnect_challenge_deferred_vpn(&st.db, effect.challenge_id)
                        .await?;
                }
                Ok::<(), AppError>(())
            })
            .await
            .map_err(|_| {
                AppError::unavailable(
                    "Bulk challenge cleanup timed out and will resume from durable progress",
                )
            })??;
            if !record_desired_cleanup(
                st.pg(),
                game_id,
                request.operation_id,
                lease_token,
                effect.challenge_id,
            )
            .await?
            {
                return Err(AppError::conflict(
                    "This bulk mutation is owned by another recovery request",
                ));
            }
            completed.insert(effect.challenge_id);
        }
    }

    let needs_vpn = effects
        .changed_runtimes
        .iter()
        .any(effect_needs_vpn_reconciliation);
    if needs_vpn && effect_progress & EFFECT_VPN_RECONCILED == 0 {
        if !renew_desired_state_lease(st.pg(), game_id, request.operation_id, lease_token).await? {
            return Err(AppError::conflict(
                "This bulk mutation is owned by another recovery request",
            ));
        }
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
        if !record_desired_effect(
            st.pg(),
            game_id,
            request.operation_id,
            lease_token,
            EFFECT_VPN_RECONCILED,
        )
        .await?
        {
            return Err(AppError::conflict(
                "This bulk mutation is owned by another recovery request",
            ));
        }
        effect_progress |= EFFECT_VPN_RECONCILED;
    }
    if !effects.changed_runtimes.is_empty() && effect_progress & EFFECT_SCOREBOARDS_FLUSHED == 0 {
        if !renew_desired_state_lease(st.pg(), game_id, request.operation_id, lease_token).await? {
            return Err(AppError::conflict(
                "This bulk mutation is owned by another recovery request",
            ));
        }
        flush_game_scoreboards(st, game_id).await;
        if !record_desired_effect(
            st.pg(),
            game_id,
            request.operation_id,
            lease_token,
            EFFECT_SCOREBOARDS_FLUSHED,
        )
        .await?
        {
            return Err(AppError::conflict(
                "This bulk mutation is owned by another recovery request",
            ));
        }
        effect_progress |= EFFECT_SCOREBOARDS_FLUSHED;
    }
    if let Some(notice_id) = effects.notice_id {
        if effect_progress & EFFECT_NOTICE_PUBLISHED == 0 {
            if !renew_desired_state_lease(st.pg(), game_id, request.operation_id, lease_token)
                .await?
            {
                return Err(AppError::conflict(
                    "This bulk mutation is owned by another recovery request",
                ));
            }
            let notice = sqlx::query_as::<_, (serde_json::Value, DateTime<Utc>)>(
                r#"SELECT values, publish_time_utc FROM "GameNotices"
                    WHERE id = $1 AND game_id = $2 AND bulk_operation_id = $3"#,
            )
            .bind(notice_id)
            .bind(game_id)
            .bind(request.operation_id)
            .fetch_optional(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .ok_or_else(|| AppError::internal("Bulk challenge notice disappeared"))?;
            st.publish_event(
                "ReceivedGameNotice",
                Some(game_id),
                serde_json::json!({
                    "type": NoticeType::NewChallenge,
                    "values": notice.0,
                    "id": notice_id,
                    "time": notice.1,
                })
                .to_string(),
            );
            if !record_desired_effect(
                st.pg(),
                game_id,
                request.operation_id,
                lease_token,
                EFFECT_NOTICE_PUBLISHED,
            )
            .await?
            {
                return Err(AppError::conflict(
                    "This bulk mutation is owned by another recovery request",
                ));
            }
        }
    }

    let required_cleanup = if desired {
        Vec::new()
    } else {
        effects
            .changed_runtimes
            .iter()
            .filter(|effect| effect_has_runtime(effect))
            .map(|effect| effect.challenge_id)
            .collect::<Vec<_>>()
    };
    let mut required_effects = 0_i16;
    if needs_vpn {
        required_effects |= EFFECT_VPN_RECONCILED;
    }
    if !effects.changed_runtimes.is_empty() {
        required_effects |= EFFECT_SCOREBOARDS_FLUSHED;
    }
    if effects.notice_id.is_some() {
        required_effects |= EFFECT_NOTICE_PUBLISHED;
    }
    let completion = sqlx::query_scalar::<_, i64>(
        r#"WITH completed AS (
               UPDATE "BulkChallengeMutationOperations" operation
                  SET state = 2, lease_token = NULL,
                      completed_at_utc = clock_timestamp()
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $3 AND result_revision = $4
                  AND cleanup_completed_ids @> $5
                  AND (effect_progress & $6) = $6
                  AND EXISTS (
                      SELECT 1 FROM "BulkChallengeDesiredStateSlots" slot
                       WHERE slot.lease_token = $3
                  )
              RETURNING 1
           ), released AS (
               UPDATE "BulkChallengeDesiredStateSlots"
                  SET lease_token = NULL, expires_at_utc = NULL
                WHERE lease_token = $3 AND EXISTS (SELECT 1 FROM completed)
              RETURNING 1
           ) SELECT COUNT(*)::bigint FROM completed"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .bind(lease_token)
    .bind(result_revision)
    .bind(&required_cleanup)
    .bind(required_effects)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if completion != 1 {
        return Err(AppError::conflict(
            "This bulk mutation is owned by another recovery request",
        ));
    }
    cleanup_operations(st).await;
    Ok(BulkChallengeMutationResult {
        operation_id: request.operation_id,
        state: "Complete",
        configuration_revision: result_revision,
        outcomes,
    })
}

pub(super) fn spawn_desired_state_job_with_permit(
    st: SharedState,
    game_id: i32,
    request: BulkChallengeMutationRequest,
    lease_token: Uuid,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(error) = complete_desired_state(&st, game_id, &request, lease_token, true).await
        {
            tracing::error!(
                %error,
                game_id,
                operation_id = %request.operation_id,
                "bulk challenge desired-state reconciliation paused"
            );
            if let Err(expire_error) =
                expire_desired_state_lease(st.pg(), game_id, request.operation_id, lease_token)
                    .await
            {
                tracing::warn!(
                    %expire_error,
                    game_id,
                    operation_id = %request.operation_id,
                    "bulk desired-state lease expiry deferred"
                );
            }
        }
    });
}

pub(super) async fn recover_desired_state_jobs(st: &SharedState) -> AppResult<u64> {
    let mut started = 0_u64;
    loop {
        let Ok(permit) = BULK_DESIRED_STATE_SLOTS.clone().try_acquire_owned() else {
            break;
        };
        let lease_token = Uuid::new_v4();
        let mut transaction = st
            .pg()
            .begin()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let candidate = sqlx::query_as::<_, (i32, Uuid, i64, i16, Vec<i32>)>(
            r#"SELECT game_id, operation_id, expected_revision, action, challenge_ids
                 FROM "BulkChallengeMutationOperations"
                WHERE state IN (0, 1) AND action IN (0, 1)
                  AND lease_expires_at_utc <= clock_timestamp()
                ORDER BY lease_expires_at_utc, game_id, operation_id
                FOR UPDATE SKIP LOCKED
                LIMIT 1"#,
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let claimed = if let Some(candidate) = candidate {
            if claim_desired_state_slot(&mut transaction, lease_token).await? {
                sqlx::query_as::<_, (i32, Uuid, i64, i16, Vec<i32>)>(
                    r#"UPDATE "BulkChallengeMutationOperations"
                          SET state = 1, lease_token = $3,
                              lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                        WHERE game_id = $1 AND operation_id = $2 AND state IN (0, 1)
                          AND action IN (0, 1)
                          AND lease_expires_at_utc <= clock_timestamp()
                      RETURNING game_id, operation_id, expected_revision, action, challenge_ids"#,
                )
                .bind(candidate.0)
                .bind(candidate.1)
                .bind(lease_token)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?
            } else {
                None
            }
        } else {
            None
        };
        let Some((game_id, operation_id, expected_revision, action, challenge_ids)) = claimed
        else {
            transaction
                .rollback()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            drop(permit);
            break;
        };
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let action = match action {
            0 => BulkChallengeAction::Enable,
            1 => BulkChallengeAction::Disable,
            _ => {
                expire_desired_state_lease(st.pg(), game_id, operation_id, lease_token).await?;
                drop(permit);
                continue;
            }
        };
        spawn_desired_state_job_with_permit(
            st.clone(),
            game_id,
            BulkChallengeMutationRequest {
                operation_id,
                expected_revision,
                action,
                challenge_ids,
            },
            lease_token,
            permit,
        );
        started = started.saturating_add(1);
    }
    Ok(started)
}
