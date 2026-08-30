//! Bounded pair-comparison work for the anti-cheat monitor.

use super::*;

/// A pair comparison is deliberately small enough that its quadratic LCS has a
/// predictable upper bound once it moves to the blocking pool.
pub(super) const MAX_COMPARE_SOLVES_PER_PARTICIPATION: usize = 512;
const MAX_REPORT_CANONICAL_SOLVES: usize = 25_000;
const COMPARE_BUILD_CONCURRENCY: usize = 2;
static COMPARE_BUILD_ADMISSION: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(COMPARE_BUILD_CONCURRENCY))
    });

/// Query for the collusion `compare` endpoint (`?participationA=&participationB=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareQuery {
    pub participation_a: i32,
    pub participation_b: i32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct CanonicalSolveRow {
    pub(super) participation_id: i32,
    pub(super) challenge_id: i32,
    pub(super) challenge_title: String,
    pub(super) submit_time_utc: DateTime<Utc>,
}

/// `GET /api/game/{id}/cheatreport/compare` — requires Monitor.
///
/// The database input and concurrent CPU work are both hard-bounded. The
/// quadratic LCS runs on Tokio's blocking pool, so a slow or disconnected
/// monitor cannot stall unrelated async requests.
pub async fn cheat_report_compare(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
    Query(q): Query<CompareQuery>,
) -> AppResult<Response> {
    let _ = load_game(&st, id).await?;

    if q.participation_a == q.participation_b {
        return Err(AppError::bad_request(
            "Cannot compare a participation with itself.",
        ));
    }

    let permit = match COMPARE_BUILD_ADMISSION.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return Ok(compare_busy_response()),
    };

    // Validate both ids in one game-scoped query.
    let requested = [q.participation_a, q.participation_b];
    let found = sqlx::query_scalar::<_, i32>(
        r#"SELECT id
             FROM "Participations"
            WHERE game_id = $1 AND id = ANY($2::INTEGER[])"#,
    )
    .bind(id)
    .bind(&requested[..])
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if found.len() != requested.len() {
        return Err(AppError::bad_request(
            "One or both participations were not found in this game.",
        ));
    }

    let solves = canonical_solves_bounded(
        st.pg(),
        id,
        &requested,
        MAX_COMPARE_SOLVES_PER_PARTICIPATION,
        MAX_COMPARE_SOLVES_PER_PARTICIPATION * requested.len(),
    )
    .await?;
    let participation_a = q.participation_a;
    let computation = tokio::task::spawn_blocking(move || {
        // The blocking task, not the cancelable HTTP future, owns admission.
        // Disconnecting a monitor therefore cannot release capacity while LCS
        // work is still running on the blocking pool.
        let _permit = permit;
        let titles: HashMap<i32, String> = solves
            .iter()
            .map(|solve| (solve.challenge_id, solve.challenge_title.clone()))
            .collect();
        let (sub_a, sub_b): (Vec<_>, Vec<_>) = solves
            .into_iter()
            .partition(|solve| solve.participation_id == participation_a);
        let (rsi, _common, details) = collusion_metrics(&sub_a, &sub_b, &titles);
        CollusionCompareResult { rsi, details }
    })
    .await
    .map_err(|error| AppError::internal(format!("anti-cheat comparison task failed: {error}")))?;
    Ok(RequestResponse::ok(computation).into_response())
}

fn compare_busy_response() -> Response {
    let mut response =
        AppError::unavailable("Anti-cheat comparison workers are busy; retry shortly")
            .into_response();
    response.headers_mut().insert(
        header::RETRY_AFTER,
        axum::http::HeaderValue::from_static("1"),
    );
    response
}

/// Canonical first solves in deterministic solve order with both a per-team and
/// a total `MAX + 1` guard. Callers must provide an explicit participation set;
/// whole-game report reads first derive a bounded set of incident participants.
pub(super) async fn canonical_solves_bounded(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_ids: &[i32],
    max_per_participation: usize,
    max_total: usize,
) -> AppResult<Vec<CanonicalSolveRow>> {
    if participation_ids.is_empty() {
        return Ok(Vec::new());
    }
    let fetch_limit = max_total
        .checked_add(1)
        .and_then(|limit| i64::try_from(limit).ok())
        .ok_or_else(|| AppError::internal("anti-cheat solve limit overflow"))?;
    let rows = sqlx::query_as::<_, CanonicalSolveRow>(
        r#"SELECT first_solve.participation_id,
                  first_solve.challenge_id,
                  challenge.title AS challenge_title,
                  submission.submit_time_utc
             FROM "FirstSolves" first_solve
             JOIN "Submissions" submission
               ON submission.id = first_solve.submission_id
              AND submission.participation_id = first_solve.participation_id
              AND submission.challenge_id = first_solve.challenge_id
             JOIN "Participations" participation
               ON participation.id = first_solve.participation_id
              AND participation.game_id = submission.game_id
             JOIN "Games" game
               ON game.id = submission.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = first_solve.challenge_id
              AND challenge.game_id = submission.game_id
            WHERE participation.game_id = $1
              AND submission.status = $2
              AND submission.submit_time_utc >= game.start_time_utc
              AND submission.submit_time_utc < game.end_time_utc
              AND first_solve.participation_id = ANY($3::INTEGER[])
            ORDER BY submission.submit_time_utc, submission.id
            LIMIT $4"#,
    )
    .bind(game_id)
    .bind(AnswerResult::Accepted as i16)
    .bind(participation_ids)
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    if rows.len() > max_total {
        return Err(AppError::payload_too_large(format!(
            "Anti-cheat comparison is limited to {max_total} canonical solves"
        )));
    }
    let mut per_participation = HashMap::<i32, usize>::new();
    for solve in &rows {
        let count = per_participation.entry(solve.participation_id).or_default();
        *count += 1;
        if *count > max_per_participation {
            return Err(AppError::payload_too_large(format!(
                "Anti-cheat comparison is limited to {max_per_participation} solves per participation"
            )));
        }
    }
    Ok(rows)
}

pub(super) async fn canonical_report_solves(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_ids: &[i32],
) -> AppResult<Vec<CanonicalSolveRow>> {
    canonical_solves_bounded(
        pool,
        game_id,
        participation_ids,
        MAX_COMPARE_SOLVES_PER_PARTICIPATION,
        MAX_REPORT_CANONICAL_SOLVES,
    )
    .await
}

/// Length of the longest common subsequence of two challenge-id sequences
/// (mirrors RSCTF `GetLongestCommonSubsequence`, rolling one-row DP).
fn lcs_len(a: &[i32], b: &[i32]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let m = b.len();
    let mut dp = vec![0usize; m + 1];
    for &x in a {
        let mut prev = 0usize;
        for j in 0..m {
            let tmp = dp[j + 1];
            dp[j + 1] = if x == b[j] {
                prev + 1
            } else {
                dp[j + 1].max(dp[j])
            };
            prev = tmp;
        }
    }
    dp[m]
}

/// RSI + common-solve overlap between two participations' canonical solves.
/// Lookup maps avoid the previous repeated linear scans while preserving the
/// established ordering and 50-row response cap.
pub(super) fn collusion_metrics(
    sub_a: &[CanonicalSolveRow],
    sub_b: &[CanonicalSolveRow],
    titles: &HashMap<i32, String>,
) -> (f64, Vec<String>, Vec<Json>) {
    let seq_a: Vec<i32> = sub_a.iter().map(|s| s.challenge_id).collect();
    let seq_b: Vec<i32> = sub_b.iter().map(|s| s.challenge_id).collect();
    let times_a: HashMap<i32, DateTime<Utc>> = sub_a
        .iter()
        .map(|solve| (solve.challenge_id, solve.submit_time_utc))
        .collect();
    let times_b: HashMap<i32, DateTime<Utc>> = sub_b
        .iter()
        .map(|solve| (solve.challenge_id, solve.submit_time_utc))
        .collect();
    let set_a: HashSet<i32> = seq_a.iter().copied().collect();
    let set_b: HashSet<i32> = seq_b.iter().copied().collect();

    let mut inter: Vec<i32> = set_a
        .iter()
        .copied()
        .filter(|challenge| set_b.contains(challenge))
        .collect();
    inter.sort_unstable();
    let union = set_a.union(&set_b).count();
    let jaccard = if union == 0 {
        0.0
    } else {
        inter.len() as f64 / union as f64
    };
    let lcs = lcs_len(&seq_a, &seq_b);
    let min_len = seq_a.len().min(seq_b.len());
    let lcs_score = if min_len == 0 {
        0.0
    } else {
        lcs as f64 / min_len as f64
    };
    let rsi = jaccard * 0.7 + lcs_score * 0.3;

    let mut rows: Vec<(String, DateTime<Utc>, DateTime<Utc>, f64)> = Vec::new();
    let mut common_solves = Vec::with_capacity(inter.len());
    for challenge_id in inter {
        let name = titles
            .get(&challenge_id)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        common_solves.push(name.clone());
        if let (Some(&time_a), Some(&time_b)) =
            (times_a.get(&challenge_id), times_b.get(&challenge_id))
        {
            let difference = ((time_a - time_b).num_milliseconds().abs() as f64) / 1000.0;
            rows.push((name, time_a, time_b, difference));
        }
    }
    rows.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(50);
    rows.sort_by_key(|row| row.1);

    let detailed = rows
        .into_iter()
        .map(|(name, time_a, time_b, difference)| {
            serde_json::json!({
                "challengeName": name,
                "timeA": time_a.timestamp_millis(),
                "timeB": time_b.timestamp_millis(),
                "timeDiff": difference,
            })
        })
        .collect();
    (rsi, common_solves, detailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solve(participation_id: i32, challenge_id: i32, second: i64) -> CanonicalSolveRow {
        CanonicalSolveRow {
            participation_id,
            challenge_id,
            challenge_title: format!("Challenge {challenge_id}"),
            submit_time_utc: DateTime::from_timestamp(second, 0).unwrap(),
        }
    }

    #[test]
    fn pair_metrics_keep_the_wire_cap_and_use_canonical_times() {
        let left = (0..80)
            .map(|id| solve(1, id, i64::from(id)))
            .collect::<Vec<_>>();
        let right = (0..80)
            .map(|id| solve(2, id, i64::from(id + 1)))
            .collect::<Vec<_>>();
        let titles = left
            .iter()
            .map(|row| (row.challenge_id, row.challenge_title.clone()))
            .collect();
        let (rsi, common, details) = collusion_metrics(&left, &right, &titles);
        assert_eq!(rsi, 1.0);
        assert_eq!(common.len(), 80);
        assert_eq!(details.len(), 50);
    }

    #[test]
    fn overload_response_is_retryable() {
        let response = compare_busy_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    }
}
