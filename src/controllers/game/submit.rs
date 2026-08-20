//! Player flag submission, challenge review, and submission status —
//! split from play.rs to keep each file under the 1000-line rule.
use super::*;

#[path = "submit_observations.rs"]
mod observations;
use observations::{
    load_first_positive_interactions, lock_game_timing_at_grade, lock_submit_caller_at_grade,
};
#[path = "submit_review.rs"]
mod review;
pub use review::{review_challenge, status};

const LOAD_GRADING_POLICY_SQL: &str = r#"
    SELECT submission_limit, deadline_utc, disable_blood_bonus, "Type",
           shared_container_id, solve_receipt_mode, variant_mode
      FROM "GameChallenges"
     WHERE id = $1 AND game_id = $2 AND is_enabled AND review_status = $3
"#;

const FINALIZE_SUBMISSION_SQL: &str = r#"
    UPDATE "GameChallenges"
       SET submission_count = submission_count + 1,
           accepted_count   = accepted_count + $2
     WHERE id = $1
       AND game_id = $3
       AND is_enabled
       AND review_status = $4
       AND submission_limit = $5
       AND deadline_utc IS NOT DISTINCT FROM $6
       AND disable_blood_bonus = $7
       AND "Type" = $8
       AND shared_container_id IS NOT DISTINCT FROM $9
       AND solve_receipt_mode = $10
       AND variant_mode = $11
"#;

async fn grade_variant_answer(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
    answer: &str,
) -> AppResult<(AnswerResult, Option<i32>)> {
    let variants = sqlx::query_as::<_, (i32, String)>(
        r#"SELECT participation_id, manifest->>'flag'
             FROM "ChallengeVariants"
            WHERE game_id = $1 AND challenge_id = $2
              AND frozen_at_utc IS NOT NULL
            ORDER BY participation_id"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !variants
        .iter()
        .any(|(candidate, _)| *candidate == participation_id)
    {
        return Err(AppError::unavailable(
            "This participation's deterministic challenge variant is not ready",
        ));
    }
    for (owner, flag) in variants {
        if ct_eq(&flag, answer) {
            return if owner == participation_id {
                Ok((AnswerResult::Accepted, None))
            } else {
                Ok((AnswerResult::CheatDetected, Some(owner)))
            };
        }
    }
    Ok((AnswerResult::WrongAnswer, None))
}

fn normal_flag_submit_type_allowed(
    challenge_type: i16,
    practice_mode: bool,
    submit_time: DateTime<Utc>,
    game_end: DateTime<Utc>,
) -> bool {
    let uses_jeopardy_scoring = challenge_type == ChallengeType::StaticAttachment as i16
        || challenge_type == ChallengeType::StaticContainer as i16
        || challenge_type == ChallengeType::DynamicAttachment as i16
        || challenge_type == ChallengeType::DynamicContainer as i16;
    if uses_jeopardy_scoring {
        return true;
    }
    let uses_live_engine = challenge_type == ChallengeType::AttackDefense as i16
        || challenge_type == ChallengeType::KingOfTheHill as i16;
    uses_live_engine && practice_mode && submit_time >= game_end
}

/// Count prior first solves that are eligible to consume a blood slot. Called
/// while the challenge-global advisory lock is held, so two teams solving at the
/// same instant cannot both announce the same tier.
async fn count_blood_eligible_solves(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    deadline: Option<DateTime<Utc>>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
             FROM (
               SELECT first_solve.participation_id
                 FROM "FirstSolves" first_solve
                 JOIN "Submissions" submission
                   ON submission.id = first_solve.submission_id
                  AND submission.participation_id = first_solve.participation_id
                  AND submission.challenge_id = first_solve.challenge_id
                 JOIN "Participations" participation
                   ON participation.id = first_solve.participation_id
                  AND participation.game_id = $1
                  AND participation.status = $7
                 LEFT JOIN "Divisions" division
                   ON division.id = participation.division_id
                  AND division.game_id = participation.game_id
                 LEFT JOIN "DivisionChallengeConfigs" permission
                   ON permission.division_id = participation.division_id
                  AND permission.challenge_id = first_solve.challenge_id
                WHERE first_solve.challenge_id = $2
                  AND submission.status = $8
                  AND submission.submit_time_utc >= $3
                  AND submission.submit_time_utc < $4
                  AND ($5::timestamptz IS NULL OR submission.submit_time_utc <= $5)
                  AND (
                    participation.division_id IS NULL
                    OR (
                      division.id IS NOT NULL
                      AND (COALESCE(permission.permissions, division.default_permissions, 0) & $6) = $6
                    )
                  )
                ORDER BY submission.submit_time_utc, participation.id
                LIMIT 3
             ) eligible"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(start)
    .bind(end)
    .bind(deadline)
    .bind(GamePermission::GET_BLOOD | GamePermission::GET_SCORE)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(AnswerResult::Accepted as i16)
    .fetch_one(connection)
    .await
}

/// Claim the one canonical solve row for a participation/challenge. The return
/// value, not the accepted submission status by itself, drives accepted_count.
/// The unique key makes a repeated accepted flag an idempotent no-op.
async fn claim_first_solve(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
    challenge_id: i32,
    submission_id: i32,
) -> AppResult<bool> {
    sqlx::query_scalar::<_, i32>(
        r#"INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
           VALUES ($1, $2, $3)
           ON CONFLICT (participation_id, challenge_id) DO NOTHING
           RETURNING submission_id"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .bind(submission_id)
    .fetch_optional(connection)
    .await
    .map(|claimed| claimed.is_some())
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Authoritative all-team flag snapshot, read on the grading transaction while
/// the challenge flag fence is held. Ordered participation ids make provenance
/// deterministic even if malformed legacy data reused a dynamic flag.
async fn load_challenge_flag_map(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
) -> AppResult<BTreeMap<i32, String>> {
    let rows: Vec<(i32, String)> = sqlx::query_as(
        r#"SELECT instance.participation_id, flag.flag
             FROM "GameInstances" instance
             JOIN "FlagContexts" flag ON flag.id = instance.flag_id
            WHERE instance.challenge_id = $1
            ORDER BY instance.participation_id
            FOR SHARE OF instance, flag"#,
    )
    .bind(challenge_id)
    .fetch_all(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows.into_iter().collect())
}

/// Grade a per-team flag and, on a wrong/missing own flag, classify a matching
/// foreign flag from the same locked snapshot. Missing an own instance is not a
/// reason to return early: possession of another team's flag is still evidence.
async fn grade_dynamic_answer(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
    challenge_id: i32,
    answer: &str,
    detect_stolen: bool,
) -> AppResult<(AnswerResult, Option<i32>)> {
    let own_flag: Option<String> = sqlx::query_scalar(
        r#"SELECT flag.flag
             FROM "GameInstances" instance
             JOIN "FlagContexts" flag ON flag.id = instance.flag_id
            WHERE instance.participation_id = $1
              AND instance.challenge_id = $2
            FOR SHARE OF instance, flag"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if own_flag.as_ref().is_some_and(|flag| ct_eq(flag, answer)) {
        return Ok((AnswerResult::Accepted, None));
    }
    if !detect_stolen {
        return Ok((AnswerResult::WrongAnswer, None));
    }

    let flag_map = load_challenge_flag_map(connection, challenge_id).await?;
    let source = flag_map
        .iter()
        .find(|(source_participation_id, flag)| {
            **source_participation_id != participation_id && ct_eq(flag, answer)
        })
        .map(|(source_participation_id, _)| *source_participation_id);
    let result = if source.is_some() {
        AnswerResult::CheatDetected
    } else {
        AnswerResult::WrongAnswer
    };
    Ok((result, source))
}

/// `POST /api/game/{id}/challenges/{challengeId}` — submit a flag.
///
/// RSCTF enqueues the submission onto a channel and a background `FlagChecker`
/// judges it. rsctf has no such worker, so the `VerifyAnswer` logic runs inline:
/// judge against the per-team dynamic flag or the challenge's static flag(s),
/// persist the graded submission, and on accept bump counts + record the
/// FirstSolve/blood order. Returns the new submission id (poll `status/{id}`).
#[allow(clippy::type_complexity)]
pub async fn submit(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::Json(model): axum::Json<FlagSubmitModel>,
) -> AppResult<RequestResponse<i32>> {
    let answer = model.flag.trim().to_string();
    if answer.is_empty() {
        return Err(AppError::bad_request("A flag is required"));
    }
    if answer.len() > MAX_FLAG_LENGTH {
        return Err(AppError::bad_request("Flag is too long"));
    }
    let submit_remote_ip_hash = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .and_then(|ip| {
            crate::services::anti_cheat::hash_ip_identity(st.config.as_ref(), &ip)
                .map(|identity| identity.exact)
        });

    let ctx = context_info(&st, &user, id, true).await?;

    let challenge = load_playable_challenge(&st, id, challenge_id).await?;

    // Division may restrict viewing/submitting this challenge (RSCTF Submit gate).
    let perm = effective_permission(&st, &ctx.participation, challenge_id).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE)
        || !perm.contains(GamePermission::SUBMIT_FLAGS)
    {
        return Err(AppError::Forbidden);
    }

    // Resolve the submitting team's name once (reused by the blood notice below).
    let team_name = team::Entity::find_by_id(ctx.participation.team_id)
        .one(&st.db)
        .await?
        .map(|t| t.name)
        .unwrap_or_default();

    // ------ Persist the grade, counters, first solve, and blood notice atomically ------
    // The pair advisory lock serializes one team's attempts at one challenge. The
    // submission-limit count and INSERT therefore share a transaction and cannot be
    // raced by parallel requests. It also makes the FirstSolve claim deterministic.
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    if !lock_submit_caller_at_grade(
        &mut transaction,
        user.id,
        &user.security_stamp,
        id,
        ctx.participation.team_id,
        ctx.participation.id,
    )
    .await?
    {
        return Err(AppError::Forbidden);
    }

    // Submissions share the engine/configuration fence for this game. Readers
    // remain fully concurrent, while an edit, repository import, or scoring
    // round that owns the exclusive form must land wholly before or after this
    // grading transaction. Acquire it before every narrower submission lock.
    crate::utils::single_flight::acquire_transaction_advisory_lock_shared(
        &mut transaction,
        &crate::services::ad_engine::game_lock_key(id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // A post-commit suspicion writer locks this participation's running score
    // before taking its game/challenge audit fences. Use the same outer lock
    // order here, ahead of submit's narrower pair lock. Without it, submissions
    // on different challenges can form an alternating four-transaction cycle:
    // submit holds participation -> detector holds challenge -> another submit
    // holds participation -> another detector holds challenge.
    crate::services::suspicion::lock_participation_suspicion_writes(
        &mut transaction,
        ctx.participation.id,
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(ctx.participation.id)
        .bind(challenge_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    // Read the authoritative grading policy after the per-team lock. Deliberately
    // do not lock the challenge row here: the late conditional counter UPDATE is
    // the policy fence, so unrelated teams can judge concurrently and hold the hot
    // row only for the final few statements of a successful transaction.
    let current: Option<(
        i32,
        Option<DateTime<Utc>>,
        bool,
        i16,
        Option<Uuid>,
        i16,
        i16,
    )> = sqlx::query_as(LOAD_GRADING_POLICY_SQL)
        .bind(challenge_id)
        .bind(id)
        .bind(ChallengeReviewStatus::Active as i16)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((
        submission_limit,
        current_deadline,
        disable_blood_bonus,
        challenge_type,
        shared_container_id,
        solve_receipt_mode,
        variant_mode,
    )) = current
    else {
        return Err(AppError::not_found("Challenge not found"));
    };

    // The cached play context is only an early gate. Hold a shared row lock on the
    // live game timing so practice/deadline/limit decisions cannot mix policies.
    let Some((game_start, game_end, practice_mode, freeze_time, submit_time)) =
        lock_game_timing_at_grade(&mut transaction, id).await?
    else {
        return Err(AppError::not_found("Game not found"));
    };
    if submit_time < game_start {
        return Err(AppError::game_not_started());
    }
    if !practice_mode && submit_time >= game_end {
        return Err(AppError::game_ended());
    }
    if current_deadline.is_some_and(|deadline| submit_time > deadline) && !practice_mode {
        return Err(AppError::bad_request("Challenge deadline has passed"));
    }
    // Live A&D flags belong to `/Ad/Submit`; KotH ownership is checker-driven.
    // Their GameInstance rows exist for service lifecycle, not as a back door into
    // Jeopardy FirstSolves/blood/tie-breaks. The sole normal-submit exception is
    // the documented post-game practice-container fallback.
    if !normal_flag_submit_type_allowed(challenge_type, practice_mode, submit_time, game_end) {
        return Err(AppError::bad_request(
            "This challenge uses its live scoring endpoint",
        ));
    }

    // Re-read permissions after acquiring the submission lock. Cache invalidation
    // handles normal edits; this live read closes the in-flight revoke race and also
    // keeps blood notices aligned with the board's fail-closed policy.
    let live_participation: Option<(i16, Option<i32>)> = sqlx::query_as(
        r#"SELECT status, division_id
             FROM "Participations"
            WHERE id = $1 AND game_id = $2
            FOR SHARE"#,
    )
    .bind(ctx.participation.id)
    .bind(id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (live_status, live_division_id) =
        live_participation.ok_or_else(|| AppError::bad_request("Participation not accepted"))?;
    if live_status != ParticipationStatus::Accepted as i16 {
        return Err(AppError::bad_request("Participation not accepted"));
    }

    let live_permissions = if let Some(division_id) = live_division_id {
        // Division mutations update the parent and all overrides in one transaction.
        // Holding FOR SHARE on that parent until this submission commits makes the
        // permission snapshot linearizable with a concurrent revoke.
        let stored: Option<i32> = sqlx::query_scalar(
            r#"SELECT COALESCE(permission.permissions, division.default_permissions)
                 FROM "Divisions" division
                 LEFT JOIN "DivisionChallengeConfigs" permission
                   ON permission.division_id = division.id
                  AND permission.challenge_id = $3
                WHERE division.id = $1 AND division.game_id = $2
                FOR SHARE OF division"#,
        )
        .bind(division_id)
        .bind(id)
        .bind(challenge_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        GamePermission(stored.unwrap_or(0))
    } else {
        GamePermission(GamePermission::ALL)
    };
    if !live_permissions.contains(GamePermission::VIEW_CHALLENGE)
        || !live_permissions.contains(GamePermission::SUBMIT_FLAGS)
    {
        return Err(AppError::Forbidden);
    }

    let solve_receipt_mode = match solve_receipt_mode {
        value if value == SolveReceiptMode::Disabled as i16 => SolveReceiptMode::Disabled,
        value if value == SolveReceiptMode::Optional as i16 => SolveReceiptMode::Optional,
        value if value == SolveReceiptMode::Required as i16 => SolveReceiptMode::Required,
        _ => return Err(AppError::internal("invalid solve receipt mode")),
    };
    let receipt = crate::services::event_security::validate_receipt_for_submission(
        &mut transaction,
        &st.config.event_vpn_credential_key,
        model.proof.as_deref(),
        solve_receipt_mode,
        id,
        challenge_id,
        ctx.participation.id,
        user.id,
        &answer,
    )
    .await?;

    let in_practice_phase = practice_mode && submit_time >= game_end;
    if submission_limit > 0 && !in_practice_phase {
        let attempts: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint FROM "Submissions"
                WHERE participation_id = $1 AND challenge_id = $2"#,
        )
        .bind(ctx.participation.id)
        .bind(challenge_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if attempts >= i64::from(submission_limit) {
            return Err(AppError::bad_request("Submission limit exceeded"));
        }
    }

    // ------ Authoritative grade (mirrors GameInstanceRepository.VerifyAnswer) ------
    // A shared challenge-scoped lock prevents a static FlagContext INSERT (which
    // has no pre-existing row to lock) from slipping between this read and commit.
    // Existing static flags and a dynamic instance/flag pair are row-locked too,
    // so deletes and per-team flag rotation linearize on the same grade.
    crate::utils::scoring::lock_jeopardy_flags_shared(&mut transaction, challenge_id).await?;
    let is_static = challenge_type == ChallengeType::StaticAttachment as i16
        || challenge_type == ChallengeType::StaticContainer as i16;
    let own_instance: Option<(Option<Uuid>, bool, DateTime<Utc>)> = sqlx::query_as(
        r#"SELECT container_id, is_loaded, last_container_operation
             FROM "GameInstances"
            WHERE participation_id = $1 AND challenge_id = $2
            FOR SHARE"#,
    )
    .bind(ctx.participation.id)
    .bind(challenge_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let (submit_container_id, container_was_loaded_at_submit, container_last_operation_at_submit) =
        match own_instance {
            Some((container_id, was_loaded, last_operation)) => (
                container_id.or(shared_container_id),
                Some(was_loaded),
                Some(last_operation),
            ),
            None => (shared_container_id, None, None),
        };
    let (first_open_at_submit, first_download_at_submit, first_container_start_at_submit) =
        load_first_positive_interactions(
            &mut transaction,
            id,
            ctx.participation.team_id,
            challenge_id,
            game_start,
            game_end,
            submit_time,
        )
        .await?;
    let (mut result, cheat_source_participation_id) =
        if variant_mode == ChallengeVariantMode::PerParticipation as i16 {
            grade_variant_answer(
                &mut transaction,
                id,
                challenge_id,
                ctx.participation.id,
                &answer,
            )
            .await?
        } else if variant_mode == ChallengeVariantMode::Disabled as i16 && is_static {
            let flags: Vec<String> = sqlx::query_scalar(
                r#"SELECT flag
                 FROM "FlagContexts"
                WHERE challenge_id = $1
                FOR SHARE"#,
            )
            .bind(challenge_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            if flags.iter().any(|flag| ct_eq(flag, &answer)) {
                (AnswerResult::Accepted, None)
            } else {
                (AnswerResult::WrongAnswer, None)
            }
        } else if variant_mode == ChallengeVariantMode::Disabled as i16 {
            let (grade, source_participation_id) = grade_dynamic_answer(
                &mut transaction,
                ctx.participation.id,
                challenge_id,
                &answer,
                submit_time < game_end,
            )
            .await?;
            (grade, source_participation_id)
        } else {
            return Err(AppError::internal("invalid challenge variant mode"));
        };

    // ------ Stolen-flag (cheat) detection ------
    // Always scan the transactionally locked, authoritative map. In particular,
    // a team without its own instance can still present another team's valid
    // flag; returning early here would erase the strongest evidence we have.
    let cheat_source = if let Some(source_participation_id) = cheat_source_participation_id {
        let source: Option<(i32, String)> = sqlx::query_as(
            r#"SELECT participation.team_id, team.name
                 FROM "Participations" participation
                 JOIN "Teams" team ON team.id = participation.team_id
                WHERE participation.id = $1 AND participation.game_id = $2
                FOR SHARE OF participation, team"#,
        )
        .bind(source_participation_id)
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        match source {
            Some((source_team_id, source_team_name)) => {
                Some((source_participation_id, source_team_id, source_team_name))
            }
            None => {
                result = AnswerResult::WrongAnswer;
                None
            }
        }
    } else {
        None
    };

    let sub_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Submissions"
             (answer, status, submit_time_utc, user_id, team_id,
              participation_id, game_id, challenge_id,
              submit_remote_ip_hash, container_id,
              container_last_operation_at_submit,
              container_was_loaded_at_submit, first_open_at_submit,
              first_download_at_submit, first_container_start_at_submit)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                   $13, $14, $15)
           RETURNING id"#,
    )
    .bind(&answer)
    .bind(result as i16)
    .bind(submit_time)
    .bind(user.id)
    .bind(ctx.participation.team_id)
    .bind(ctx.participation.id)
    .bind(id)
    .bind(challenge_id)
    .bind(submit_remote_ip_hash.as_deref())
    .bind(submit_container_id)
    .bind(container_last_operation_at_submit)
    .bind(container_was_loaded_at_submit)
    .bind(first_open_at_submit)
    .bind(first_download_at_submit)
    .bind(first_container_start_at_submit)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    if let Some(receipt) = receipt {
        crate::services::event_security::consume_receipt(&mut transaction, receipt, sub_id).await?;
    }

    // Canonical stolen-flag provenance is committed with the grade. Unlike the
    // presentation event, this row preserves both participation identities and
    // stable display snapshots, and cannot be updated or deleted afterward.
    if let Some((source_participation_id, source_team_id, source_team_name)) = cheat_source.as_ref()
    {
        let evidence_key = format!("submission:{sub_id}");
        // The database trigger discards this placeholder and builds the v1
        // display snapshot from rows locked in this grading transaction.
        let evidence_payload = serde_json::json!({});
        sqlx::query(
            r#"INSERT INTO "CheatInfo"
                 (game_id, submit_team_id, source_team_id, submission_id,
                  submit_participation_id, source_participation_id, challenge_id,
                  evidence_key, observed_at_utc, evidence_payload, evidence_version)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 1)"#,
        )
        .bind(id)
        .bind(ctx.participation.team_id)
        .bind(*source_team_id)
        .bind(sub_id)
        .bind(ctx.participation.id)
        .bind(*source_participation_id)
        .bind(challenge_id)
        .bind(evidence_key)
        .bind(submit_time)
        .bind(sqlx::types::Json(&evidence_payload))
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

        let values = serde_json::json!([challenge.title, team_name, source_team_name,]);
        sqlx::query(
            r#"INSERT INTO "GameEvents"
                 (game_id, "Type", "values", publish_time_utc, user_id, team_id)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
        )
        .bind(id)
        .bind(crate::utils::enums::EventType::CheatDetected as i16)
        .bind(sqlx::types::Json(&values))
        .bind(submit_time)
        .bind(user.id)
        .bind(ctx.participation.team_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    // The submission and its replay intent are one commit. Control may crash at
    // any point afterward; the leased reconciler will resume this exact source.
    let evaluation_is_durable = crate::services::suspicion::enqueue_submission_evaluation(
        &mut transaction,
        sub_id,
        id,
        ctx.participation.id,
        challenge_id,
        submit_time,
    )
    .await?;
    if !evaluation_is_durable {
        return Err(AppError::internal(
            "submission evaluation provenance is inconsistent",
        ));
    }

    let mut notice_to_broadcast: Option<(NoticeType, i32, Json, DateTime<Utc>)> = None;
    let mut claimed_first_solve = false;
    if result == AnswerResult::Accepted {
        let already_solved: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(
                 SELECT 1 FROM "FirstSolves"
                  WHERE participation_id = $1 AND challenge_id = $2
               )"#,
        )
        .bind(ctx.participation.id)
        .bind(challenge_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

        if !already_solved {
            let blood_eligible = submit_time >= game_start
                && submit_time < game_end
                && current_deadline.is_none_or(|deadline| submit_time <= deadline)
                && !disable_blood_bonus
                && live_permissions.contains(GamePermission::GET_BLOOD)
                && live_permissions.contains(GamePermission::GET_SCORE);

            // Serialize only the rare first-three eligible solves globally for this
            // challenge. The per-team lock is always acquired first, so lock order is
            // consistent and cannot deadlock between submitters.
            let prior = if blood_eligible {
                let observed = count_blood_eligible_solves(
                    &mut transaction,
                    id,
                    challenge_id,
                    game_start,
                    game_end,
                    current_deadline,
                )
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
                if observed < 3 {
                    sqlx::query("SELECT pg_advisory_xact_lock(0, $1)")
                        .bind(challenge_id)
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| AppError::internal(error.to_string()))?;
                    count_blood_eligible_solves(
                        &mut transaction,
                        id,
                        challenge_id,
                        game_start,
                        game_end,
                        current_deadline,
                    )
                    .await
                    .map_err(|error| AppError::internal(error.to_string()))?
                } else {
                    observed
                }
            } else {
                3
            };

            claimed_first_solve =
                claim_first_solve(&mut transaction, ctx.participation.id, challenge_id, sub_id)
                    .await?;

            let notice_type = if claimed_first_solve && blood_eligible {
                match prior {
                    0 => Some(NoticeType::FirstBlood),
                    1 => Some(NoticeType::SecondBlood),
                    2 => Some(NoticeType::ThirdBlood),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(notice_type) = notice_type {
                let values = serde_json::json!([team_name, challenge.title]);
                let publish_time = Utc::now();
                let notice_id: i32 = sqlx::query_scalar(
                    r#"INSERT INTO "GameNotices"
                         (game_id, "Type", "values", publish_time_utc)
                       VALUES ($1, $2, $3, $4)
                       RETURNING id"#,
                )
                .bind(id)
                .bind(notice_type as i16)
                .bind(sqlx::types::Json(&values))
                .bind(publish_time)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
                notice_to_broadcast = Some((notice_type, notice_id, values, publish_time));
            }
        }
    }

    // Finalize against exactly the challenge policy used for authorization and
    // grading. If an organizer committed a deadline/limit/blood/type/visibility
    // edit while this transaction was in flight, the predicate matches no row and
    // the whole submission (including a tentative FirstSolve/notice) rolls back.
    // This is intentionally the first write lock on GameChallenges and is placed
    // immediately before commit to avoid serializing the longer grading path.
    let accepted_inc = i32::from(claimed_first_solve);
    let counter_update = sqlx::query(FINALIZE_SUBMISSION_SQL)
        .bind(challenge_id)
        .bind(accepted_inc)
        .bind(id)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(submission_limit)
        .bind(current_deadline)
        .bind(disable_blood_bonus)
        .bind(challenge_type)
        .bind(shared_container_id)
        .bind(solve_receipt_mode as i16)
        .bind(variant_mode)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if counter_update.rows_affected() != 1 {
        return Err(AppError::bad_request(
            "Challenge policy changed; please submit again",
        ));
    }

    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    st.publish_event(
        "ReceivedSubmissions",
        Some(id),
        serde_json::json!({
            "answer": answer,
            "status": result,
            "time": submit_time,
            "user": user.name,
            "team": team_name,
            "challenge": challenge.title,
        })
        .to_string(),
    );

    if let Some((notice_type, notice_id, values, publish_time)) = notice_to_broadcast {
        let broadcast_now = Utc::now();
        let in_freeze =
            freeze_time.is_some_and(|freeze| broadcast_now >= freeze && broadcast_now < game_end);
        if !in_freeze {
            st.publish_event(
                "ReceivedGameNotice",
                Some(id),
                serde_json::json!({
                    "type": notice_type,
                    "values": values,
                    "id": notice_id,
                    "time": publish_time,
                })
                .to_string(),
            );
        }
    }

    Ok(RequestResponse::ok(sub_id))
}

#[cfg(test)]
mod tests {
    use super::{
        normal_flag_submit_type_allowed, ChallengeType, FINALIZE_SUBMISSION_SQL,
        LOAD_GRADING_POLICY_SQL,
    };
    use chrono::{Duration, Utc};

    #[test]
    fn challenge_policy_read_does_not_hold_the_hot_row() {
        assert!(
            !LOAD_GRADING_POLICY_SQL.contains("FOR UPDATE"),
            "authoritative policy reads must rely on the late optimistic fence"
        );
    }

    #[test]
    fn finalization_fences_every_authoritative_challenge_input() {
        for predicate in [
            "AND game_id = $3",
            "AND is_enabled",
            "AND review_status = $4",
            "AND submission_limit = $5",
            "AND deadline_utc IS NOT DISTINCT FROM $6",
            "AND disable_blood_bonus = $7",
            "AND \"Type\" = $8",
        ] {
            assert!(
                FINALIZE_SUBMISSION_SQL.contains(predicate),
                "missing optimistic grading fence predicate: {predicate}"
            );
        }
    }

    #[test]
    fn live_engine_types_cannot_enter_jeopardy_scoring() {
        let end = Utc::now() + Duration::hours(1);
        let live = end - Duration::minutes(30);
        for challenge_type in [
            ChallengeType::StaticAttachment,
            ChallengeType::StaticContainer,
            ChallengeType::DynamicAttachment,
            ChallengeType::DynamicContainer,
        ] {
            assert!(normal_flag_submit_type_allowed(
                challenge_type as i16,
                false,
                live,
                end
            ));
        }
        for challenge_type in [ChallengeType::AttackDefense, ChallengeType::KingOfTheHill] {
            assert!(!normal_flag_submit_type_allowed(
                challenge_type as i16,
                false,
                live,
                end
            ));
            assert!(!normal_flag_submit_type_allowed(
                challenge_type as i16,
                true,
                live,
                end
            ));
        }
    }

    #[test]
    fn post_game_practice_keeps_the_normal_container_fallback() {
        let end = Utc::now();
        let after_end = end + Duration::seconds(1);
        for challenge_type in [ChallengeType::AttackDefense, ChallengeType::KingOfTheHill] {
            assert!(!normal_flag_submit_type_allowed(
                challenge_type as i16,
                false,
                after_end,
                end
            ));
            assert!(normal_flag_submit_type_allowed(
                challenge_type as i16,
                true,
                after_end,
                end
            ));
        }
        assert!(!normal_flag_submit_type_allowed(
            i16::MAX,
            true,
            after_end,
            end
        ));
    }
}

#[cfg(test)]
#[path = "submit_evidence_tests.rs"]
mod evidence_tests;
