//! `Ad/Submit` endpoint — batch flag submission and evidence adjudication.

use super::*;
use sea_orm::PaginatorTrait;

/// RSCTF `Game.AdFlagLifetimeTicks` fallback — a flag planted in round `N` stays
/// submittable while the live round number is `< N + lifetime`. Used only when
/// the game row leaves `ad_flag_lifetime_ticks` null.
const AD_FLAG_LIFETIME_TICKS_DEFAULT: i32 = 5;

/// Max flags accepted in one batch submit (RSCTF bounds this server-side).
const AD_MAX_BATCH: usize = 100;

/// Body for `POST /api/Game/{id}/Ad/Submit` (`AdBatchSubmitModel`).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdBatchSubmitModel {
    #[serde(default)]
    pub flags: Vec<String>,
}

/// `AdSubmitResultModel` — per-flag result row (echoed in input order).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdSubmitResultModel {
    pub flag: String,
    /// `accepted | duplicate | wrong | expired | self_attack | not_started |
    /// ended | paused | rejected`.
    pub status: String,
    pub flag_planted_at_round: Option<i32>,
    pub message: Option<String>,
}

/// `AdBatchSubmitResultModel` — `POST Ad/Submit` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdBatchSubmitResultModel {
    pub accepted_count: i32,
    pub results: Vec<AdSubmitResultModel>,
}

/// `POST /api/Game/{id}/Ad/Submit` — batch-submit flags captured from other
/// teams. Each flag is matched against a currently-valid `ad_flag` not owned by
/// the submitter; a hit records a deduplicated `ad_attack`.
pub async fn submit(
    State(st): State<SharedState>,
    maybe_user: MaybeUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
    verified: Option<axum::Extension<crate::services::ad::api_token::VerifiedTeamToken>>,
    rejected: Option<axum::Extension<crate::services::ad::api_token::RejectedTeamToken>>,
    axum::Json(model): axum::Json<AdBatchSubmitModel>,
) -> AppResult<RequestResponse<AdBatchSubmitResultModel>> {
    // RSCTF `AdBatchSubmitModel.Flags` is `[MinLength(1)][MaxLength(100)]`, so
    // `SubmitBatch` 400s on an empty or over-length batch before any work — never
    // silently truncating. Gate here so a 150-flag submit is rejected whole rather
    // than dropping the tail 50 captures under a partial success.
    if model.flags.is_empty() || model.flags.len() > AD_MAX_BATCH {
        return Err(AppError::bad_request(
            "Flags must contain between 1 and 100 entries",
        ));
    }

    // Token-first (scripted exploits via `Authorization: Bearer ad_...`), then the
    // interactive session — mirrors RSCTF's `ResolveTeamApiTokenAsync ?? ResolveUser…`.
    let attacker = resolve_ad_attacker(
        &st,
        &headers,
        verified.as_ref().map(|extension| &extension.0),
        rejected.as_ref().map(|extension| &extension.0),
        maybe_user,
        id,
    )
    .await?;
    let game = game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;

    // Submissions are only accepted inside the game window. Round-number expiry
    // alone doesn't close the door at game end (the scheduler freezes the round
    // number), so flags from the last lifetime window would otherwise stay
    // submittable forever and mutate final standings. Mirrors RSCTF `SubmitBatch`.
    let now = Utc::now();
    if now < game.start_time_utc || now >= game.end_time_utc {
        let (status, message) = if now < game.start_time_utc {
            ("not_started", "the game has not started yet")
        } else {
            ("ended", "the game has ended — submissions are closed")
        };
        let results = model
            .flags
            .into_iter()
            .map(|f| AdSubmitResultModel {
                flag: f,
                status: status.to_string(),
                flag_planted_at_round: None,
                message: Some(message.to_string()),
            })
            .collect();
        return Ok(RequestResponse::ok(AdBatchSubmitResultModel {
            accepted_count: 0,
            results,
        }));
    }

    // Scoring pause halts the round scheduler + checker (no new flags, no SLA
    // accrual); captures must freeze too, or attack points keep accruing while
    // the rest of the board is paused. Records nothing, accepts nothing.
    if game.ad_scoring_paused {
        let results = model
            .flags
            .into_iter()
            .map(|f| AdSubmitResultModel {
                flag: f,
                status: "paused".to_string(),
                flag_planted_at_round: None,
                message: Some("scoring is paused — submissions are not being recorded".to_string()),
            })
            .collect();
        return Ok(RequestResponse::ok(AdBatchSubmitResultModel {
            accepted_count: 0,
            results,
        }));
    }

    let lifetime_ticks = game
        .ad_flag_lifetime_ticks
        .unwrap_or(AD_FLAG_LIFETIME_TICKS_DEFAULT)
        .clamp(1, 50);

    let current_round = ad_round::Entity::find()
        .filter(ad_round::Column::GameId.eq(id))
        .order_by_desc(ad_round::Column::Number)
        .one(&st.db)
        .await?;
    let round_number = round_number_map(&st, id).await?;

    let mut results = Vec::new();
    let mut accepted_count = 0i32;
    let submit_context = SubmitContext {
        st: &st,
        game_id: id,
        attacker: &attacker,
        current_round: current_round.as_ref(),
        round_number: &round_number,
        lifetime_ticks,
    };

    // Length already gated above (1..=AD_MAX_BATCH), so process the whole batch.
    for raw in model.flags.into_iter() {
        let flag_text = raw.trim().to_string();
        let (status, planted) = submit_one(&submit_context, &flag_text).await?;
        if status == "accepted" {
            accepted_count += 1;
        }
        results.push(AdSubmitResultModel {
            flag: raw,
            status: status.to_string(),
            flag_planted_at_round: planted,
            message: None,
        });
    }

    // A capture changes both role-stable official cache entries while the
    // public view is live. Once frozen, evicting it rebuilds the same cutoff.
    if accepted_count > 0 {
        for k in [
            format!("_AdScoreBoard_{id}"),
            format!("_AdScoreBoardFrozen_{id}"),
        ] {
            st.cache.remove(&k).await;
        }
    }

    Ok(RequestResponse::ok(AdBatchSubmitResultModel {
        accepted_count,
        results,
    }))
}

struct SubmitContext<'a> {
    st: &'a SharedState,
    game_id: i32,
    attacker: &'a participation::Model,
    current_round: Option<&'a ad_round::Model>,
    round_number: &'a HashMap<i32, i32>,
    lifetime_ticks: i32,
}

/// Adjudicate one flag and return its status plus planted round. Captures are
/// priced only when their epoch is scored, after rarity is known.
async fn submit_one(
    context: &SubmitContext<'_>,
    flag_text: &str,
) -> AppResult<(&'static str, Option<i32>)> {
    let st = context.st;
    let game_id = context.game_id;
    let attacker = context.attacker;
    let current_round = context.current_round;
    let round_number = context.round_number;
    let lifetime_ticks = context.lifetime_ticks;
    let Some(current_round) = current_round else {
        return Ok(("not_started", None));
    };
    if flag_text.is_empty() {
        return Ok(("wrong", None));
    }

    let Some(flag) = ad_flag::Entity::find()
        .filter(ad_flag::Column::Flag.eq(flag_text.to_string()))
        .order_by_desc(ad_flag::Column::Id)
        .one(&st.db)
        .await?
    else {
        return Ok(("wrong", None));
    };
    let planted = round_number.get(&flag.round_id).copied();

    let Some(victim_service) = ad_team_service::Entity::find_by_id(flag.team_service_id)
        .one(&st.db)
        .await?
        .filter(|s| s.game_id == game_id)
    else {
        return Ok(("wrong", planted));
    };
    if !crate::services::ad_engine::active_ad_services(&st.db, game_id)
        .await?
        .iter()
        .any(|service| service.id == victim_service.id)
    {
        return Ok(("wrong", planted));
    }

    if victim_service.participation_id == attacker.id {
        return Ok(("self_attack", planted));
    }

    if let Some(planted) = planted {
        if planted < current_round.number - lifetime_ticks + 1 {
            return Ok(("expired", Some(planted)));
        }
    }

    // Dedup an accepted capture on (attacker, flag) — RSCTF's unique key. A
    // distinct still-valid flag from the same victim service still scores, and
    // re-submitting the *same* flag (even in a later round) is a duplicate.
    // Atomic dedup on (attacker, flag) via ux_adattacks_attacker_flag: INSERT … ON
    // CONFLICT DO NOTHING RETURNING id. N concurrent identical-flag submits race into
    // one INSERT — only the winner gets a row back; the losers see None and are treated
    // as duplicates, so a capture can never be double-inserted (#14).
    let inserted: Option<(i32, bool)> = sqlx::query_as(
        r#"WITH latest_round AS (
               SELECT id, number
                 FROM "AdRounds"
                WHERE game_id = $4 AND finalized = FALSE
                ORDER BY number DESC
                LIMIT 1
                FOR SHARE
           ), inserted AS (
             INSERT INTO "AdAttacks"
               (round_id, attacker_participation_id, victim_team_service_id, flag_id, submitted_at)
             SELECT latest.id, attacker.id, service.id, planted.id, now()
             FROM "Participations" attacker
             JOIN "AdTeamServices" service ON service.id = $2
             JOIN "Participations" victim
               ON victim.id = service.participation_id
              AND victim.game_id = service.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
             JOIN "Games" game ON game.id = service.game_id
             JOIN "AdFlags" planted
               ON planted.id = $3 AND planted.team_service_id = service.id
             JOIN "AdRounds" planted_round
               ON planted_round.id = planted.round_id
              AND planted_round.game_id = game.id
             JOIN latest_round latest ON TRUE
            WHERE attacker.id = $1
              AND attacker.game_id = $4
              AND attacker.status = $5
              AND service.game_id = $4
              AND victim.status = $5
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $6
              AND challenge."Type" = $7
              AND game.start_time_utc <= now()
              AND now() < game.end_time_utc
              AND game.ad_scoring_paused = FALSE
              AND planted_round.number >= latest.number
                    - LEAST(
                        50,
                        GREATEST(COALESCE(game.ad_flag_lifetime_ticks, $8), 1)
                      ) + 1
             ON CONFLICT (attacker_participation_id, flag_id) DO NOTHING
             RETURNING id
           )
           SELECT inserted.id,
                  NOT game.hidden
                  AND NOT (
                    game.freeze_time_utc IS NOT NULL
                    AND now() >= game.freeze_time_utc
                    AND now() < game.end_time_utc
                  )
             FROM inserted
             JOIN "Games" game ON game.id = $4"#,
    )
    .bind(attacker.id)
    .bind(victim_service.id)
    .bind(flag.id)
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .bind(AD_FLAG_LIFETIME_TICKS_DEFAULT)
    .fetch_optional(st.pg())
    .await
    .map_err(|e| crate::utils::error::AppError::internal(e.to_string()))?;
    let Some((_attack_id, broadcast_ok)) = inserted else {
        // ON CONFLICT and every eligibility predicate share the same zero-row
        // shape. Re-read authoritative state so a pause/end/rejection race is not
        // falsely reported as an already-scored duplicate.
        let rejection: String = sqlx::query_scalar(
            r#"SELECT CASE
                 WHEN clock_timestamp() < game.start_time_utc THEN 'not_started'
                 WHEN clock_timestamp() >= game.end_time_utc THEN 'ended'
                 WHEN game.ad_scoring_paused THEN 'paused'
                 WHEN NOT EXISTS (
                   SELECT 1
                     FROM "Participations" attacker
                     JOIN "AdTeamServices" service ON service.id = $2
                     JOIN "Participations" victim
                       ON victim.id = service.participation_id
                      AND victim.game_id = service.game_id
                     JOIN "GameChallenges" challenge
                       ON challenge.id = service.challenge_id
                      AND challenge.game_id = service.game_id
                     JOIN "AdFlags" planted
                       ON planted.id = $3
                      AND planted.team_service_id = service.id
                    WHERE attacker.id = $1
                      AND attacker.game_id = $4
                      AND attacker.status = $5
                      AND service.game_id = $4
                      AND victim.status = $5
                      AND challenge.is_enabled = TRUE
                      AND challenge.review_status = $6
                      AND challenge."Type" = $7
                 ) THEN 'rejected'
                 WHEN NOT EXISTS (
                   SELECT 1 FROM "AdRounds"
                    WHERE game_id = $4 AND finalized = FALSE
                 ) THEN 'not_started'
                 WHEN EXISTS (
                   SELECT 1
                     FROM "AdFlags" planted
                     JOIN "AdRounds" planted_round ON planted_round.id = planted.round_id
                     CROSS JOIN LATERAL (
                       SELECT number FROM "AdRounds"
                        WHERE game_id = $4 AND finalized = FALSE
                        ORDER BY number DESC LIMIT 1
                     ) latest
                    WHERE planted.id = $3
                      AND planted_round.number < latest.number
                        - LEAST(
                            50,
                            GREATEST(COALESCE(game.ad_flag_lifetime_ticks, $8), 1)
                          ) + 1
                 ) THEN 'expired'
                 WHEN EXISTS (
                   SELECT 1 FROM "AdAttacks"
                    WHERE attacker_participation_id = $1 AND flag_id = $3
                 ) THEN 'duplicate'
                 ELSE 'rejected'
               END
               FROM "Games" game
              WHERE game.id = $4"#,
        )
        .bind(attacker.id)
        .bind(victim_service.id)
        .bind(flag.id)
        .bind(game_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(AD_FLAG_LIFETIME_TICKS_DEFAULT)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .unwrap_or_else(|| "rejected".to_string());
        let status = match rejection.as_str() {
            "duplicate" => "duplicate",
            "expired" => "expired",
            "not_started" => "not_started",
            "ended" => "ended",
            "paused" => "paused",
            _ => "rejected",
        };
        return Ok((status, planted));
    };

    // Real-time attack feed (raw WS `/hub/attack/ws`). Emit the client's
    // AttackEvent shape (team names + challenge title + blood type) so the arena
    // renders it — the React `liveAttack` handler reads teamName / victimTeamName
    // / challengeTitle / type. The SignalR `/hub/attack` (unused by the client)
    // rides the same bus event harmlessly. Suppressed for hidden games / during the
    // ICPC freeze (`broadcast_ok`); RSCTF returns before any of these lookups run.
    if broadcast_ok {
        let attacker_team = team::Entity::find_by_id(attacker.team_id)
            .one(&st.db)
            .await?
            .map(|t| t.name)
            .unwrap_or_default();
        let victim_team = match participation::Entity::find_by_id(victim_service.participation_id)
            .one(&st.db)
            .await?
        {
            Some(p) => team::Entity::find_by_id(p.team_id)
                .one(&st.db)
                .await?
                .map(|t| t.name),
            None => None,
        };
        let challenge_title = game_challenge::Entity::find_by_id(victim_service.challenge_id)
            .one(&st.db)
            .await?
            .map(|c| c.title)
            .unwrap_or_default();
        // FirstBlood = the first time THIS victim (defending) team is breached on this
        // challenge — RSCTF fires the laser once per defending team, not once per
        // challenge game-wide (`AdGameController.BroadcastAdAttack`: count AdAttacks
        // where VictimParticipationId == victim && ChallengeId == challenge). rsctf's
        // ad_attack carries no participation/challenge columns, so we resolve the
        // victim team's service rows for this challenge and count captures against
        // them. This attack row is already persisted, so a count of 1 means it's the
        // victim team's first breach.
        let svc_ids: Vec<i32> = ad_team_service::Entity::find()
            .filter(ad_team_service::Column::GameId.eq(game_id))
            .filter(ad_team_service::Column::ChallengeId.eq(victim_service.challenge_id))
            .filter(ad_team_service::Column::ParticipationId.eq(victim_service.participation_id))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|s| s.id)
            .collect();
        let prior = if svc_ids.is_empty() {
            1
        } else {
            ad_attack::Entity::find()
                .filter(ad_attack::Column::VictimTeamServiceId.is_in(svc_ids))
                .count(&st.db)
                .await
                .unwrap_or(1)
        };
        let blood = if prior <= 1 { "FirstBlood" } else { "Normal" };
        st.publish_event(
            "ReceivedAttack",
            Some(game_id),
            serde_json::json!({
                "kind": "attack",
                "gameId": game_id,
                "teamName": attacker_team,
                "victimTeamName": victim_team,
                "challengeTitle": challenge_title,
                "type": blood,
            })
            .to_string(),
        );
    }

    Ok(("accepted", planted))
}
