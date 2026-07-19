//! `Ad/Submit` endpoint — batch flag submission and evidence adjudication.

use super::*;
use axum::response::{IntoResponse, Response};

/// RSCTF `Game.AdFlagLifetimeTicks` fallback — a flag planted in round `N` stays
/// submittable while the live round number is `< N + lifetime`. Used only when
/// the game row leaves `ad_flag_lifetime_ticks` null.
const AD_FLAG_LIFETIME_TICKS_DEFAULT: i32 = 5;

/// Max flags accepted in one batch submit (RSCTF bounds this server-side).
const AD_MAX_BATCH: usize = 100;

/// Resolve the latest row for one submitted flag together with its service,
/// but expose the service only when it still belongs to the authoritative
/// active A&D field. This preserves the historical latest-flag lookup while
/// replacing its second query and game-wide service scan with one bounded row.
const ACTIVE_VICTIM_FLAG_SQL: &str = r#"
    WITH planted AS (
        SELECT id, round_id, team_service_id
          FROM "AdFlags"
         WHERE flag = $1
         ORDER BY id DESC
         LIMIT 1
    )
    SELECT planted.id AS flag_id,
           planted_round.number AS planted_round_number,
           active.id AS service_id,
           active.participation_id,
           active.challenge_id
      FROM planted
      LEFT JOIN "AdRounds" planted_round
        ON planted_round.id = planted.round_id
       AND planted_round.game_id = $2
      LEFT JOIN LATERAL (
        SELECT service.id, service.participation_id, service.challenge_id
          FROM "AdTeamServices" service
          JOIN "Participations" victim
            ON victim.id = service.participation_id
           AND victim.game_id = service.game_id
          JOIN "GameChallenges" challenge
            ON challenge.id = service.challenge_id
           AND challenge.game_id = service.game_id
         WHERE service.id = planted.team_service_id
           AND service.game_id = $2
           AND victim.status = $3
           AND challenge.is_enabled = TRUE
           AND challenge.review_status = $4
           AND challenge."Type" = $5
      ) active ON TRUE
"#;

/// Atomically revalidate and insert one accepted capture, returning everything
/// needed by the live attack event. Keeping the metadata and victim-scoped
/// first-blood decision in this statement avoids four serialized round trips
/// after every successful insert.
const ACCEPTED_ATTACK_SQL: &str = r#"
    WITH latest_round AS (
        SELECT id, number
          FROM "AdRounds"
         WHERE game_id = $4 AND finalized = FALSE
         ORDER BY number DESC
         LIMIT 1
         FOR SHARE
    -- Every relation below is joined by its primary key and latest_round is
    -- bounded to one row, so candidate has cardinality zero or one.
    ), candidate AS (
        SELECT latest.id AS round_id,
               attacker.id AS attacker_participation_id,
               service.id AS victim_team_service_id,
               planted.id AS flag_id,
               now() AS submitted_at,
               NOT game.hidden
               AND NOT (
                   game.freeze_time_utc IS NOT NULL
                   AND now() >= game.freeze_time_utc
                   AND now() < game.end_time_utc
               ) AS broadcast_ok,
               COALESCE(attacker_team.name, '') AS attacker_team,
               victim_team.name AS victim_team,
               challenge.title AS challenge_title,
               service.challenge_id,
               service.participation_id AS victim_participation_id
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
          LEFT JOIN "Teams" attacker_team ON attacker_team.id = attacker.team_id
          LEFT JOIN "Teams" victim_team ON victim_team.id = victim.team_id
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
    ), inserted AS (
        INSERT INTO "AdAttacks"
          (round_id, attacker_participation_id, victim_team_service_id, flag_id, submitted_at)
        SELECT round_id, attacker_participation_id, victim_team_service_id, flag_id, submitted_at
          FROM candidate
        ON CONFLICT (attacker_participation_id, flag_id) DO NOTHING
        RETURNING id, victim_team_service_id, flag_id
    )
    SELECT candidate.broadcast_ok,
           candidate.attacker_team,
           candidate.victim_team,
           candidate.challenge_title,
           CASE WHEN candidate.broadcast_ok THEN NOT EXISTS (
               SELECT 1
                 FROM "AdAttacks" prior_attack
                 JOIN "AdTeamServices" prior_service
                   ON prior_service.id = prior_attack.victim_team_service_id
                WHERE prior_attack.id <> inserted.id
                  AND prior_service.game_id = $4
                  AND prior_service.challenge_id = candidate.challenge_id
                  AND prior_service.participation_id = candidate.victim_participation_id
           ) ELSE FALSE END AS first_blood
      FROM inserted
      JOIN candidate
        ON candidate.victim_team_service_id = inserted.victim_team_service_id
       AND candidate.flag_id = inserted.flag_id
"#;

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

enum AdSubmitCaller {
    Session(uuid::Uuid),
    TeamToken(String),
}

async fn acquire_submit_roster_fence(
    st: &SharedState,
    team_id: i32,
) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
    let key = format!("team-roster:{team_id}");
    crate::utils::single_flight::PgAdvisoryLock::try_acquire_shared(st.pg(), &key)
        .await?
        .ok_or_else(|| AppError::unavailable("Team credentials are changing; retry this request"))
}

async fn submit_caller_is_live(
    connection: &mut sqlx::PgConnection,
    caller: &AdSubmitCaller,
    part: &participation::Model,
) -> AppResult<bool> {
    if !crate::services::ad::roster::lock_team_shared_credentials_on(connection, part.team_id)
        .await?
    {
        return Ok(false);
    }
    match caller {
        AdSubmitCaller::Session(user_id) => {
            crate::services::ad::roster::user_allows_shared_credentials_on(
                connection,
                *user_id,
                part.game_id,
                part.team_id,
                part.id,
            )
            .await
        }
        AdSubmitCaller::TeamToken(token) => {
            let verified =
                crate::services::ad::api_token::authenticate_on(connection, token).await?;
            Ok(verified.is_some_and(|credential| {
                credential.participation.id == part.id
                    && credential.participation.game_id == part.game_id
                    && credential.participation.team_id == part.team_id
            }))
        }
    }
}

/// `POST /api/Game/{id}/Ad/Submit` — batch-submit flags captured from other
/// teams. Each flag is matched against a currently-valid `ad_flag` not owned by
/// the submitter; a hit records a deduplicated `ad_attack`.
#[allow(clippy::type_complexity)]
pub async fn submit(
    State(st): State<SharedState>,
    maybe_user: MaybeUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
    verified: Option<axum::Extension<crate::services::ad::api_token::VerifiedTeamToken>>,
    rejected: Option<axum::Extension<crate::services::ad::api_token::RejectedTeamToken>>,
    axum::Json(model): axum::Json<AdBatchSubmitModel>,
) -> AppResult<Response> {
    // RSCTF `AdBatchSubmitModel.Flags` is `[MinLength(1)][MaxLength(100)]`, so
    // `SubmitBatch` 400s on an empty or over-length batch before any work — never
    // silently truncating. Gate here so a 150-flag submit is rejected whole rather
    // than dropping the tail 50 captures under a partial success.
    if model.flags.is_empty() || model.flags.len() > AD_MAX_BATCH {
        return Err(AppError::bad_request(
            "Flags must contain between 1 and 100 entries",
        ));
    }

    let session_user_id = maybe_user.0.as_ref().map(|user| user.id);
    let presented_team_token = crate::services::ad::api_token::bearer_token(&headers)
        .filter(|token| crate::services::ad::api_token::is_well_formed(token))
        .map(str::to_owned);
    let token_auth_selected = verified.is_some() || presented_team_token.is_some();

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
    let caller = if token_auth_selected {
        AdSubmitCaller::TeamToken(presented_team_token.ok_or(AppError::Unauthorized)?)
    } else {
        AdSubmitCaller::Session(session_user_id.ok_or(AppError::Unauthorized)?)
    };

    // Charge the canonical participation for the database work the batch can
    // cause. Repeated flags are adjudicated once below, malformed flags never
    // reach PostgreSQL, and neither should consume 100 work units merely because
    // the response echoes 100 rows.
    let distinct_plausible_flags = model
        .flags
        .iter()
        .map(|flag| flag.trim())
        .filter(|flag| is_plausible_flag(flag))
        .collect::<HashSet<_>>()
        .len();
    if let Some(response) =
        crate::middlewares::rate_limiter::admit_ad_submit(id, attacker.id, distinct_plausible_flags)
            .await
    {
        return Ok(response);
    }

    // Serialize batches from one participation before retaining a pool
    // connection. The cross-replica advisory key then prevents two transactions
    // from deadlocking on the same `(attacker, flag)` uniqueness rows in a
    // different input order.
    let submit_key = format!("ad-submit:{id}:{}", attacker.id);
    let _submit_local = crate::utils::single_flight::coalesce(&submit_key).await;
    let mut roster = acquire_submit_roster_fence(&st, attacker.team_id).await?;
    roster.acquire_additional(&submit_key).await?;
    if !submit_caller_is_live(roster.transaction_mut(), &caller, &attacker).await? {
        roster.release().await?;
        return Err(match caller {
            AdSubmitCaller::Session(_) => AppError::Forbidden,
            AdSubmitCaller::TeamToken(_) => AppError::Unauthorized,
        });
    }

    // Every query from this point through the irreversible attack INSERTs uses
    // the transaction that owns the roster fence. This avoids a second pool
    // checkout and makes revocation linearizable with the complete batch.
    let game: Option<(DateTime<Utc>, DateTime<Utc>, bool, Option<i32>)> = sqlx::query_as(
        r#"SELECT start_time_utc, end_time_utc, ad_scoring_paused,
                  ad_flag_lifetime_ticks
             FROM "Games" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((game_start, game_end, scoring_paused, configured_lifetime_ticks)) = game else {
        roster.release().await?;
        return Err(AppError::not_found("Game not found"));
    };

    // Submissions are only accepted inside the game window. Round-number expiry
    // alone doesn't close the door at game end (the scheduler freezes the round
    // number), so flags from the last lifetime window would otherwise stay
    // submittable forever and mutate final standings. Mirrors RSCTF `SubmitBatch`.
    let now = Utc::now();
    if now < game_start || now >= game_end {
        let (status, message) = if now < game_start {
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
        roster.release().await?;
        return Ok(RequestResponse::ok(AdBatchSubmitResultModel {
            accepted_count: 0,
            results,
        })
        .into_response());
    }

    // Scoring pause halts the round scheduler + checker (no new flags, no SLA
    // accrual); captures must freeze too, or attack points keep accruing while
    // the rest of the board is paused. Records nothing, accepts nothing.
    if scoring_paused {
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
        roster.release().await?;
        return Ok(RequestResponse::ok(AdBatchSubmitResultModel {
            accepted_count: 0,
            results,
        })
        .into_response());
    }

    let lifetime_ticks = configured_lifetime_ticks
        .unwrap_or(AD_FLAG_LIFETIME_TICKS_DEFAULT)
        .clamp(1, 50);

    let current_round_number: Option<i32> = sqlx::query_scalar(
        r#"SELECT number FROM "AdRounds"
            WHERE game_id = $1 ORDER BY number DESC LIMIT 1"#,
    )
    .bind(id)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut results = Vec::new();
    let mut accepted_count = 0i32;
    let mut broadcasts = Vec::new();
    let mut adjudicated = HashMap::<String, (&'static str, Option<i32>)>::new();
    let submit_context = SubmitContext {
        game_id: id,
        attacker: &attacker,
        current_round_number,
        lifetime_ticks,
    };

    // Length already gated above (1..=AD_MAX_BATCH), so process the whole batch.
    for raw in model.flags.into_iter() {
        let flag_text = raw.trim().to_string();
        let outcome = match adjudicated.get(&flag_text).copied() {
            Some(previous) => SubmitOneResult::without_broadcast(repeated_batch_result(previous)),
            None => {
                let result =
                    submit_one(roster.transaction_mut(), &submit_context, &flag_text).await?;
                adjudicated.insert(flag_text, result.decision);
                result
            }
        };
        let (status, planted) = outcome.decision;
        if let Some(broadcast) = outcome.broadcast {
            broadcasts.push(broadcast);
        }
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

    // Commit every adjudicated attack before exposing cache invalidations or
    // real-time events. A cancellation/error drops the guard and rolls the whole
    // batch back without publishing phantom captures.
    roster.release().await?;

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
    for broadcast in broadcasts {
        broadcast.publish(&st);
    }

    Ok(RequestResponse::ok(AdBatchSubmitResultModel {
        accepted_count,
        results,
    })
    .into_response())
}

struct SubmitContext<'a> {
    game_id: i32,
    attacker: &'a participation::Model,
    current_round_number: Option<i32>,
    lifetime_ticks: i32,
}

type FlagDecision = (&'static str, Option<i32>);

struct SubmitOneResult {
    decision: FlagDecision,
    broadcast: Option<PendingAttackBroadcast>,
}

impl SubmitOneResult {
    fn without_broadcast(decision: FlagDecision) -> Self {
        Self {
            decision,
            broadcast: None,
        }
    }
}

struct PendingAttackBroadcast {
    game_id: i32,
    attacker_team: String,
    victim_team: Option<String>,
    challenge_title: String,
    blood: &'static str,
}

impl PendingAttackBroadcast {
    fn publish(self, st: &SharedState) {
        st.publish_event(
            "ReceivedAttack",
            Some(self.game_id),
            serde_json::json!({
                "kind": "attack",
                "gameId": self.game_id,
                "teamName": self.attacker_team,
                "victimTeamName": self.victim_team,
                "challengeTitle": self.challenge_title,
                "type": self.blood,
            })
            .to_string(),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, sqlx::FromRow)]
struct ActiveVictimService {
    id: i32,
    participation_id: i32,
    challenge_id: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, sqlx::FromRow)]
struct ActiveVictimFlag {
    flag_id: i32,
    planted_round_number: Option<i32>,
    service_id: Option<i32>,
    participation_id: Option<i32>,
    challenge_id: Option<i32>,
}

impl ActiveVictimFlag {
    fn active_service(self) -> Option<ActiveVictimService> {
        Some(ActiveVictimService {
            id: self.service_id?,
            participation_id: self.participation_id?,
            challenge_id: self.challenge_id?,
        })
    }
}

fn repeated_batch_result(
    (status, planted): (&'static str, Option<i32>),
) -> (&'static str, Option<i32>) {
    if status == "accepted" {
        ("duplicate", planted)
    } else {
        (status, planted)
    }
}

fn is_plausible_flag(value: &str) -> bool {
    const PREFIX: &[u8] = b"flag{";
    const PAYLOAD_LEN: usize = 32;
    let bytes = value.as_bytes();
    bytes.len() == PREFIX.len() + PAYLOAD_LEN + 1
        && bytes.starts_with(PREFIX)
        && bytes.ends_with(b"}")
        && bytes[PREFIX.len()..PREFIX.len() + PAYLOAD_LEN]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn require_active_victim(
    victim_service: Option<ActiveVictimService>,
    planted: Option<i32>,
) -> Result<ActiveVictimService, (&'static str, Option<i32>)> {
    victim_service.ok_or(("wrong", planted))
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct AcceptedAttack {
    broadcast_ok: bool,
    attacker_team: String,
    victim_team: Option<String>,
    challenge_title: String,
    first_blood: bool,
}

async fn insert_accepted_attack_on(
    connection: &mut sqlx::PgConnection,
    attacker_id: i32,
    victim_service_id: i32,
    flag_id: i32,
    game_id: i32,
) -> AppResult<Option<AcceptedAttack>> {
    sqlx::query_as(ACCEPTED_ATTACK_SQL)
        .bind(attacker_id)
        .bind(victim_service_id)
        .bind(flag_id)
        .bind(game_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(AD_FLAG_LIFETIME_TICKS_DEFAULT)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Adjudicate one flag and return its status plus planted round. Captures are
/// priced only when their epoch is scored, after rarity is known.
async fn submit_one(
    connection: &mut sqlx::PgConnection,
    context: &SubmitContext<'_>,
    flag_text: &str,
) -> AppResult<SubmitOneResult> {
    let game_id = context.game_id;
    let attacker = context.attacker;
    let current_round_number = context.current_round_number;
    let lifetime_ticks = context.lifetime_ticks;
    let Some(current_round_number) = current_round_number else {
        return Ok(SubmitOneResult::without_broadcast(("not_started", None)));
    };
    // Every engine-issued flag has one fixed 38-byte format. Reject malformed
    // input before binding it into PostgreSQL; the JSON body is globally
    // bounded, but one adversarial entry could otherwise still be very large.
    if !is_plausible_flag(flag_text) {
        return Ok(SubmitOneResult::without_broadcast(("wrong", None)));
    }

    let Some(flag) = sqlx::query_as::<_, ActiveVictimFlag>(ACTIVE_VICTIM_FLAG_SQL)
        .bind(flag_text)
        .bind(game_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
    else {
        return Ok(SubmitOneResult::without_broadcast(("wrong", None)));
    };
    let planted = flag.planted_round_number;

    let victim_service = match require_active_victim(flag.active_service(), planted) {
        Ok(victim_service) => victim_service,
        Err(result) => return Ok(SubmitOneResult::without_broadcast(result)),
    };

    if victim_service.participation_id == attacker.id {
        return Ok(SubmitOneResult::without_broadcast(("self_attack", planted)));
    }

    if let Some(planted) = planted {
        if planted < current_round_number - lifetime_ticks + 1 {
            return Ok(SubmitOneResult::without_broadcast((
                "expired",
                Some(planted),
            )));
        }
    }

    // Dedup an accepted capture on (attacker, flag) — RSCTF's unique key. A
    // distinct still-valid flag from the same victim service still scores, and
    // re-submitting the *same* flag (even in a later round) is a duplicate.
    // Atomic dedup on (attacker, flag) via ux_adattacks_attacker_flag: INSERT … ON
    // CONFLICT DO NOTHING RETURNING id. N concurrent identical-flag submits race into
    // one INSERT — only the winner gets a row back; the losers see None and are treated
    // as duplicates, so a capture can never be double-inserted (#14).
    let Some(inserted) = insert_accepted_attack_on(
        connection,
        attacker.id,
        victim_service.id,
        flag.flag_id,
        game_id,
    )
    .await?
    else {
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
        .bind(flag.flag_id)
        .bind(game_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(AD_FLAG_LIFETIME_TICKS_DEFAULT)
        .fetch_optional(&mut *connection)
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
        return Ok(SubmitOneResult::without_broadcast((status, planted)));
    };

    // The INSERT result carries the complete live event shape. FirstBlood is
    // victim-participation + challenge scoped, matching RSCTF's defending-team
    // laser semantics even when a challenge has multiple service rows. Like the
    // former COUNT, concurrent distinct captures may both label FirstBlood; this
    // is cosmetic, while AdAttacks dedup and constant epoch scoring are unaffected.
    if inserted.broadcast_ok {
        let blood = if inserted.first_blood {
            "FirstBlood"
        } else {
            "Normal"
        };
        return Ok(SubmitOneResult {
            decision: ("accepted", planted),
            broadcast: Some(PendingAttackBroadcast {
                game_id,
                attacker_team: inserted.attacker_team,
                victim_team: inserted.victim_team,
                challenge_title: inserted.challenge_title,
                blood,
            }),
        });
    }

    Ok(SubmitOneResult::without_broadcast(("accepted", planted)))
}

#[cfg(test)]
mod tests {
    use super::{
        is_plausible_flag, repeated_batch_result, require_active_victim, ActiveVictimFlag,
        ActiveVictimService, ACCEPTED_ATTACK_SQL, ACTIVE_VICTIM_FLAG_SQL,
    };

    #[test]
    fn inactive_victim_preserves_wrong_status_and_planted_round() {
        assert_eq!(
            require_active_victim(None, Some(41)),
            Err(("wrong", Some(41)))
        );
    }

    #[test]
    fn active_victim_continues_adjudication_with_exact_identifiers() {
        let expected = ActiveVictimService {
            id: 7,
            participation_id: 11,
            challenge_id: 13,
        };
        assert_eq!(
            require_active_victim(Some(expected), Some(41)).unwrap(),
            expected
        );
    }

    #[test]
    fn victim_lookup_is_bounded_to_one_authoritative_active_service() {
        for predicate in [
            "WHERE flag = $1",
            "LIMIT 1",
            "WHERE service.id = planted.team_service_id",
            "AND service.game_id = $2",
            "AND victim.status = $3",
            "AND challenge.is_enabled = TRUE",
            "AND challenge.review_status = $4",
            "AND challenge.\"Type\" = $5",
        ] {
            assert!(
                ACTIVE_VICTIM_FLAG_SQL.contains(predicate),
                "missing active-victim predicate: {predicate}"
            );
        }
        assert!(ACTIVE_VICTIM_FLAG_SQL.contains("ORDER BY id DESC"));
        assert!(!ACTIVE_VICTIM_FLAG_SQL.contains("SELECT *"));
    }

    #[test]
    fn planted_round_lookup_is_bounded_independently_of_round_history() {
        // `planted` is at most one row and the only AdRounds access follows its
        // primary-key id. Adding years of historical rounds therefore cannot
        // increase the rows materialized by a submit; in particular, never
        // restore the former game-wide `fetch_all` round map.
        for fragment in [
            r#"SELECT id, round_id, team_service_id"#,
            r#"ORDER BY id DESC"#,
            r#"LIMIT 1"#,
            r#"LEFT JOIN "AdRounds" planted_round"#,
            r#"ON planted_round.id = planted.round_id"#,
            r#"AND planted_round.game_id = $2"#,
            r#"planted_round.number AS planted_round_number"#,
        ] {
            assert!(
                ACTIVE_VICTIM_FLAG_SQL.contains(fragment),
                "missing bounded round-lookup invariant: {fragment}"
            );
        }
        assert_eq!(ACTIVE_VICTIM_FLAG_SQL.matches(r#""AdRounds""#).count(), 1);
        assert!(
            !ACTIVE_VICTIM_FLAG_SQL.contains(r#"SELECT id, number FROM "AdRounds" WHERE game_id"#)
        );
    }

    #[test]
    fn accepted_insert_returns_event_metadata_and_victim_scoped_first_blood() {
        for fragment in [
            r#"ON CONFLICT (attacker_participation_id, flag_id) DO NOTHING"#,
            r#"COALESCE(attacker_team.name, '') AS attacker_team"#,
            r#"victim_team.name AS victim_team"#,
            r#"challenge.title AS challenge_title"#,
            r#"prior_service.challenge_id = candidate.challenge_id"#,
            r#"prior_service.participation_id = candidate.victim_participation_id"#,
            r#"prior_attack.id <> inserted.id"#,
            r#"CASE WHEN candidate.broadcast_ok THEN NOT EXISTS"#,
        ] {
            assert!(
                ACCEPTED_ATTACK_SQL.contains(fragment),
                "missing accepted-insert invariant: {fragment}"
            );
        }
        assert!(ACCEPTED_ATTACK_SQL.contains("NOT game.hidden"));
        assert!(ACCEPTED_ATTACK_SQL.contains("game.freeze_time_utc IS NOT NULL"));
        assert_eq!(ACCEPTED_ATTACK_SQL.matches("INSERT INTO").count(), 1);
        assert!(!ACCEPTED_ATTACK_SQL.contains("COUNT("));
    }

    #[test]
    fn inactive_service_keeps_the_flag_identity_but_cannot_be_attacked() {
        let flag = ActiveVictimFlag {
            flag_id: 17,
            planted_round_number: Some(19),
            service_id: None,
            participation_id: None,
            challenge_id: None,
        };
        assert_eq!(flag.active_service(), None);
    }

    #[test]
    fn repeated_accepted_flag_is_reported_as_duplicate_without_double_counting() {
        assert_eq!(
            repeated_batch_result(("accepted", Some(41))),
            ("duplicate", Some(41))
        );
        assert_eq!(
            repeated_batch_result(("expired", Some(12))),
            ("expired", Some(12))
        );
    }

    #[test]
    fn malformed_or_oversized_flags_never_reach_postgres() {
        assert!(is_plausible_flag("flag{ABCDEFGHIJKLMNOPQRSTUVWXYZabcd_-}"));
        for invalid in [
            "",
            "flag{short}",
            "FLAG{ABCDEFGHIJKLMNOPQRSTUVWXYZabcd_-}",
            "flag{ABCDEFGHIJKLMNOPQRSTUVWXYZabcd+/}",
            "flag{ABCDEFGHIJKLMNOPQRSTUVWXYZabcd_-}suffix",
        ] {
            assert!(!is_plausible_flag(invalid), "accepted {invalid:?}");
        }
        assert!(!is_plausible_flag(&"x".repeat(1024 * 1024)));
    }
}

#[cfg(test)]
#[path = "submit_fence_tests.rs"]
mod fence_tests;

#[cfg(test)]
#[path = "submit_insert_tests.rs"]
mod insert_tests;
