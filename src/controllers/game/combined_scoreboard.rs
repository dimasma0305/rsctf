//! Normalized standings across Jeopardy, Attack & Defense, and King of the Hill.
//!
//! Each format keeps its own official scoring contract. This projection maps
//! every active format onto the same fixed 0-100 interval, then weights that
//! result by the format's locked challenge count. No field-relative or
//! leader-relative scaling is used.

use super::*;
use axum::http::HeaderMap;

const SCORE_UNITS_PER_POINT: i64 = 10_000;
const MAX_COMPONENT_UNITS: i64 = 100 * SCORE_UNITS_PER_POINT;
const COMBINED_SCOREBOARD_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const COMBINED_SCOREBOARD_MIN_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

static COMBINED_SCOREBOARD_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ModeChallengeCounts {
    jeopardy: i64,
    attack_defense: i64,
    koth: i64,
}

impl ModeChallengeCounts {
    fn total(self) -> i64 {
        self.jeopardy + self.attack_defense + self.koth
    }

    fn from_board(board: &ScoreboardModel) -> Self {
        let mut counts = Self::default();
        for challenge in board.challenges.values().flatten() {
            match challenge.challenge_type {
                ChallengeType::AttackDefense => counts.attack_defense += 1,
                ChallengeType::KingOfTheHill => counts.koth += 1,
                _ => counts.jeopardy += 1,
            }
        }
        counts
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CombinedMode {
    pub active: bool,
    /// Locked number of enabled, approved challenges in this format.
    pub challenge_count: i64,
    /// Constant challenge-count share of the combined score, in `[0,1]`.
    pub weight: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CombinedModes {
    pub jeopardy: CombinedMode,
    pub attack_defense: CombinedMode,
    pub koth: CombinedMode,
}

impl CombinedModes {
    fn new(counts: ModeChallengeCounts) -> Self {
        let total = counts.total();
        let mode = |challenge_count| CombinedMode {
            active: challenge_count > 0,
            challenge_count,
            weight: if total == 0 {
                0.0
            } else {
                challenge_count as f64 / total as f64
            },
        };
        Self {
            jeopardy: mode(counts.jeopardy),
            attack_defense: mode(counts.attack_defense),
            koth: mode(counts.koth),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CombinedScoreComponent {
    pub active: bool,
    /// Official normalized score. A&D and KotH use finalized epochs.
    pub score: f64,
    /// Live projection, equal to `score` for Jeopardy.
    pub projected_score: f64,
    /// Raw Jeopardy points earned. Absent for epoch-scored formats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earned_points: Option<i64>,
    /// Maximum current Jeopardy contribution allowed for this division.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attainable_points: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CombinedScoreComponents {
    pub jeopardy: CombinedScoreComponent,
    pub attack_defense: CombinedScoreComponent,
    pub koth: CombinedScoreComponent,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CombinedScoreboardItem {
    pub id: i32,
    pub name: String,
    pub avatar: Option<String>,
    pub division_id: Option<i32>,
    pub division: Option<String>,
    pub rank: i32,
    pub division_rank: Option<i32>,
    /// Challenge-count-weighted mean of official component scores.
    pub score: f64,
    /// Challenge-count-weighted mean including open A&D/KotH epochs.
    pub projected_score: f64,
    pub components: CombinedScoreComponents,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CombinedDivision {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CombinedScoreboardModel {
    #[serde(with = "crate::utils::datetime::millis")]
    pub generated_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub freeze: Option<DateTime<Utc>>,
    pub is_frozen_view: bool,
    /// True when every active epoch-scored format has durably settled.
    pub fully_settled: bool,
    pub modes: CombinedModes,
    pub divisions: Vec<CombinedDivision>,
    pub items: Vec<CombinedScoreboardItem>,
}

#[derive(Debug)]
struct DivisionAccess {
    name: String,
    default_permissions: i32,
    challenge_permissions: HashMap<i32, i32>,
}

#[derive(Debug, sqlx::FromRow)]
struct DivisionAccessRow {
    id: i32,
    name: String,
    default_permissions: i32,
    challenge_id: Option<i32>,
    permissions: Option<i32>,
}

#[derive(Debug)]
struct RankableItem {
    model: CombinedScoreboardItem,
    score_units: i64,
    overall_eligible: bool,
    division_eligible: bool,
}

fn combined_cache_key(game_id: i32, is_monitor: bool) -> String {
    if is_monitor {
        format!("_CombinedScoreBoardByChallenge_{game_id}")
    } else {
        format!("_CombinedScoreBoardByChallengeFrozen_{game_id}")
    }
}

/// Do not let this derived cache add another full five seconds on top of an
/// already-aged component snapshot. A one-second floor retains single-flight
/// protection while a stale A&D SWR entry is being repaired.
fn combined_cache_ttl(generated_at: DateTime<Utc>, now: DateTime<Utc>) -> std::time::Duration {
    let age_ms = now
        .signed_duration_since(generated_at)
        .num_milliseconds()
        .max(0) as u64;
    COMBINED_SCOREBOARD_CACHE_TTL
        .saturating_sub(std::time::Duration::from_millis(age_ms))
        .max(COMBINED_SCOREBOARD_MIN_CACHE_TTL)
}

pub(crate) async fn invalidate_combined_scoreboard(st: &SharedState, game_id: i32) {
    let live = combined_cache_key(game_id, true);
    let frozen = combined_cache_key(game_id, false);
    tokio::join!(st.cache.remove(&live), st.cache.remove(&frozen),);
}

fn normalized_ratio_units(earned: i64, attainable: i64) -> i64 {
    if attainable <= 0 || earned <= 0 {
        return 0;
    }
    let earned = earned.min(attainable) as i128;
    let attainable = attainable as i128;
    let numerator = earned * i128::from(MAX_COMPONENT_UNITS);
    ((numerator + attainable / 2) / attainable) as i64
}

fn normalized_value_units(value: f64) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    (value.clamp(0.0, 100.0) * SCORE_UNITS_PER_POINT as f64).round() as i64
}

fn score_from_units(units: i64) -> f64 {
    units as f64 / SCORE_UNITS_PER_POINT as f64
}

fn challenge_weighted_mean_units(
    values: impl Iterator<Item = (i64, i64)>,
    total_challenges: i64,
) -> i64 {
    if total_challenges <= 0 {
        return 0;
    }
    let weighted_sum: i128 = values
        .map(|(score, challenge_count)| i128::from(score) * i128::from(challenge_count))
        .sum();
    let denominator = i128::from(total_challenges);
    ((weighted_sum + denominator / 2) / denominator) as i64
}

fn permission_for(
    division_id: Option<i32>,
    challenge_id: i32,
    divisions: &HashMap<i32, DivisionAccess>,
) -> GamePermission {
    let Some(division_id) = division_id else {
        return GamePermission(GamePermission::ALL);
    };
    let Some(division) = divisions.get(&division_id) else {
        return GamePermission(0);
    };
    GamePermission(
        division
            .challenge_permissions
            .get(&challenge_id)
            .copied()
            .unwrap_or(division.default_permissions),
    )
}

async fn load_division_access(
    st: &SharedState,
    game_id: i32,
) -> AppResult<HashMap<i32, DivisionAccess>> {
    let rows = sqlx::query_as::<_, DivisionAccessRow>(
        r#"SELECT division.id, division.name, division.default_permissions,
                  config.challenge_id, config.permissions
             FROM "Divisions" AS division
        LEFT JOIN "DivisionChallengeConfigs" AS config
               ON config.division_id = division.id
            WHERE division.game_id = $1
         ORDER BY division.id, config.challenge_id"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut divisions = HashMap::new();
    for row in rows {
        let division = divisions.entry(row.id).or_insert_with(|| DivisionAccess {
            name: row.name,
            default_permissions: row.default_permissions,
            challenge_permissions: HashMap::new(),
        });
        if let (Some(challenge_id), Some(permissions)) = (row.challenge_id, row.permissions) {
            division
                .challenge_permissions
                .insert(challenge_id, permissions);
        }
    }
    Ok(divisions)
}

fn jeopardy_attainable_points(
    item: &ScoreboardItem,
    board: &ScoreboardModel,
    divisions: &HashMap<i32, DivisionAccess>,
) -> i64 {
    board
        .challenges
        .values()
        .flatten()
        .filter(|challenge| {
            !matches!(
                challenge.challenge_type,
                ChallengeType::AttackDefense | ChallengeType::KingOfTheHill
            )
        })
        .filter_map(|challenge| {
            let permission = permission_for(item.division_id, challenge.id, divisions);
            permission.contains(GamePermission::GET_SCORE).then(|| {
                maximum_jeopardy_contribution(
                    challenge.score,
                    board.blood_bonus,
                    permission.contains(GamePermission::GET_BLOOD)
                        && !challenge.disable_blood_bonus,
                )
            })
        })
        .map(i64::from)
        .sum()
}

fn rank_items(items: &mut [RankableItem]) {
    items.sort_by(|left, right| {
        right
            .score_units
            .cmp(&left.score_units)
            .then_with(|| left.model.id.cmp(&right.model.id))
    });

    let mut overall_position = 0;
    let mut overall_previous: Option<(i64, i32)> = None;
    let mut division_ranks: HashMap<i32, (i32, i64, i32)> = HashMap::new();
    for item in items {
        if item.overall_eligible {
            overall_position += 1;
            let rank = overall_previous.map_or(overall_position, |(score, rank)| {
                if score == item.score_units {
                    rank
                } else {
                    overall_position
                }
            });
            item.model.rank = rank;
            overall_previous = Some((item.score_units, rank));
        }
        if item.division_eligible {
            if let Some(division_id) = item.model.division_id {
                let state = division_ranks
                    .entry(division_id)
                    .or_insert((0, i64::MIN, 0));
                state.0 += 1;
                if state.1 != item.score_units {
                    state.1 = item.score_units;
                    state.2 = state.0;
                }
                item.model.division_rank = Some(state.2);
            }
        }
    }
}

fn combine_scoreboards(
    game: &game::Model,
    jeopardy: ScoreboardModel,
    ad: Option<crate::services::ad::scoring::AdScoreboard>,
    koth: Option<koth::KothScoreboardModel>,
    divisions: HashMap<i32, DivisionAccess>,
    counts: ModeChallengeCounts,
) -> CombinedScoreboardModel {
    let total_challenges = counts.total();
    // The combined model represents the oldest source snapshot, not the time
    // this inexpensive projection happened to serialize it.
    let generated_at = std::iter::once(jeopardy.update_time_utc)
        .chain(ad.as_ref().map(|board| board.generated_at))
        .chain(koth.as_ref().map(|board| board.generated_at))
        .min()
        .unwrap_or_else(Utc::now);
    let ad_by_team: HashMap<i32, &crate::services::ad::scoring::AdTeamScore> = ad
        .as_ref()
        .map(|board| {
            board
                .teams
                .iter()
                .map(|team| (team.team_id, team))
                .collect()
        })
        .unwrap_or_default();
    let koth_by_team: HashMap<i32, &koth::KothTeamScoreRow> = koth
        .as_ref()
        .map(|board| {
            board
                .teams
                .iter()
                .map(|team| (team.team_id, team))
                .collect()
        })
        .unwrap_or_default();

    let mut items: Vec<RankableItem> = jeopardy
        .items
        .iter()
        .map(|item| {
            let attainable = if counts.jeopardy > 0 {
                jeopardy_attainable_points(item, &jeopardy, &divisions)
            } else {
                0
            };
            let jeopardy_units = if counts.jeopardy > 0 {
                normalized_ratio_units(item.score, attainable)
            } else {
                0
            };
            let ad_team = ad_by_team.get(&item.id).copied();
            let ad_units = if counts.attack_defense > 0 {
                normalized_value_units(ad_team.map_or(0.0, |team| team.settled_total))
            } else {
                0
            };
            let ad_projected_units = if counts.attack_defense > 0 {
                normalized_value_units(ad_team.map_or(0.0, |team| team.projected_total))
            } else {
                0
            };
            let koth_team = koth_by_team.get(&item.id).copied();
            let koth_units = if counts.koth > 0 {
                normalized_value_units(koth_team.map_or(0.0, |team| team.settled_total))
            } else {
                0
            };
            let koth_projected_units = if counts.koth > 0 {
                normalized_value_units(koth_team.map_or(0.0, |team| team.projected_total))
            } else {
                0
            };
            let score_units = challenge_weighted_mean_units(
                [
                    (jeopardy_units, counts.jeopardy),
                    (ad_units, counts.attack_defense),
                    (koth_units, counts.koth),
                ]
                .into_iter(),
                total_challenges,
            );
            let projected_units = challenge_weighted_mean_units(
                [
                    (jeopardy_units, counts.jeopardy),
                    (ad_projected_units, counts.attack_defense),
                    (koth_projected_units, counts.koth),
                ]
                .into_iter(),
                total_challenges,
            );
            let division = item
                .division_id
                .and_then(|id| divisions.get(&id).map(|division| division.name.clone()));

            RankableItem {
                model: CombinedScoreboardItem {
                    id: item.id,
                    name: item.name.clone(),
                    avatar: item.avatar.clone(),
                    division_id: item.division_id,
                    division,
                    rank: 0,
                    division_rank: None,
                    score: score_from_units(score_units),
                    projected_score: score_from_units(projected_units),
                    components: CombinedScoreComponents {
                        jeopardy: CombinedScoreComponent {
                            active: counts.jeopardy > 0,
                            score: score_from_units(jeopardy_units),
                            projected_score: score_from_units(jeopardy_units),
                            earned_points: (counts.jeopardy > 0).then_some(item.score.max(0)),
                            attainable_points: (counts.jeopardy > 0).then_some(attainable),
                        },
                        attack_defense: CombinedScoreComponent {
                            active: counts.attack_defense > 0,
                            score: score_from_units(ad_units),
                            projected_score: score_from_units(ad_projected_units),
                            earned_points: None,
                            attainable_points: None,
                        },
                        koth: CombinedScoreComponent {
                            active: counts.koth > 0,
                            score: score_from_units(koth_units),
                            projected_score: score_from_units(koth_projected_units),
                            earned_points: None,
                            attainable_points: None,
                        },
                    },
                },
                score_units,
                overall_eligible: item.rank > 0,
                division_eligible: item.division_rank.is_some(),
            }
        })
        .collect();
    rank_items(&mut items);

    let mut division_models: Vec<CombinedDivision> = divisions
        .iter()
        .map(|(id, division)| CombinedDivision {
            id: *id,
            name: division.name.clone(),
        })
        .collect();
    division_models.sort_by_key(|division| division.id);

    let is_frozen_view = jeopardy.is_frozen_view
        || ad.as_ref().is_some_and(|board| board.is_frozen_view)
        || koth.as_ref().is_some_and(|board| board.is_frozen_view);
    let fully_settled = ad.as_ref().is_none_or(|board| board.fully_settled)
        && koth.as_ref().is_none_or(|board| board.fully_settled);

    CombinedScoreboardModel {
        generated_at,
        freeze: game.freeze_time_utc,
        is_frozen_view,
        fully_settled,
        modes: CombinedModes::new(counts),
        divisions: division_models,
        items: items.into_iter().map(|item| item.model).collect(),
    }
}

async fn build_combined_scoreboard(
    st: &SharedState,
    game: &game::Model,
    is_monitor: bool,
) -> AppResult<CombinedScoreboardModel> {
    let jeopardy = build_scoreboard_cached(st, game, is_monitor).await?;
    // The scoreboard contains only enabled, approved challenges. Their scoring
    // eligibility is immutable after the competition boundary, so these counts
    // are the event's durable, precommitted Overall budget without a separate
    // operator-controlled weight or scoring-version selector.
    let counts = ModeChallengeCounts::from_board(&jeopardy);
    let ad_future = async {
        if counts.attack_defense > 0 {
            ad::build_ad_scoreboard_cached(st, game.id, is_monitor)
                .await
                .map(Some)
        } else {
            Ok(None)
        }
    };
    let koth_future = async {
        if counts.koth > 0 {
            koth::build_koth_scoreboard_cached(st, game, is_monitor)
                .await
                .map(Some)
        } else {
            Ok(None)
        }
    };
    let (ad, koth, divisions) =
        tokio::try_join!(ad_future, koth_future, load_division_access(st, game.id))?;
    Ok(combine_scoreboards(
        game, jeopardy, ad, koth, divisions, counts,
    ))
}

async fn encode_combined_scoreboard(
    model: &CombinedScoreboardModel,
) -> AppResult<super::scoreboard_encoding::BuiltBoardBody> {
    let raw = bytes::Bytes::from(
        serde_json::to_vec(model).map_err(|error| AppError::internal(error.to_string()))?,
    );
    super::scoreboard_encoding::build_bundle(raw).await
}

async fn cached_combined_scoreboard_bundle(st: &SharedState, key: &str) -> Option<bytes::Bytes> {
    let bytes = st.cache.get(key).await?;
    if super::scoreboard_encoding::valid_bundle(&bytes) {
        return Some(bytes);
    }
    tracing::warn!(
        cache_key = key,
        "evicting corrupt Overall scoreboard cache entry"
    );
    st.cache.remove(key).await;
    None
}

async fn build_combined_scoreboard_bundle(
    st: &SharedState,
    game: &game::Model,
    is_monitor: bool,
) -> AppResult<bytes::Bytes> {
    let key = combined_cache_key(game.id, is_monitor);
    if let Some(bytes) = cached_combined_scoreboard_bundle(st, &key).await {
        return Ok(bytes);
    }
    let (st2, game2, key2) = (st.clone(), game.clone(), key.clone());
    let result = COMBINED_SCOREBOARD_SF
        .run(&key, move || async move {
            if let Some(bytes) = cached_combined_scoreboard_bundle(&st2, &key2).await {
                return Some(bytes);
            }
            let model = build_combined_scoreboard(&st2, &game2, is_monitor)
                .await
                .ok()?;
            let now = Utc::now();
            let ttl = super::scoreboard_encoding::final_or_live_cache_ttl(
                !game2.practice_mode && now >= game2.end_time_utc,
                combined_cache_ttl(model.generated_at, now),
            );
            let built = encode_combined_scoreboard(&model).await.ok()?;
            if built.cacheable {
                st2.cache.set(&key2, &built.bytes, Some(ttl)).await;
            }
            Some(built.bytes)
        })
        .await;
    result.ok_or_else(|| AppError::internal("combined scoreboard cache fill failed"))
}

/// `GET /api/game/{id}/scoreboard/combined` — challenge-weighted normalized standings
/// across every active competition format in the game.
pub async fn combined_scoreboard(
    State(st): State<SharedState>,
    MaybeUser(maybe): MaybeUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let game = load_game_cached(&st, id).await?;
    let is_monitor = maybe.as_ref().is_some_and(|user| user.is_monitor());
    if game.hidden && !is_monitor {
        return Err(AppError::not_found("Game not found"));
    }
    if Utc::now() < game.start_time_utc {
        return Err(AppError::game_not_started());
    }
    let bundle = build_combined_scoreboard_bundle(&st, &game, is_monitor).await?;
    super::scoreboard_encoding::response(bundle, &headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_is_absolute_and_scheme_independent() {
        assert_eq!(normalized_ratio_units(50, 100), 50 * SCORE_UNITS_PER_POINT);
        assert_eq!(
            normalized_ratio_units(500, 1_000),
            50 * SCORE_UNITS_PER_POINT
        );
        assert_eq!(normalized_ratio_units(75, 100), 75 * SCORE_UNITS_PER_POINT);
    }

    #[test]
    fn normalization_is_bounded_and_rejects_invalid_denominators() {
        assert_eq!(normalized_ratio_units(150, 100), MAX_COMPONENT_UNITS);
        assert_eq!(normalized_ratio_units(-1, 100), 0);
        assert_eq!(normalized_ratio_units(10, 0), 0);
        assert_eq!(normalized_value_units(f64::NAN), 0);
        assert_eq!(normalized_value_units(-20.0), 0);
        assert_eq!(normalized_value_units(120.0), MAX_COMPONENT_UNITS);
    }

    #[test]
    fn derived_cache_never_doubles_component_staleness() {
        let now = Utc::now();
        assert_eq!(combined_cache_ttl(now, now), COMBINED_SCOREBOARD_CACHE_TTL);
        assert_eq!(
            combined_cache_ttl(now - chrono::Duration::milliseconds(4_500), now),
            COMBINED_SCOREBOARD_MIN_CACHE_TTL
        );
        assert_eq!(
            combined_cache_ttl(now - chrono::Duration::seconds(30), now),
            COMBINED_SCOREBOARD_MIN_CACHE_TTL
        );
    }

    #[test]
    fn challenge_budget_does_not_reuse_equal_weight_cache_entries() {
        assert_eq!(
            combined_cache_key(17, true),
            "_CombinedScoreBoardByChallenge_17"
        );
        assert_eq!(
            combined_cache_key(17, false),
            "_CombinedScoreBoardByChallengeFrozen_17"
        );
    }

    #[test]
    fn format_weight_is_its_locked_share_of_challenges() {
        let uneven = CombinedModes::new(ModeChallengeCounts {
            jeopardy: 0,
            attack_defense: 1,
            koth: 2,
        });
        assert!(!uneven.jeopardy.active);
        assert_eq!(uneven.jeopardy.challenge_count, 0);
        assert_eq!(uneven.jeopardy.weight, 0.0);
        assert_eq!(uneven.attack_defense.challenge_count, 1);
        assert!((uneven.attack_defense.weight - 1.0 / 3.0).abs() < f64::EPSILON);
        assert_eq!(uneven.koth.challenge_count, 2);
        assert!((uneven.koth.weight - 2.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn one_ad_service_and_two_koth_hills_have_equal_challenge_budgets() {
        let score = challenge_weighted_mean_units(
            [
                (80 * SCORE_UNITS_PER_POINT, 1),
                (70 * SCORE_UNITS_PER_POINT, 2),
            ]
            .into_iter(),
            3,
        );
        assert!((score_from_units(score) - 73.3333).abs() < 0.0001);
        assert_eq!(
            challenge_weighted_mean_units([(73 * SCORE_UNITS_PER_POINT, 1)].into_iter(), 1),
            73 * SCORE_UNITS_PER_POINT
        );
    }

    #[test]
    fn mode_counts_ignore_dynamic_jeopardy_values() {
        let challenge = |id, challenge_type, score| ChallengeInfo {
            id,
            title: id.to_string(),
            category: ChallengeCategory::Misc,
            challenge_type,
            score,
            solved: 0,
            deadline: None,
            bloods: Vec::new(),
            disable_blood_bonus: false,
        };
        let mut board = ScoreboardModel {
            update_time_utc: Utc::now(),
            blood_bonus: 0,
            timelines: Vec::new(),
            items: Vec::new(),
            divisions: Vec::new(),
            challenges: BTreeMap::from([(
                "Misc".to_owned(),
                vec![
                    challenge(1, ChallengeType::StaticAttachment, 1_000),
                    challenge(2, ChallengeType::AttackDefense, 0),
                    challenge(3, ChallengeType::KingOfTheHill, 0),
                    challenge(4, ChallengeType::KingOfTheHill, 0),
                ],
            )]),
            challenge_count: 4,
            freeze: None,
            is_frozen_view: false,
        };

        let before = ModeChallengeCounts::from_board(&board);
        board.challenges.get_mut("Misc").unwrap()[0].score = 250;
        let after = ModeChallengeCounts::from_board(&board);

        assert_eq!(before, after);
        assert_eq!(before.jeopardy, 1);
        assert_eq!(before.attack_defense, 1);
        assert_eq!(before.koth, 2);
    }

    #[test]
    fn jeopardy_ceiling_honors_division_score_and_blood_permissions() {
        let bonus = (500_i64 << 20) | (250_i64 << 10) | 100_i64;
        let challenge = |id, score, disable_blood_bonus| ChallengeInfo {
            id,
            title: id.to_string(),
            category: ChallengeCategory::Misc,
            challenge_type: ChallengeType::StaticAttachment,
            score,
            solved: 0,
            deadline: None,
            bloods: Vec::new(),
            disable_blood_bonus,
        };
        let board = ScoreboardModel {
            update_time_utc: Utc::now(),
            blood_bonus: bonus,
            timelines: Vec::new(),
            items: Vec::new(),
            divisions: Vec::new(),
            challenges: BTreeMap::from([(
                "Misc".to_owned(),
                vec![challenge(1, 100, false), challenge(2, 50, true)],
            )]),
            challenge_count: 2,
            freeze: None,
            is_frozen_view: false,
        };
        let access = |defaults, overrides| DivisionAccess {
            name: String::new(),
            default_permissions: defaults,
            challenge_permissions: overrides,
        };
        let divisions = HashMap::from([
            (
                1,
                access(
                    GamePermission::GET_SCORE | GamePermission::GET_BLOOD,
                    HashMap::new(),
                ),
            ),
            (2, access(GamePermission::GET_SCORE, HashMap::new())),
            (
                3,
                access(
                    GamePermission::GET_SCORE | GamePermission::GET_BLOOD,
                    HashMap::from([(1, GamePermission::GET_SCORE)]),
                ),
            ),
        ]);
        let item = |division_id| ScoreboardItem {
            id: 1,
            name: String::new(),
            bio: None,
            division_id,
            avatar: None,
            score: 0,
            rank: 1,
            division_rank: division_id.map(|_| 1),
            last_submission_time: DateTime::<Utc>::MIN_UTC,
            solved_challenges: Vec::new(),
            solved_count: 0,
        };

        // Challenge 1 has a 50% first-blood ceiling; challenge 2 explicitly
        // disables blood, so its ceiling remains its 50-point base value.
        assert_eq!(
            jeopardy_attainable_points(&item(None), &board, &divisions),
            200
        );
        assert_eq!(
            jeopardy_attainable_points(&item(Some(1)), &board, &divisions),
            200
        );
        assert_eq!(
            jeopardy_attainable_points(&item(Some(2)), &board, &divisions),
            150
        );
        assert_eq!(
            jeopardy_attainable_points(&item(Some(3)), &board, &divisions),
            150
        );
        assert_eq!(
            jeopardy_attainable_points(&item(Some(404)), &board, &divisions),
            0
        );
    }

    fn rankable(id: i32, score: i64, projected: i64, division: Option<i32>) -> RankableItem {
        let inactive = || CombinedScoreComponent {
            active: false,
            score: 0.0,
            projected_score: 0.0,
            earned_points: None,
            attainable_points: None,
        };
        RankableItem {
            model: CombinedScoreboardItem {
                id,
                name: id.to_string(),
                avatar: None,
                division_id: division,
                division: None,
                rank: 0,
                division_rank: None,
                score: score_from_units(score),
                projected_score: score_from_units(projected),
                components: CombinedScoreComponents {
                    jeopardy: inactive(),
                    attack_defense: inactive(),
                    koth: inactive(),
                },
            },
            score_units: score,
            overall_eligible: true,
            division_eligible: division.is_some(),
        }
    }

    #[test]
    fn ranking_uses_only_official_score_and_shares_exact_ties() {
        let mut rows = vec![
            rankable(8, 500, 700, Some(1)),
            rankable(2, 500, 700, Some(1)),
            rankable(4, 500, 800, Some(2)),
            rankable(9, 600, 600, Some(1)),
        ];
        rank_items(&mut rows);
        assert_eq!(
            rows.iter().map(|row| row.model.id).collect::<Vec<_>>(),
            [9, 2, 4, 8]
        );
        assert_eq!(
            rows.iter().map(|row| row.model.rank).collect::<Vec<_>>(),
            [1, 2, 2, 2]
        );
        assert_eq!(rows[0].model.division_rank, Some(1));
        assert_eq!(rows[1].model.division_rank, Some(2));
        assert_eq!(rows[3].model.division_rank, Some(2));
    }

    #[tokio::test]
    async fn large_overall_board_uses_the_shared_precompressed_bundle() {
        use std::io::Read;

        let mut item = rankable(1, 500, 500, Some(1)).model;
        item.name = "A".repeat(8 * 1024);
        let model = CombinedScoreboardModel {
            generated_at: Utc::now(),
            freeze: None,
            is_frozen_view: false,
            fully_settled: true,
            modes: CombinedModes::new(ModeChallengeCounts {
                jeopardy: 1,
                attack_defense: 1,
                koth: 1,
            }),
            divisions: vec![CombinedDivision {
                id: 1,
                name: "Open".to_owned(),
            }],
            items: vec![item],
        };
        let raw = serde_json::to_vec(&model).unwrap();
        let bundle = encode_combined_scoreboard(&model).await.unwrap();
        assert!(bundle.cacheable);

        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
        let response = super::super::scoreboard_encoding::response(bundle.bytes, &headers).unwrap();
        assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");

        let encoded = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(encoded.len() < raw.len() / 10);
        let mut decoder = flate2::read::GzDecoder::new(encoded.as_ref());
        let mut decoded = Vec::new();
        decoder.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, raw);
    }
}
