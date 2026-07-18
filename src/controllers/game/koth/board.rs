//! KotH board computation (compute_koth_board + team-row folding) — split
//! from koth/mod.rs to keep each file under the 1000-line rule.
use super::*;
use crate::services::ad_engine::AdScoringConfig;

#[derive(sqlx::FromRow)]
struct BoardGameRow {
    end_time_utc: DateTime<Utc>,
    freeze_time_utc: Option<DateTime<Utc>>,
    ad_tick_seconds: Option<i32>,
    koth_scoring_start_round: Option<i32>,
    koth_epoch_ticks: i32,
}

#[derive(sqlx::FromRow)]
struct RoundClockRow {
    number: i32,
    end_time_utc: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct LatestControlRow {
    challenge_id: i32,
    status: i16,
    round_number: i32,
    confirmed_participation_id: Option<i32>,
    confirmed_team_name: Option<String>,
}

#[derive(sqlx::FromRow)]
struct HillRow {
    challenge_id: i32,
    title: String,
    category: i16,
    is_enabled: bool,
    container_ip: Option<String>,
    container_port: Option<i32>,
    container_id: Option<String>,
    holder_participation_id: Option<i32>,
    holder_team_name: Option<String>,
}

#[derive(sqlx::FromRow)]
struct RosterRow {
    participation_id: i32,
    team_id: i32,
    team_name: String,
    division: Option<String>,
}

fn challenge_category(value: i16) -> AppResult<ChallengeCategory> {
    <ChallengeCategory as sea_orm::ActiveEnum>::try_from_value(&value)
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Compute the shared KotH board state for a game.
///
/// Combines the latest hill state with the bounded epoch scoring snapshot used by
/// both the player board and the admin console.
pub(super) async fn compute_koth_board(
    st: &SharedState,
    game_id: i32,
    cutoff: Option<DateTime<Utc>>,
    include_unreviewed: bool,
) -> AppResult<KothBoard> {
    compute_koth_board_inner(st, game_id, cutoff, include_unreviewed, true).await
}

/// Compute only the state used by the player automation hill list. This avoids
/// loading/materializing epoch scoring that the endpoint never serializes.
pub(super) async fn compute_koth_hill_state(
    st: &SharedState,
    game_id: i32,
) -> AppResult<KothBoard> {
    compute_koth_board_inner(st, game_id, None, false, false).await
}

async fn compute_koth_board_inner(
    st: &SharedState,
    game_id: i32,
    cutoff: Option<DateTime<Utc>>,
    include_unreviewed: bool,
    include_scoring: bool,
) -> AppResult<KothBoard> {
    let cfg = AdScoringConfig::from_env();

    let game = sqlx::query_as::<_, BoardGameRow>(
        r#"SELECT game.end_time_utc, game.freeze_time_utc,
                  game.ad_tick_seconds, game.koth_scoring_start_round,
                  game.koth_epoch_ticks
             FROM "Games" game
            WHERE game.id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let koth_scoring_start_round = game.koth_scoring_start_round;

    let event_ended = Utc::now() >= game.end_time_utc;
    // Every caller, including the direct hill/admin endpoints, gets the same
    // immutable event-end fence even when its wrapper supplied no public cutoff.
    let cutoff = if event_ended {
        Some(cutoff.map_or(game.end_time_utc, |value| value.min(game.end_time_utc)))
    } else {
        cutoff
    };
    // `cutoff` is the public freeze instant while live and the hard event-end
    // instant after close. Keep it for checker evidence at end: synthetic
    // closeout rows are timestamped exactly at the deadline, while historical
    // post-deadline observations must never enter the final score.
    let checker_cutoff = cutoff;
    let round_clock = sqlx::query_as::<_, RoundClockRow>(
        r#"SELECT number, end_time_utc FROM "AdRounds"
            WHERE game_id = $1
              AND ($2::timestamptz IS NULL
                   OR (NOT $3 AND start_time_utc <= $2)
                   OR ($3 AND start_time_utc < $2))
            ORDER BY number DESC LIMIT 1"#,
    )
    .bind(game_id)
    .bind(cutoff)
    .bind(event_ended)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let latest_round = round_clock.as_ref().map_or(0, |round| round.number);
    let current_round_ends_at = round_clock.as_ref().map(|round| round.end_time_utc);
    let latest_controls = sqlx::query_as::<_, LatestControlRow>(
        r#"SELECT DISTINCT ON (result.challenge_id)
                  result.challenge_id,
                  result.status,
                  round.number AS round_number,
                  confirmed.id AS confirmed_participation_id,
                  confirmed_team.name AS confirmed_team_name
             FROM "KothControlResults" result
             JOIN "AdRounds" round ON round.id = result.ad_round_id
             JOIN "KothCrownCycles" cycle
               ON cycle.id = result.cycle_id
              AND cycle.game_id = result.game_id
              AND cycle.challenge_id = result.challenge_id
              AND $6 BETWEEN cycle.planned_start_round AND cycle.planned_end_round
             JOIN LATERAL (
                  SELECT audit.attempt
                    FROM "KothCycleAuditReceipts" audit
                   WHERE audit.cycle_id = cycle.id
                     AND ($3::timestamptz IS NULL OR audit.created_at <= $3)
                   ORDER BY audit.attempt DESC, audit.created_at DESC, audit.id DESC
                   LIMIT 1
             ) capability_window
               ON capability_window.attempt = result.token_window_attempt
             JOIN "KothCycleAuditReceipts" activation
               ON activation.cycle_id = cycle.id
              AND activation.phase = 'FirewallPending'
              AND activation.attempt = capability_window.attempt
              AND ($3::timestamptz IS NULL OR activation.created_at <= $3)
        LEFT JOIN "Participations" confirmed
               ON confirmed.id = result.confirmed_participation_id
              AND confirmed.game_id = result.game_id
              AND confirmed.status = $5
        LEFT JOIN "Teams" confirmed_team ON confirmed_team.id = confirmed.team_id
            WHERE result.game_id = $1 AND round.game_id = result.game_id
              AND ($2::timestamptz IS NULL
                   OR (NOT $4 AND round.start_time_utc <= $2)
                   OR ($4 AND round.start_time_utc < $2))
              AND ($3::timestamptz IS NULL OR result.checked_at <= $3)
            ORDER BY result.challenge_id, round.number DESC, result.id DESC"#,
    )
    .bind(game_id)
    .bind(cutoff)
    .bind(checker_cutoff)
    .bind(event_ended)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(latest_round)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let latest_control_by_challenge: HashMap<i32, (String, i32)> = latest_controls
        .iter()
        .map(|row| {
            (
                row.challenge_id,
                (
                    koth_check_status_label(row.status).to_string(),
                    row.round_number,
                ),
            )
        })
        .collect();

    // The tick duration is shared with the A&D round engine.
    let tick_seconds = game
        .ad_tick_seconds
        .filter(|&s| s > 0)
        .map(|s| s as i64)
        .unwrap_or(cfg.tick_seconds)
        .max(1);
    let freeze = game.freeze_time_utc;

    // Load only board-visible challenge/target fields. The schema guarantees one
    // target per game and challenge; the lateral joins keep this hot query compact.
    let challenge_rows = sqlx::query_as::<_, HillRow>(
        r#"SELECT challenge.id AS challenge_id, challenge.title,
                  challenge.category, challenge.is_enabled,
                  address.host AS container_ip, address.port AS container_port,
                  address.container_id,
                  holder_participation.id AS holder_participation_id,
                  holder_team.name AS holder_team_name
             FROM "GameChallenges" challenge
        LEFT JOIN LATERAL (
               SELECT target.host, target.port, target.container_id
                 FROM "KothTargets" target
                WHERE target.game_id = challenge.game_id
                  AND target.challenge_id = challenge.id
                ORDER BY target.id LIMIT 1
             ) address ON TRUE
        LEFT JOIN LATERAL (
               SELECT target.holder_participation_id
                 FROM "KothTargets" target
                WHERE target.game_id = challenge.game_id
                  AND target.challenge_id = challenge.id
                  AND target.holder_participation_id IS NOT NULL
                ORDER BY target.id LIMIT 1
             ) holder ON TRUE
        LEFT JOIN "Participations" holder_participation
               ON holder_participation.id = holder.holder_participation_id
              AND holder_participation.game_id = challenge.game_id
              AND holder_participation.status = $5
        LEFT JOIN "Teams" holder_team ON holder_team.id = holder_participation.team_id
            WHERE challenge.game_id = $1 AND challenge."Type" = $2
              AND ($3 OR challenge.review_status = $4)
            ORDER BY challenge.category, challenge.id"#,
    )
    .bind(game_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .bind(include_unreviewed)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut holder_by_challenge: HashMap<i32, i32> = challenge_rows
        .iter()
        .filter_map(|row| {
            row.holder_participation_id
                .map(|participation_id| (row.challenge_id, participation_id))
        })
        .collect();
    let mut holder_team_name_by_challenge: HashMap<i32, String> = challenge_rows
        .iter()
        .filter_map(|row| {
            row.holder_team_name
                .as_ref()
                .map(|name| (row.challenge_id, name.clone()))
        })
        .collect();

    // A frozen view uses the latest confirmed holder whose check was visible at
    // the cutoff, not the target's live state.
    if cutoff.is_some() {
        holder_by_challenge.clear();
        holder_team_name_by_challenge.clear();
        for control in &latest_controls {
            if let Some(participation_id) = control.confirmed_participation_id {
                holder_by_challenge.insert(control.challenge_id, participation_id);
                if let Some(team_name) = control.confirmed_team_name.as_ref() {
                    holder_team_name_by_challenge.insert(control.challenge_id, team_name.clone());
                }
            }
        }
    }

    let hills: Vec<KothHillInfo> = challenge_rows
        .into_iter()
        .map(|row| {
            Ok(KothHillInfo {
                challenge_id: row.challenge_id,
                title: row.title,
                category: challenge_category(row.category)?,
                is_enabled: row.is_enabled,
                container_ip: row.container_ip,
                container_port: row.container_port,
                container_id: row.container_id,
            })
        })
        .collect::<AppResult<_>>()?;

    // The UI branches on `hills.length`, and the automation list never consumes
    // team scoring. Skip the roster and epoch pipeline in both cases.
    if hills.is_empty() || !include_scoring {
        return Ok(KothBoard {
            tick_seconds,
            freeze,
            latest_round,
            current_round_ends_at,
            hills,
            roster: Vec::new(),
            epoch_ticks: game.koth_epoch_ticks.clamp(2, 64),
            scoring_start_round: koth_scoring_start_round,
            scoring: KothScoringSnapshot {
                fully_settled: event_ended,
                ..KothScoringSnapshot::default()
            },
            holder_by_challenge,
            holder_team_name_by_challenge,
            latest_control_by_challenge,
        });
    }

    // ── Roster: the game's accepted participations (the full board, zeros incl.) ──
    let roster: Vec<RosterMember> = sqlx::query_as::<_, RosterRow>(
        r#"SELECT participation.id AS participation_id,
                  participation.team_id, team.name AS team_name,
                  division.name AS division
             FROM "Participations" participation
             JOIN "Teams" team ON team.id = participation.team_id
        LEFT JOIN "Divisions" division
               ON division.id = participation.division_id
              AND division.game_id = participation.game_id
            WHERE participation.game_id = $1
              AND participation.status = $2
            ORDER BY participation.id"#,
    )
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .map(|row| RosterMember {
        participation_id: row.participation_id,
        team_id: row.team_id,
        team_name: row.team_name,
        division: row.division,
    })
    .collect();
    let scoring = load_koth_scoring(st.pg(), game_id, cutoff, event_ended).await?;

    Ok(KothBoard {
        tick_seconds,
        freeze,
        latest_round,
        current_round_ends_at,
        hills,
        roster,
        epoch_ticks: game.koth_epoch_ticks.clamp(2, 64),
        scoring_start_round: koth_scoring_start_round,
        scoring,
        holder_by_challenge,
        holder_team_name_by_challenge,
        latest_control_by_challenge,
    })
}

/// Build the ranked epoch-normalized team rows for a chosen set of hill columns.
pub(super) fn build_team_rows(board: &KothBoard, hills: &[&KothHillInfo]) -> Vec<KothTeamScoreRow> {
    let mut rows: Vec<KothTeamScoreRow> = board
        .roster
        .iter()
        .map(|m| {
            let aggregate = board.scoring.teams.get(&m.participation_id);
            let hill_scores: Vec<KothHillScore> = hills
                .iter()
                .map(|h| {
                    let cell = aggregate.and_then(|team| team.cells.get(&h.challenge_id));
                    let is_holder =
                        board.holder_by_challenge.get(&h.challenge_id) == Some(&m.participation_id);
                    KothHillScore {
                        challenge_id: h.challenge_id,
                        settled_points: cell.map_or(0.0, |cell| cell.settled_points),
                        projected_points: cell.map_or(0.0, |cell| cell.projected_points),
                        acquisition_rate: cell.map_or(0.0, |cell| cell.acquisition_rate),
                        control_rate: cell.map_or(0.0, |cell| cell.control_rate),
                        reliability_rate: cell.map_or(0.0, |cell| cell.reliability_rate),
                        acquisition_windows: cell.map_or(0, |cell| cell.acquisition_windows),
                        controlled_ticks: cell.map_or(0, |cell| cell.controlled_ticks),
                        responsible_ticks: cell.map_or(0, |cell| cell.responsible_ticks),
                        healthy_responsible_ticks: cell
                            .map_or(0, |cell| cell.healthy_responsible_ticks),
                        is_current_holder: is_holder,
                    }
                })
                .collect();
            let epochs = aggregate.map_or_else(Vec::new, |aggregate| {
                let first = aggregate
                    .epochs
                    .len()
                    .saturating_sub(KOTH_DETAIL_EPOCH_LIMIT);
                aggregate.epochs[first..]
                    .iter()
                    .map(|epoch| KothEpochScore {
                        epoch: epoch.epoch,
                        points: epoch.points,
                        epoch_weight: epoch.epoch_weight,
                        finalized: epoch.finalized,
                    })
                    .collect()
            });
            KothTeamScoreRow {
                rank: 0,
                participation_id: m.participation_id,
                team_id: m.team_id,
                team_name: m.team_name.clone(),
                division: m.division.clone(),
                settled_total: aggregate.map_or(0.0, |aggregate| aggregate.settled_total),
                projected_total: aggregate.map_or(0.0, |aggregate| aggregate.projected_total),
                acquisition_rate: aggregate.map_or(0.0, |aggregate| aggregate.acquisition_rate),
                control_rate: aggregate.map_or(0.0, |aggregate| aggregate.control_rate),
                reliability_rate: aggregate.map_or(0.0, |aggregate| aggregate.reliability_rate),
                hills: hill_scores,
                epochs,
            }
        })
        .collect();

    sort_and_rank_team_rows(&mut rows);
    rows
}

fn team_acquisition_windows(row: &KothTeamScoreRow) -> i64 {
    row.hills.iter().fold(0_i64, |total, hill| {
        total.saturating_add(hill.acquisition_windows)
    })
}

/// Order tied official scores deterministically without using a live projection,
/// then expose that competitive order as ordinal displayed ranks.
fn sort_and_rank_team_rows(rows: &mut [KothTeamScoreRow]) {
    rows.sort_by(|a, b| {
        b.settled_total
            .partial_cmp(&a.settled_total)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.control_rate
                    .partial_cmp(&a.control_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.reliability_rate
                    .partial_cmp(&a.reliability_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| team_acquisition_windows(b).cmp(&team_acquisition_windows(a)))
            .then_with(|| a.participation_id.cmp(&b.participation_id))
    });
    for (index, row) in rows.iter_mut().enumerate() {
        row.rank = index as i32 + 1;
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One KotH hill (keyed by its challenge) with its shared-container address and
/// current holder — the raw material both boards shape their hill rows from.
pub(super) struct KothHillInfo {
    pub(super) challenge_id: i32,
    pub(super) title: String,
    pub(super) category: ChallengeCategory,
    pub(super) is_enabled: bool,
    pub(super) container_ip: Option<String>,
    pub(super) container_port: Option<i32>,
    /// Docker container id of the shared hill container (for the admin shell).
    pub(super) container_id: Option<String>,
}

/// One team eligible to appear on the board.
pub(super) struct RosterMember {
    pub(super) participation_id: i32,
    pub(super) team_id: i32,
    pub(super) team_name: String,
    pub(super) division: Option<String>,
}

/// Everything the scoreboard + admin console need, computed once from the DB.
pub(super) struct KothBoard {
    pub(super) tick_seconds: i64,
    pub(super) freeze: Option<DateTime<Utc>>,
    /// Highest `ad_round.number` advanced for the game (0 before the first round).
    pub(super) latest_round: i32,
    /// End time of the current round — the latest non-finalized round, falling
    /// back to the latest round overall — for the board's countdown.
    pub(super) current_round_ends_at: Option<DateTime<Utc>>,
    /// All KotH challenges for the game (enabled + disabled), sorted `(category, id)`.
    pub(super) hills: Vec<KothHillInfo>,
    /// Display roster — the game's accepted participations.
    pub(super) roster: Vec<RosterMember>,
    pub(super) epoch_ticks: i32,
    pub(super) scoring_start_round: Option<i32>,
    pub(super) scoring: KothScoringSnapshot,
    /// `challenge_id` → current holder participation id.
    pub(super) holder_by_challenge: HashMap<i32, i32>,
    /// `challenge_id` → current holder's team name.
    pub(super) holder_team_name_by_challenge: HashMap<i32, String>,
    /// `challenge_id` → latest `(checkStatus label, round number)` from the
    /// per-round `KothControlResult` history.
    pub(super) latest_control_by_challenge: HashMap<i32, (String, i32)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_challenge_categories_keep_the_domain_enum() {
        assert_eq!(challenge_category(0).unwrap(), ChallengeCategory::Misc);
        assert_eq!(challenge_category(9).unwrap(), ChallengeCategory::Ppc);
        assert_eq!(challenge_category(12).unwrap(), ChallengeCategory::Osint);
        assert!(challenge_category(13).is_err());
    }

    fn team_row(
        participation_id: i32,
        settled: f64,
        projected: f64,
        control: f64,
        reliability: f64,
        acquisitions: i64,
    ) -> KothTeamScoreRow {
        KothTeamScoreRow {
            rank: 0,
            participation_id,
            team_id: participation_id,
            team_name: format!("team-{participation_id}"),
            division: None,
            settled_total: settled,
            projected_total: projected,
            acquisition_rate: 0.0,
            control_rate: control,
            reliability_rate: reliability,
            hills: vec![KothHillScore {
                challenge_id: 1,
                settled_points: settled,
                projected_points: projected,
                acquisition_rate: 0.0,
                control_rate: control,
                reliability_rate: reliability,
                acquisition_windows: acquisitions,
                controlled_ticks: 0,
                responsible_ticks: 0,
                healthy_responsible_ticks: 0,
                is_current_holder: false,
            }],
            epochs: Vec::new(),
        }
    }

    #[test]
    fn equal_settled_scores_receive_ordinal_evidence_ranks() {
        let mut rows = vec![
            team_row(2, 50.0, 99.0, 0.4, 0.9, 2),
            team_row(1, 50.0, 10.0, 0.7, 0.8, 1),
            team_row(3, 40.0, 100.0, 1.0, 1.0, 10),
        ];

        sort_and_rank_team_rows(&mut rows);

        assert_eq!(
            rows.iter()
                .map(|row| row.participation_id)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            rows.iter().map(|row| row.rank).collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn official_tie_order_uses_control_reliability_acquisitions_then_id() {
        let mut rows = vec![
            team_row(5, 50.0, 100.0, 0.8, 0.7, 4),
            team_row(4, 50.0, 0.0, 0.8, 0.9, 1),
            team_row(3, 50.0, 50.0, 0.8, 0.9, 2),
            team_row(2, 50.0, 50.0, 0.8, 0.9, 2),
            team_row(1, 50.0, 50.0, 0.9, 0.1, 0),
        ];

        sort_and_rank_team_rows(&mut rows);

        assert_eq!(
            rows.iter()
                .map(|row| row.participation_id)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        assert_eq!(
            rows.iter().map(|row| row.rank).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
    }
}
