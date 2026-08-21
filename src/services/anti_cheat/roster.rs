//! Identity-policy enforcement at the team-roster mutation boundary.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{
    database_error, database_now, find_conflict, kind_policy, lock_and_load_admission_policy,
    lock_identity_user_scope, lock_identity_values, prepare_identity, record_block,
    record_global_observations, validate_required_identity, AdmissionOutcome, Conflict,
    IdentitySource, PolicyFlags, PreparedIdentity, IDENTITY_WINDOW_HOURS,
};
use crate::models::internal::configs::AppConfig;
use crate::utils::error::{AppError, AppResult};

const MAX_RECENT_IDENTITY_CANDIDATES: i64 = 64;

async fn find_roster_conflict(
    transaction: &mut Transaction<'_, Postgres>,
    policy: PolicyFlags,
    user_id: Uuid,
    team_id: i32,
    identity: &PreparedIdentity,
    since: DateTime<Utc>,
) -> AppResult<Option<Conflict>> {
    let candidates = identity
        .values
        .iter()
        .filter(|value| kind_policy(policy, value.kind).0)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(None);
    }
    let kinds = candidates
        .iter()
        .map(|value| value.kind)
        .collect::<Vec<_>>();
    let hashes = candidates
        .iter()
        .map(|value| value.hash.clone())
        .collect::<Vec<_>>();
    let conflict = sqlx::query_as::<_, (Uuid, Option<String>, String, Vec<u8>)>(
        r#"WITH roster(team_id, user_id) AS (
                   SELECT team.id, team.captain_id FROM "Teams" team
                   UNION
                   SELECT member.team_id, member.user_id FROM "TeamMembers" member
               ), candidate(kind, value_hash, ordinality) AS (
                   SELECT value.kind, value.value_hash, value.ordinality
                     FROM UNNEST($3::TEXT[], $4::BYTEA[]) WITH ORDINALITY
                          AS value(kind, value_hash, ordinality)
               )
               SELECT observation.user_id, account.user_name,
                      candidate.kind, candidate.value_hash
                 FROM candidate
                 JOIN roster member ON member.team_id = $1
                 JOIN "IdentityObservations" observation
                   ON observation.user_id = member.user_id
                  AND observation.kind = candidate.kind
                  AND observation.value_hash = candidate.value_hash
                 JOIN "AspNetUsers" account ON account.id = observation.user_id
                WHERE member.user_id <> $2
                  AND observation.observed_at_utc > $5
                  AND NOT EXISTS (
                        SELECT 1
                          FROM "AntiCheatExemptions" exemption
                         WHERE exemption.user_a = LEAST($2, observation.user_id)
                           AND exemption.user_b = GREATEST($2, observation.user_id)
                           AND exemption.kind = observation.kind
                           AND exemption.value_hash = observation.value_hash
                           AND exemption.created_at_utc <= $5 + INTERVAL '24 hours'
                           AND exemption.expires_at_utc > $5 + INTERVAL '24 hours'
                           AND (exemption.revoked_at_utc IS NULL
                                OR exemption.revoked_at_utc > $5 + INTERVAL '24 hours')
                  )
                ORDER BY candidate.ordinality,
                         observation.observed_at_utc DESC, observation.id DESC
                LIMIT 1"#,
    )
    .bind(team_id)
    .bind(user_id)
    .bind(&kinds)
    .bind(&hashes)
    .bind(since)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    if let Some((conflict_user_id, conflict_user_name, kind, hash)) = conflict {
        let value = identity
            .values
            .iter()
            .find(|value| value.kind == kind && value.hash == hash)
            .cloned()
            .ok_or_else(|| AppError::internal("identity conflict candidate was not loaded"))?;
        return Ok(Some(Conflict {
            user_id: conflict_user_id,
            user_name: conflict_user_name,
            value,
        }));
    }
    Ok(None)
}

pub(super) async fn recent_joining_identities(
    transaction: &mut Transaction<'_, Postgres>,
    policy: PolicyFlags,
    user_id: Uuid,
    since: DateTime<Utc>,
) -> AppResult<Vec<(super::IdentityValue, DateTime<Utc>)>> {
    type Row = (
        String,
        Vec<u8>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        String,
        DateTime<Utc>,
    );
    let kinds = [
        policy.require_unique_ip_per_team_user.then_some("Ip"),
        policy
            .require_unique_fingerprint_per_team_user
            .then_some("Fingerprint"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, Row>(
        r#"SELECT kind, value_hash, subnet_group_hash, broad_network_hash,
                  value_hint, observed_at_utc
             FROM (
                   SELECT DISTINCT ON (kind, value_hash)
                          kind, value_hash, subnet_group_hash,
                          broad_network_hash, value_hint, observed_at_utc, id
                     FROM "IdentityObservations"
                    WHERE user_id = $1
                      AND team_id IS NULL AND game_id IS NULL
                      AND participation_id IS NULL
                      AND observed_at_utc > $2
                      AND kind = ANY($3)
                    ORDER BY kind, value_hash, observed_at_utc DESC, id DESC
             ) recent
            ORDER BY observed_at_utc DESC, id DESC
            LIMIT $4"#,
    )
    .bind(user_id)
    .bind(since)
    .bind(&kinds)
    .bind(MAX_RECENT_IDENTITY_CANDIDATES + 1)
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    if rows.len() > MAX_RECENT_IDENTITY_CANDIDATES as usize {
        return Err(AppError::bad_request(
            "Too many recent identity changes; wait before joining a team",
        ));
    }
    Ok(rows
        .into_iter()
        .filter_map(|(kind, hash, subnet, broad, hint, observed_at)| {
            let kind = match kind.as_str() {
                "Ip" => "Ip",
                "Fingerprint" => "Fingerprint",
                _ => return None,
            };
            Some((
                super::IdentityValue {
                    kind,
                    hash,
                    subnet_group_hash: subnet,
                    broad_network_hash: broad,
                    hint,
                },
                observed_at,
            ))
        })
        .collect())
}

/// Check the identity on the current team-join request. The caller must run
/// this inside the same roster transaction as the eventual `TeamMembers`
/// insert, and must lock the live account row only after this function so the
/// global lock order remains identity-user -> identity-hashes -> account row.
pub async fn admit_team_member_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    config: &AppConfig,
    user_id: Uuid,
    user_name: Option<&str>,
    team_id: i32,
    current_ip: Option<&str>,
    fingerprint: Option<&str>,
) -> AppResult<AdmissionOutcome> {
    let policy = lock_and_load_admission_policy(transaction).await?;
    let identity = prepare_identity(
        config.identity_hash_key.as_bytes(),
        current_ip,
        // If collection was disabled while a proof was being computed, the
        // canonical locked policy drops the now-unneeded fingerprint.
        policy
            .fingerprint_required()
            .then_some(fingerprint)
            .flatten(),
    );
    validate_required_identity(policy, &identity)?;
    lock_identity_user_scope(transaction, user_id).await?;
    let history_cutoff = database_now(transaction).await? - Duration::hours(IDENTITY_WINDOW_HOURS);
    let history = recent_joining_identities(transaction, policy, user_id, history_cutoff).await?;
    let mut identities_to_check = identity.clone();
    let mut seen = identities_to_check
        .values
        .iter()
        .map(|value| (value.kind, value.hash.clone()))
        .collect::<HashSet<_>>();
    for (value, _) in &history {
        if seen.insert((value.kind, value.hash.clone())) {
            identities_to_check.values.push(value.clone());
        }
    }
    lock_identity_values(transaction, &identities_to_check).await?;
    let now = database_now(transaction).await?;
    let since = now - Duration::hours(IDENTITY_WINDOW_HOURS);
    identities_to_check.values.retain(|value| {
        identity
            .values
            .iter()
            .any(|current| current.kind == value.kind && current.hash == value.hash)
            || history.iter().any(|(historic, observed_at)| {
                historic.kind == value.kind && historic.hash == value.hash && *observed_at > since
            })
    });
    // Re-apply global and already-shared-team rules to the identity on this
    // request before evaluating the target roster.
    if let Some(conflict) = find_conflict(transaction, policy, user_id, &identity, since).await? {
        record_block(transaction, user_id, user_name, &conflict, now).await?;
        return Ok(AdmissionOutcome::Blocked);
    }
    if let Some(conflict) = find_roster_conflict(
        transaction,
        policy,
        user_id,
        team_id,
        &identities_to_check,
        since,
    )
    .await?
    {
        record_block(transaction, user_id, user_name, &conflict, now).await?;
        return Ok(AdmissionOutcome::Blocked);
    }
    record_global_observations(
        transaction,
        user_id,
        &identity,
        IdentitySource::TeamJoin,
        now,
    )
    .await?;
    Ok(AdmissionOutcome::Accepted)
}
