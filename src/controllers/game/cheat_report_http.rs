//! Conditional, version-aware HTTP owner for the expensive cheat report.

use super::cheat_report_cache::{
    cache_cheat_report, cached_cheat_report, CheatReportFill, CHEAT_REPORT_BUILD_SLOTS,
    CHEAT_REPORT_FLIGHTS, MAX_CHEAT_REPORT_BYTES,
};
use super::*;
use axum::http::{HeaderMap, HeaderValue};
use bytes::Bytes;

const CHEAT_REPORT_RETRY_SECONDS: u64 = 2;
const MAX_CHEAT_REPORT_VERSION_KEYS: usize = 64;
const CHEAT_REPORT_VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
static CHEAT_REPORT_VERSION_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
static CHEAT_REPORT_VERSION_FLIGHTS: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<VersionFill>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Clone, Default)]
struct VersionFill {
    version: Option<String>,
    error: Option<String>,
}

fn cheat_report_etag(version: &str) -> String {
    format!(
        "W/\"rsctf-cheat-report-{}\"",
        crate::utils::codec::sha256_str(version)
    )
}

fn normalize_weak_etag(value: &str) -> &str {
    let value = value.trim();
    value.strip_prefix("W/").unwrap_or(value)
}

fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers.get_all(header::IF_NONE_MATCH).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value.split(',').any(|candidate| {
                let candidate = candidate.trim();
                candidate == "*" || normalize_weak_etag(candidate) == normalize_weak_etag(etag)
            })
        })
    })
}

fn response(body: Option<Bytes>, etag: &str) -> Response {
    let mut response = match body {
        Some(body) => (StatusCode::OK, body).into_response(),
        None => StatusCode::NOT_MODIFIED.into_response(),
    };
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("report hash is a valid ETag"),
    );
    response
}

#[derive(Debug, sqlx::FromRow)]
struct VersionRow {
    incident_id: i64,
    incident_count: i64,
    solve_submission_id: i64,
    solve_count: i64,
    suspicion_id: i64,
    suspicion_count: i64,
    identity_id: i64,
    identity_count: i64,
    game_signature: String,
    roster_signature: String,
    challenge_signature: String,
    rule_signature: String,
    exemption_signature: String,
    reconciliation_signature: String,
    pending_signature: String,
}

pub(super) async fn report_version(pool: &sqlx::PgPool, game_id: i32) -> AppResult<String> {
    let row = sqlx::query_as::<_, VersionRow>(
        r#"SELECT
              COALESCE((SELECT MAX(id)::bigint FROM "CheatInfo" WHERE game_id = $1), 0)
                AS incident_id,
              COALESCE((SELECT COUNT(*)::bigint FROM "CheatInfo" WHERE game_id = $1), 0)
                AS incident_count,
              COALESCE((SELECT MAX(first_solve.submission_id)::bigint
                          FROM "FirstSolves" first_solve
                          JOIN "Participations" participation
                            ON participation.id = first_solve.participation_id
                         WHERE participation.game_id = $1), 0) AS solve_submission_id,
              COALESCE((SELECT COUNT(*)::bigint
                          FROM "FirstSolves" first_solve
                          JOIN "Participations" participation
                            ON participation.id = first_solve.participation_id
                         WHERE participation.game_id = $1), 0) AS solve_count,
              COALESCE((SELECT MAX(id)::bigint FROM "SuspicionEvents" WHERE game_id = $1), 0)
                AS suspicion_id,
              COALESCE((SELECT COUNT(*)::bigint FROM "SuspicionEvents" WHERE game_id = $1), 0)
                AS suspicion_count,
              COALESCE((SELECT MAX(id)::bigint FROM "IdentityObservations" WHERE game_id = $1), 0)
                AS identity_id,
              COALESCE((SELECT COUNT(*)::bigint FROM "IdentityObservations" WHERE game_id = $1), 0)
                AS identity_count,
              COALESCE((SELECT MD5(JSONB_BUILD_ARRAY(
                                  start_time_utc, end_time_utc, practice_mode)::text)
                          FROM "Games" WHERE id = $1), MD5('[]')) AS game_signature,
              COALESCE((SELECT MD5(JSONB_AGG(JSONB_BUILD_ARRAY(
                                  participation.id, participation.status,
                                  participation.division_id, division.name,
                                  team.id, team.name, team.avatar_hash)
                                  ORDER BY participation.id)::text)
                          FROM "Participations" participation
                          JOIN "Teams" team ON team.id = participation.team_id
                     LEFT JOIN "Divisions" division ON division.id = participation.division_id
                         WHERE participation.game_id = $1), MD5('[]')) AS roster_signature,
              COALESCE((SELECT MD5(JSONB_AGG(JSONB_BUILD_ARRAY(id, title)
                                  ORDER BY id)::text)
                          FROM "GameChallenges" WHERE game_id = $1), MD5('[]'))
                AS challenge_signature,
              COALESCE((SELECT MD5(JSONB_AGG(JSONB_BUILD_ARRAY(rule_code, weight)
                                  ORDER BY rule_code)::text)
                          FROM "SuspicionRules"), MD5('[]')) AS rule_signature,
              COALESCE((SELECT MD5(JSONB_AGG(JSONB_BUILD_ARRAY(
                                  exemption.user_a, exemption.user_b, exemption.kind,
                                  ENCODE(exemption.value_hash, 'hex'),
                                  exemption.created_at_utc, exemption.expires_at_utc,
                                  exemption.revoked_at_utc)
                                  ORDER BY exemption.user_a, exemption.user_b,
                                           exemption.kind, exemption.value_hash,
                                           exemption.created_at_utc)::text)
                          FROM "AntiCheatExemptions" exemption
                         WHERE EXISTS (
                               SELECT 1 FROM "IdentityObservations" observation
                                WHERE observation.game_id = $1
                                  AND observation.user_id = exemption.user_a
                                  AND observation.kind = exemption.kind
                                  AND observation.value_hash = exemption.value_hash)
                           AND EXISTS (
                               SELECT 1 FROM "IdentityObservations" observation
                                WHERE observation.game_id = $1
                                  AND observation.user_id = exemption.user_b
                                  AND observation.kind = exemption.kind
                                  AND observation.value_hash = exemption.value_hash)), MD5('[]'))
                AS exemption_signature,
              COALESCE((SELECT MD5(JSONB_BUILD_ARRAY(
                                  evidence_closed_at_utc, last_reconciled_at_utc,
                                  sealed_at_utc, attempts, last_error,
                                  dirty_generation, completed_generation,
                                  dirty_mask, lease_token, lease_expires_at_utc)::text)
                          FROM "SuspicionReconciliationState" WHERE game_id = $1), MD5('[]'))
                AS reconciliation_signature,
              COALESCE((SELECT MD5(JSONB_AGG(JSONB_BUILD_ARRAY(
                                  id, observed_at_utc, completed_at_utc, last_error)
                                  ORDER BY id)::text)
                          FROM "SuspicionEvaluationOutbox"
                         WHERE game_id = $1 AND completed_at_utc IS NULL), MD5('[]'))
                AS pending_signature"#,
    )
    .bind(game_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        row.incident_id,
        row.incident_count,
        row.solve_submission_id,
        row.solve_count,
        row.suspicion_id,
        row.suspicion_count,
        row.identity_id,
        row.identity_count,
        row.game_signature,
        row.roster_signature,
        row.challenge_signature,
        row.rule_signature,
        row.exemption_signature,
        row.reconciliation_signature,
        row.pending_signature
    ))
}

async fn coalesced_report_version(pool: sqlx::PgPool, game_id: i32) -> AppResult<String> {
    let key = game_id.to_string();
    let fill = CHEAT_REPORT_VERSION_FLIGHTS
        .run_with_limit(
            &key,
            CHEAT_REPORT_VERSION_TIMEOUT,
            MAX_CHEAT_REPORT_VERSION_KEYS,
            move || async move {
                let Ok(_permit) = CHEAT_REPORT_VERSION_SLOTS.try_acquire() else {
                    return VersionFill {
                        error: Some("Cheat report version capacity is busy".to_string()),
                        ..Default::default()
                    };
                };
                match report_version(&pool, game_id).await {
                    Ok(version) => VersionFill {
                        version: Some(version),
                        error: None,
                    },
                    Err(error) => VersionFill {
                        version: None,
                        error: Some(error.to_string()),
                    },
                }
            },
        )
        .await;
    fill.version.ok_or_else(|| {
        AppError::retryable_unavailable(
            fill.error
                .as_deref()
                .unwrap_or("Cheat report version lookup timed out"),
            CHEAT_REPORT_RETRY_SECONDS,
        )
    })
}

pub async fn cheat_report(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let _ = load_game(&st, id).await?;
    let version = coalesced_report_version(st.pg().clone(), id).await?;
    let etag = cheat_report_etag(&version);
    if if_none_match(&headers, &etag) {
        return Ok(response(None, &etag));
    }
    if let Some(body) = cached_cheat_report(id, &version) {
        return Ok(response(Some(body), &etag));
    }
    let key = format!("{id}:{version}");
    let fill_version = version.clone();
    let fill = CHEAT_REPORT_FLIGHTS
        .run(&key, move || async move {
            let Ok(_permit) = CHEAT_REPORT_BUILD_SLOTS.try_acquire() else {
                return CheatReportFill::Busy;
            };
            match super::cheat::build_cheat_report(&st, id).await {
                Ok(report) => match serde_json::to_vec(&report) {
                    Ok(body) if body.len() <= MAX_CHEAT_REPORT_BYTES => {
                        let body = Bytes::from(body);
                        cache_cheat_report(id, &fill_version, &body);
                        CheatReportFill::Ready(body)
                    }
                    Ok(_) => CheatReportFill::Oversized,
                    Err(error) => CheatReportFill::Failed(error.to_string()),
                },
                Err(AppError::PayloadTooLarge(_)) => CheatReportFill::Oversized,
                Err(error) => CheatReportFill::Failed(error.to_string()),
            }
        })
        .await;
    match fill {
        CheatReportFill::Ready(body) => Ok(response(Some(body), &etag)),
        CheatReportFill::Busy | CheatReportFill::TimedOut => Err(AppError::retryable_unavailable(
            "Cheat report generation is busy; retry shortly",
            CHEAT_REPORT_RETRY_SECONDS,
        )),
        CheatReportFill::Oversized => Err(AppError::payload_too_large(
            "Cheat report exceeds the safe response limit; use the paginated evidence views",
        )),
        CheatReportFill::Failed(error) => Err(AppError::internal(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_etags_accept_weak_lists_and_reject_other_versions() {
        let etag = cheat_report_etag("1:2:3");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_str(&format!("\"other\", {etag}")).unwrap(),
        );
        assert!(if_none_match(&headers, &etag));
        assert!(!if_none_match(&headers, &cheat_report_etag("changed")));
    }

    #[test]
    fn overload_has_retry_after() {
        let response =
            AppError::retryable_unavailable("busy", CHEAT_REPORT_RETRY_SECONDS).into_response();
        assert_eq!(response.headers()[header::RETRY_AFTER], "2");
    }
}
