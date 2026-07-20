//! Player flag submission, challenge review, and submission status —
//! split from play.rs to keep each file under the 1000-line rule.
use super::*;

const LOAD_GRADING_POLICY_SQL: &str = r#"
    SELECT submission_limit, deadline_utc, disable_blood_bonus, "Type"
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
"#;

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

/// Cache-only prefetch for the best-effort stolen-flag scan. This runs before the
/// submission transaction is opened, so a Redis/cache lookup never lengthens a DB
/// transaction or attempts to lease a second PostgreSQL connection.
async fn cached_challenge_flag_map(
    st: &SharedState,
    challenge_id: i32,
) -> Option<std::collections::HashMap<i32, String>> {
    let key = format!("_ChalFlagMap_{challenge_id}");
    if let Some(bytes) = st.cache.get(&key).await {
        if let Ok(m) = serde_json::from_slice::<std::collections::HashMap<i32, String>>(&bytes) {
            return Some(m);
        }
    }
    None
}

/// Cache-miss loader that deliberately uses the caller's existing transaction
/// connection. Leasing `st.db` here while every pool connection is already held
/// by a concurrent submit would deadlock the pool.
async fn load_challenge_flag_map(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
) -> AppResult<std::collections::HashMap<i32, String>> {
    let rows: Vec<(i32, String)> = sqlx::query_as(
        r#"SELECT instance.participation_id, flag.flag
             FROM "GameInstances" instance
             JOIN "FlagContexts" flag ON flag.id = instance.flag_id
            WHERE instance.challenge_id = $1"#,
    )
    .bind(challenge_id)
    .fetch_all(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows.into_iter().collect())
}

async fn cache_challenge_flag_map(
    st: &SharedState,
    challenge_id: i32,
    map: &std::collections::HashMap<i32, String>,
) {
    if let Ok(json) = serde_json::to_vec(&map) {
        let key = format!("_ChalFlagMap_{challenge_id}");
        st.cache
            .set(&key, &json, Some(std::time::Duration::from_secs(5)))
            .await;
    }
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
    axum::Json(model): axum::Json<FlagSubmitModel>,
) -> AppResult<RequestResponse<i32>> {
    let submit_time = Utc::now();
    let answer = model.flag.trim().to_string();
    if answer.is_empty() {
        return Err(AppError::bad_request("A flag is required"));
    }
    if answer.len() > MAX_FLAG_LENGTH {
        return Err(AppError::bad_request("Flag is too long"));
    }

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
    let mut cheat_flag_map = cached_challenge_flag_map(&st, challenge_id).await;
    let mut cache_loaded_cheat_flags = false;

    // ------ Persist the grade, counters, first solve, and blood notice atomically ------
    // The pair advisory lock serializes one team's attempts at one challenge. The
    // submission-limit count and INSERT therefore share a transaction and cannot be
    // raced by parallel requests. It also makes the FirstSolve claim deterministic.
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
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
    let current: Option<(i32, Option<DateTime<Utc>>, bool, i16)> =
        sqlx::query_as(LOAD_GRADING_POLICY_SQL)
            .bind(challenge_id)
            .bind(id)
            .bind(ChallengeReviewStatus::Active as i16)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((submission_limit, current_deadline, disable_blood_bonus, challenge_type)) = current
    else {
        return Err(AppError::not_found("Challenge not found"));
    };

    // The cached play context is only an early gate. Hold a shared row lock on the
    // live game timing so practice/deadline/limit decisions cannot mix policies.
    let timing: Option<(DateTime<Utc>, DateTime<Utc>, bool, Option<DateTime<Utc>>)> =
        sqlx::query_as(
            r#"SELECT start_time_utc, end_time_utc, practice_mode, freeze_time_utc
             FROM "Games" WHERE id = $1 FOR SHARE"#,
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((game_start, game_end, practice_mode, freeze_time)) = timing else {
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
    let mut result = if is_static {
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
            AnswerResult::Accepted
        } else {
            AnswerResult::WrongAnswer
        }
    } else {
        let flag: Option<String> = sqlx::query_scalar(
            r#"SELECT flag.flag
                 FROM "GameInstances" instance
                 JOIN "FlagContexts" flag ON flag.id = instance.flag_id
                WHERE instance.participation_id = $1
                  AND instance.challenge_id = $2
                FOR SHARE OF instance, flag"#,
        )
        .bind(ctx.participation.id)
        .bind(challenge_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let flag = flag.ok_or_else(|| AppError::not_found("Challenge not found"))?;
        if ct_eq(&flag, &answer) {
            AnswerResult::Accepted
        } else {
            AnswerResult::WrongAnswer
        }
    };

    // ------ Stolen-flag (cheat) detection ------
    // This classification is best-effort and may use the short-lived all-team
    // cache, but it can no longer influence whether an answer is accepted: the
    // canonical grade above came from transactionally locked rows.
    let mut cheat_source_team: Option<String> = None;
    if result == AnswerResult::WrongAnswer && submit_time < game_end {
        if cheat_flag_map.is_none() {
            cheat_flag_map = Some(load_challenge_flag_map(&mut transaction, challenge_id).await?);
            cache_loaded_cheat_flags = true;
        }
        let flag_map = cheat_flag_map
            .as_ref()
            .expect("cache miss was loaded on the transaction connection");
        for (&pid, flag) in flag_map.iter() {
            if pid != ctx.participation.id && ct_eq(flag, &answer) {
                result = AnswerResult::CheatDetected;
                cheat_source_team = sqlx::query_scalar(
                    r#"SELECT team.name
                         FROM "Participations" participation
                         JOIN "Teams" team ON team.id = participation.team_id
                        WHERE participation.id = $1"#,
                )
                .bind(pid)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
                break;
            }
        }
    }

    let sub_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Submissions"
             (answer, status, submit_time_utc, user_id, team_id,
              participation_id, game_id, challenge_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
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
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // Persist the audit event in the same transaction as the triggering submission.
    if result == AnswerResult::CheatDetected {
        let values = serde_json::json!([
            challenge.title,
            team_name,
            cheat_source_team.clone().unwrap_or_default(),
        ]);
        sqlx::query(
            r#"INSERT INTO "GameEvents"
                 (game_id, "Type", "values", publish_time_utc, user_id, team_id)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
        )
        .bind(id)
        .bind(crate::utils::enums::EventType::CheatDetected as i16)
        .bind(sqlx::types::Json(&values))
        .bind(Utc::now())
        .bind(user.id)
        .bind(ctx.participation.team_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    let mut notice_to_broadcast: Option<(NoticeType, i32, Json, DateTime<Utc>)> = None;
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

            let claimed = sqlx::query_scalar::<_, i32>(
                r#"INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
               VALUES ($1, $2, $3)
               ON CONFLICT (participation_id, challenge_id) DO NOTHING
               RETURNING submission_id"#,
            )
            .bind(ctx.participation.id)
            .bind(challenge_id)
            .bind(sub_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .is_some();

            let notice_type = if claimed && blood_eligible {
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
    let accepted_inc = i32::from(result == AnswerResult::Accepted);
    let counter_update = sqlx::query(FINALIZE_SUBMISSION_SQL)
        .bind(challenge_id)
        .bind(accepted_inc)
        .bind(id)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(submission_limit)
        .bind(current_deadline)
        .bind(disable_blood_bonus)
        .bind(challenge_type)
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

    if cache_loaded_cheat_flags {
        if let Some(map) = cheat_flag_map.as_ref() {
            cache_challenge_flag_map(&st, challenge_id, map).await;
        }
    }

    // Best-effort suspicion detection runs only after the canonical submission is
    // committed. It is intentionally not allowed to roll back a player's answer.
    if let Err(error) = crate::services::suspicion::evaluate_submission(
        &st.db,
        id,
        ctx.participation.id,
        sub_id,
        &challenge,
        &answer,
    )
    .await
    {
        tracing::warn!(
            game = id,
            participation = ctx.participation.id,
            submission = sub_id,
            %error,
            "post-submit suspicion evaluation failed"
        );
    }

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

/// `POST /api/game/{id}/challenges/{challengeId}/review` — rate a solved challenge.
///
/// Mirrors RSCTF `ReviewChallenge` + `ChallengeReviewRepository.AddOrUpdateReviewAsync`:
/// the caller must be an accepted participant who has solved the challenge, then a
/// `ChallengeReviews` row (keyed on user+challenge) is inserted or updated in place.
pub async fn review_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
    axum::Json(model): axum::Json<ChallengeReviewModel>,
) -> AppResult<MessageResponse> {
    use sea_orm::ActiveEnum;

    let ctx = context_info(&st, &user, id, false).await?;

    // The challenge must belong to this game.
    let _challenge = load_scoped_challenge(&st, id, challenge_id).await?;

    // RSCTF requires the caller's team to have solved the challenge first.
    let solved = submission::Entity::find()
        .filter(submission::Column::ParticipationId.eq(ctx.participation.id))
        .filter(submission::Column::ChallengeId.eq(challenge_id))
        .filter(submission::Column::Status.eq(AnswerResult::Accepted))
        .one(&st.db)
        .await?
        .is_some();
    if !solved {
        return Err(AppError::bad_request("You must solve the challenge first."));
    }

    // Map the wire rating (numeric ReviewRating) onto the stored enum.
    let rating = ReviewRating::try_from_value(&(model.rating.unwrap_or(0) as i16))
        .unwrap_or(ReviewRating::None);
    let comment = model.comment.clone().filter(|c| !c.is_empty());

    // Upsert on (user, challenge): update in place if one exists, else insert.
    let existing = challenge_review::Entity::find()
        .filter(challenge_review::Column::UserId.eq(user.id))
        .filter(challenge_review::Column::ChallengeId.eq(challenge_id))
        .one(&st.db)
        .await?;
    match existing {
        Some(row) => {
            let mut am: challenge_review::ActiveModel = row.into();
            am.rating = Set(rating);
            am.comment = Set(comment);
            am.submit_time_utc = Set(Utc::now());
            am.update(&st.db).await?;
        }
        None => {
            challenge_review::ActiveModel {
                challenge_id: Set(challenge_id),
                user_id: Set(user.id),
                game_id: Set(id),
                rating: Set(rating),
                comment: Set(comment),
                submit_time_utc: Set(Utc::now()),
                ..Default::default()
            }
            .insert(&st.db)
            .await?;
        }
    }

    Ok(MessageResponse::ok(""))
}

/// `GET /api/game/{id}/challenges/{challengeId}/status/{submitId}`
pub async fn status(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id, submit_id)): Path<(i32, i32, i32)>,
) -> AppResult<RequestResponse<AnswerResult>> {
    let sub = submission::Entity::find_by_id(submit_id)
        .one(&st.db)
        .await?
        .filter(|s| s.game_id == id && s.challenge_id == challenge_id && s.user_id == Some(user.id))
        .ok_or_else(|| AppError::not_found("Submission not found"))?;

    // Never reveal cheat detection to the player.
    let visible = match sub.status {
        AnswerResult::CheatDetected => AnswerResult::WrongAnswer,
        other => other,
    };
    Ok(RequestResponse::ok(visible))
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
