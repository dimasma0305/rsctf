use super::*;

const GLOBAL_SCAN_SLOTS: i16 = 2;
const SCAN_CANDIDATE_LIMIT: i64 = 32;
const SCAN_CLAIM_LOCK: &str = "repo-binding-scan-admission";
const SCAN_LEASE_RENEW_SECONDS: u64 = 60;

fn scan_host_key(repo_url: &str) -> String {
    reqwest::Url::parse(repo_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
        .filter(|host| !host.is_empty() && host.len() <= 255)
        // Legacy damaged URLs must still enter the scheduler once, fail with a
        // bounded backoff, and remain operator-visible instead of hot-looping.
        .unwrap_or_else(|| "invalid-repository-host".to_string())
}

pub(crate) async fn claim_repo_scan(
    pool: &sqlx::PgPool,
    specific_id: Option<i32>,
    limit: i64,
) -> AppResult<Vec<(i32, Uuid)>> {
    let requested = limit.clamp(1, i64::from(GLOBAL_SCAN_SLOTS));
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let owns_admission = crate::utils::single_flight::try_acquire_transaction_advisory_lock(
        &mut transaction,
        SCAN_CLAIM_LOCK,
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !owns_admission {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        if specific_id.is_some() {
            return Err(AppError::conflict(
                "Repository scan admission is busy; retry",
            ));
        }
        return Ok(Vec::new());
    }

    // Slot-bearing leases are unique, so this cleanup touches at most two
    // rows. Legacy leases created during a rolling upgrade have no slot and
    // remain valid until their original expiry.
    sqlx::query(
        r#"UPDATE "RepoBindings"
              SET scan_lease_token = NULL, scan_lease_until = NULL,
                  scan_started_at_utc = NULL, scan_host_key = NULL,
                  scan_slot = NULL
            WHERE scan_slot IS NOT NULL
              AND (scan_lease_until IS NULL
                   OR scan_lease_until <= clock_timestamp())"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let active_lease_count = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*)::BIGINT FROM "RepoBindings"
            WHERE scan_lease_token IS NOT NULL
              AND scan_lease_until > clock_timestamp()"#,
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let capacity = i64::from(GLOBAL_SCAN_SLOTS)
        .saturating_sub(active_lease_count)
        .min(requested);
    if capacity == 0 {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(Vec::new());
    }

    let live_admission = sqlx::query_as::<_, (i16, String)>(
        r#"SELECT scan_slot, scan_host_key FROM "RepoBindings"
            WHERE scan_slot IS NOT NULL
              AND scan_host_key IS NOT NULL
              AND scan_lease_until > clock_timestamp()"#,
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut used_slots = live_admission
        .iter()
        .map(|(slot, _)| *slot)
        .collect::<std::collections::HashSet<_>>();
    let mut used_hosts = live_admission
        .into_iter()
        .map(|(_, host)| host)
        .collect::<std::collections::HashSet<_>>();

    let candidate_limit = if specific_id.is_some() {
        1
    } else {
        SCAN_CANDIDATE_LIMIT
    };
    let candidates = sqlx::query_as::<_, (i32, String)>(
        r#"SELECT id, repo_url FROM "RepoBindings"
            WHERE (
                    ($2::INTEGER IS NOT NULL AND id = $2)
                    OR (
                        $2::INTEGER IS NULL
                        AND status = $1
                        AND (next_scan_utc IS NULL
                             OR next_scan_utc <= clock_timestamp())
                    )
                  )
              AND (scan_lease_until IS NULL
                   OR scan_lease_until <= clock_timestamp())
            ORDER BY next_scan_utc NULLS FIRST, id
            FOR UPDATE SKIP LOCKED
            LIMIT $3"#,
    )
    .bind(RepoWatchStatus::Active as i16)
    .bind(specific_id)
    .bind(candidate_limit)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let token = Uuid::new_v4();
    let mut claimed = Vec::with_capacity(capacity as usize);
    for (id, repo_url) in candidates {
        if claimed.len() >= capacity as usize {
            break;
        }
        let host_key = scan_host_key(&repo_url);
        if used_hosts.contains(&host_key) {
            continue;
        }
        let Some(slot) = (0..GLOBAL_SCAN_SLOTS).find(|slot| !used_slots.contains(slot)) else {
            break;
        };
        let updated = sqlx::query(
            r#"UPDATE "RepoBindings"
                  SET scan_lease_token = $2,
                      scan_lease_until = clock_timestamp()
                          + make_interval(secs => $3),
                      scan_started_at_utc = clock_timestamp(),
                      scan_host_key = $4,
                      scan_slot = $5
                WHERE id = $1
                  AND (scan_lease_until IS NULL
                       OR scan_lease_until <= clock_timestamp())"#,
        )
        .bind(id)
        .bind(token)
        .bind(SCAN_LEASE_SECONDS)
        .bind(&host_key)
        .bind(slot)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if updated.rows_affected() == 1 {
            used_slots.insert(slot);
            used_hosts.insert(host_key);
            claimed.push((id, token));
        }
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(claimed)
}

pub(crate) async fn run_claimed_repo_scan(
    st: &SharedState,
    id: i32,
    lease_token: Uuid,
    allow_paused: bool,
) -> AppResult<RepoBindingScanResultModel> {
    let scan = run_repo_scan(st, id, allow_paused);
    let heartbeat = maintain_repo_scan_lease(st.pg(), id, lease_token);
    tokio::pin!(scan);
    tokio::pin!(heartbeat);
    let result = tokio::select! {
        result = &mut scan => result,
        lease = &mut heartbeat => match lease {
            Ok(()) => unreachable!("repository scan lease heartbeat is unbounded on success"),
            Err(error) => Err(error),
        },
    };
    let successful = result.as_ref().is_ok_and(|result| result.failures == 0);
    let error = result.as_ref().err().map(ToString::to_string);
    finish_repo_scan_lease(st.pg(), id, lease_token, successful, error.as_deref()).await?;
    result
}

async fn maintain_repo_scan_lease(
    pool: &sqlx::PgPool,
    id: i32,
    lease_token: Uuid,
) -> AppResult<()> {
    let mut last_success = std::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(SCAN_LEASE_RENEW_SECONDS)).await;
        match renew_repo_scan_lease(pool, id, lease_token).await {
            Ok(true) => last_success = std::time::Instant::now(),
            Ok(false) => return Err(AppError::conflict("Repository scan lease was lost")),
            Err(error)
                if last_success.elapsed()
                    < std::time::Duration::from_secs((SCAN_LEASE_SECONDS as u64) / 2) =>
            {
                tracing::warn!(binding_id = id, %error, "repository scan lease renewal failed; retrying within the current lease");
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) async fn renew_repo_scan_lease(
    pool: &sqlx::PgPool,
    id: i32,
    lease_token: Uuid,
) -> AppResult<bool> {
    let updated = sqlx::query(
        r#"UPDATE "RepoBindings"
              SET scan_lease_until = clock_timestamp() + make_interval(secs => $3)
            WHERE id = $1 AND scan_lease_token = $2
              AND scan_host_key IS NOT NULL AND scan_slot IS NOT NULL"#,
    )
    .bind(id)
    .bind(lease_token)
    .bind(SCAN_LEASE_SECONDS)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(updated.rows_affected() == 1)
}

pub(super) async fn finish_repo_scan_lease(
    pool: &sqlx::PgPool,
    id: i32,
    lease_token: Uuid,
    successful: bool,
    error: Option<&str>,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"UPDATE "RepoBindings"
              SET scan_lease_token = NULL, scan_lease_until = NULL,
                  scan_started_at_utc = NULL, scan_host_key = NULL,
                  scan_slot = NULL,
                  consecutive_scan_failures = CASE WHEN $3 THEN 0
                      ELSE consecutive_scan_failures + 1 END,
                  next_scan_utc = CASE WHEN $3 THEN next_scan_utc ELSE
                      clock_timestamp() + make_interval(secs =>
                          LEAST(3600, 30 * (1 << LEAST(consecutive_scan_failures, 6)))
                          + (id % 17)) END,
                  last_scan_message = COALESCE($4, last_scan_message)
            WHERE id = $1 AND scan_lease_token = $2"#,
    )
    .bind(id)
    .bind(lease_token)
    .bind(successful)
    .bind(error.map(|message| message.chars().take(2_000).collect::<String>()))
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() == 0 {
        return Err(AppError::conflict("Repository scan lease was lost"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::scan_host_key;

    #[test]
    fn host_admission_normalizes_case_and_ports_to_one_provider() {
        assert_eq!(
            scan_host_key("https://GitHub.COM:443/org/repo"),
            "github.com"
        );
        assert_eq!(
            scan_host_key("https://github.com/another/repo"),
            "github.com"
        );
        assert_eq!(scan_host_key("not a URL"), "invalid-repository-host");
    }
}
