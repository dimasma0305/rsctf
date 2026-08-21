//! Transactional IP/browser-identity admission and trusted client-IP parsing.
//!
//! Identity values are context for an explicit account policy, not proof that a
//! person cheated.  Successful admissions append timestamped, hashed
//! observations; rejected attempts append an `AntiCheatBlocks` audit row and do
//! not replace the account's last accepted identity.

use std::net::IpAddr;

use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::models::internal::configs::AppConfig;
use crate::services::captcha::CaptchaAdmission;
use crate::utils::error::{AppError, AppResult};

const IDENTITY_WINDOW_HOURS: i64 = 24;
const EXEMPTION_TTL_DAYS: i64 = 7;
const IDENTITY_BOOTSTRAP_LOCK_ID: i64 = 0x4944_4253_5452_5031; // "IDBSTRP1"
/// Domain/version marker shared by every durable identity correlation hash.
/// Changing this value would make old and new observations incomparable.
const IDENTITY_HASH_DOMAIN: &[u8] = b"rsctf-identity-observation-v1\0";

mod account_guard;
mod bootstrap;
pub use bootstrap::{bootstrap_legacy_identity_observations, ensure_identity_bootstrap_complete};
mod exemption;
pub use exemption::{exempt_block, ExemptionGrant};
mod fingerprint;
pub use fingerprint::{
    issue_fingerprint_challenge, validate_fingerprint_submission, FingerprintChallenge,
};
mod game_join;
pub(crate) use game_join::{
    evaluate_game_join_identity, lock_game_join_identity_scope, lock_game_join_observation_games,
    record_game_join_identity_decision, snapshot_recent_global_observations_for_game,
};
mod network;
pub use network::{
    client_ip, configured_trusted_proxy_cidrs, is_trusted_proxy, redacted_identity_hint,
    validate_trusted_proxy_config,
};
mod policy;
pub use policy::load_policy_flags;
pub(crate) use policy::{
    authorize_captcha_admission, load_account_policy_after_lock, lock_and_load_account_policy,
    lock_and_load_admission_policy, lock_policy_update,
};
mod roster;
pub use roster::admit_team_member_in_transaction;
#[cfg(test)]
mod admission_edge_tests;
#[cfg(test)]
mod auth_race_tests;
#[cfg(test)]
mod bootstrap_tests;
#[cfg(test)]
mod db_tests;
#[cfg(test)]
mod exemption_tests;
#[cfg(test)]
mod unit_tests;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PolicyFlags {
    pub enable_browser_fingerprint: bool,
    pub require_unique_ip_per_team_user: bool,
    pub require_unique_fingerprint_per_team_user: bool,
    pub require_unique_ip_global: bool,
    pub require_unique_fingerprint_global: bool,
}

impl PolicyFlags {
    pub fn fingerprint_required(self) -> bool {
        self.enable_browser_fingerprint
            || self.require_unique_fingerprint_per_team_user
            || self.require_unique_fingerprint_global
    }

    pub fn validate(self) -> AppResult<()> {
        if (self.require_unique_fingerprint_per_team_user || self.require_unique_fingerprint_global)
            && !self.enable_browser_fingerprint
        {
            return Err(AppError::bad_request(
                "Browser fingerprinting must be enabled before a fingerprint uniqueness policy",
            ));
        }
        Ok(())
    }
}

/// Mark an account insert as intentionally identity-neutral for this SQL
/// transaction. This is limited to pending OAuth/password accounts and
/// administrator provisioning; it never admits a session.
pub(crate) async fn mark_identity_neutral_insert(
    transaction: &mut Transaction<'_, Postgres>,
) -> AppResult<()> {
    sqlx::query("SELECT set_config('rsctf.identity_neutral_insert', '1', true)")
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    Registration,
    Password,
    OAuth,
    TeamJoin,
    GameJoin,
    Legacy,
}

impl IdentitySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Registration => "Registration",
            Self::Password => "Password",
            Self::OAuth => "OAuth",
            Self::TeamJoin => "TeamJoin",
            Self::GameJoin => "GameJoin",
            Self::Legacy => "Legacy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionOutcome {
    Accepted,
    Blocked,
}

#[derive(Debug, Clone)]
struct IdentityValue {
    kind: &'static str,
    hash: Vec<u8>,
    subnet_group_hash: Option<Vec<u8>>,
    broad_network_hash: Option<Vec<u8>>,
    hint: String,
}

#[derive(Debug, Clone, Default)]
struct PreparedIdentity {
    ip: Option<String>,
    values: Vec<IdentityValue>,
}

#[derive(Debug)]
struct Conflict {
    user_id: Uuid,
    user_name: Option<String>,
    value: IdentityValue,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

pub fn valid_browser_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hash_value(secret: &[u8], kind: &str, value: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)
        .expect("HMAC-SHA256 accepts deployment secrets of every length");
    mac.update(IDENTITY_HASH_DOMAIN);
    mac.update(kind.as_bytes());
    mac.update(b"\0");
    mac.update(value.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Canonical deployment-keyed hashes for an IP identity. Durable callers such
/// as submission and access-event evidence must use this API so their hashes
/// remain comparable with `IdentityObservations` without retaining the raw IP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IpIdentityHashes {
    pub normalized: String,
    pub exact: Vec<u8>,
    pub subnet_group: Vec<u8>,
    pub broad_network: Vec<u8>,
}

pub(crate) fn hash_ip_identity(config: &AppConfig, raw_ip: &str) -> Option<IpIdentityHashes> {
    hash_ip_identity_with_key(config.identity_hash_key.as_bytes(), raw_ip)
}

fn hash_ip_identity_with_key(identity_key: &[u8], raw_ip: &str) -> Option<IpIdentityHashes> {
    let parsed = normalize_ip_addr(parse_ip(raw_ip)?);
    let normalized = normalize_ip(parsed);
    let (subnet, broad) = match parsed {
        IpAddr::V4(_) => (network_prefix(parsed, 28), network_prefix(parsed, 20)),
        IpAddr::V6(_) => (network_prefix(parsed, 64), network_prefix(parsed, 48)),
    };
    Some(IpIdentityHashes {
        exact: hash_value(identity_key, "Ip", &normalized),
        subnet_group: hash_value(identity_key, "IpSubnetGroup", &subnet),
        broad_network: hash_value(identity_key, "IpBroadNetwork", &broad),
        normalized,
    })
}

fn fingerprint_hint(hash: &[u8]) -> String {
    format!("{}…", hex::encode(&hash[..6]))
}

fn network_prefix(ip: IpAddr, prefix: u8) -> String {
    match ip {
        IpAddr::V4(address) => {
            let bits = u32::from(address);
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            format!("{}/{}", std::net::Ipv4Addr::from(bits & mask), prefix)
        }
        IpAddr::V6(address) => {
            let bits = u128::from(address);
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            format!("{}/{}", std::net::Ipv6Addr::from(bits & mask), prefix)
        }
    }
}

fn ip_hint(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(address) => {
            let [a, b, c, _] = address.octets();
            format!("{a}.{b}.{c}.x")
        }
        IpAddr::V6(address) => network_prefix(IpAddr::V6(address), 64),
    }
}

fn prepare_identity(
    identity_key: &[u8],
    current_ip: Option<&str>,
    fingerprint: Option<&str>,
) -> PreparedIdentity {
    let parsed_ip = current_ip
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(parse_ip)
        .map(normalize_ip_addr);
    let ip = parsed_ip.map(normalize_ip);
    let fingerprint = fingerprint
        .map(str::trim)
        .filter(|value| valid_browser_fingerprint(value))
        .map(str::to_owned);
    let mut values = Vec::with_capacity(2);
    if let (Some(parsed_ip), Some(ip)) = (parsed_ip, &ip) {
        let hashes = hash_ip_identity_with_key(identity_key, ip)
            .expect("an already parsed canonical IP remains valid");
        values.push(IdentityValue {
            kind: "Ip",
            hash: hashes.exact,
            subnet_group_hash: Some(hashes.subnet_group),
            broad_network_hash: Some(hashes.broad_network),
            hint: ip_hint(parsed_ip),
        });
    }
    if let Some(fingerprint) = &fingerprint {
        let hash = hash_value(identity_key, "Fingerprint", fingerprint);
        values.push(IdentityValue {
            kind: "Fingerprint",
            hint: fingerprint_hint(&hash),
            hash,
            subnet_group_hash: None,
            broad_network_hash: None,
        });
    }
    PreparedIdentity { ip, values }
}

fn validate_required_identity(policy: PolicyFlags, identity: &PreparedIdentity) -> AppResult<()> {
    let has_kind = |kind: &str| identity.values.iter().any(|value| value.kind == kind);
    if (policy.require_unique_ip_per_team_user || policy.require_unique_ip_global)
        && !has_kind("Ip")
    {
        return Err(AppError::bad_request(
            "A valid client IP is required by the account integrity policy.",
        ));
    }
    if policy.fingerprint_required() && !has_kind("Fingerprint") {
        return Err(AppError::bad_request(
            "A fresh browser fingerprint proof is required by the account integrity policy.",
        ));
    }
    Ok(())
}

fn identity_lock_key(hash: &[u8]) -> i64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&hash[..8]);
    i64::from_be_bytes(bytes)
}

fn identity_user_lock_key(user_id: Uuid) -> i64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&user_id.as_bytes()[..8]);
    i64::from_be_bytes(bytes) ^ 0x4954_5955_5345_5200_i64 // "ITYUSER\0"
}

async fn lock_identity_user(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(identity_user_lock_key(user_id))
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    Ok(())
}

async fn lock_identity_bootstrap_shared(
    transaction: &mut Transaction<'_, Postgres>,
) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(IDENTITY_BOOTSTRAP_LOCK_ID)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    Ok(())
}

/// Acquire the canonical pre-game identity scope for one authenticated user:
/// bootstrap stability first, then the user-exclusive identity advisory lock.
/// Callers must invoke this before locking Games, Participations, membership,
/// or account rows so identity writers and telemetry cannot deadlock.
pub(crate) async fn lock_identity_user_scope(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> AppResult<()> {
    lock_identity_bootstrap_shared(transaction).await?;
    lock_identity_user(transaction, user_id).await
}

/// Lock and revalidate the account backing an authenticated request. Call only
/// after the canonical identity/Game locks for the operation are held.
pub(crate) async fn lock_live_request_account(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    expected_security_stamp: &str,
) -> AppResult<()> {
    account_guard::lock_live_request_account(transaction, user_id, expected_security_stamp).await
}

/// Opaque identity state held across the ordered game-join transaction. The
/// policy and identity hashes are fixed only after the shared policy lock, and
/// all of their advisory locks remain held until the caller commits.
async fn lock_identity_values(
    transaction: &mut Transaction<'_, Postgres>,
    identity: &PreparedIdentity,
) -> AppResult<()> {
    let mut keys = identity
        .values
        .iter()
        .map(|value| identity_lock_key(&value.hash))
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys.dedup();
    for key in keys {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(key)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
    }
    Ok(())
}

fn kind_policy(policy: PolicyFlags, kind: &str) -> (bool, bool) {
    match kind {
        "Ip" => (
            policy.require_unique_ip_per_team_user,
            policy.require_unique_ip_global,
        ),
        "Fingerprint" => (
            policy.require_unique_fingerprint_per_team_user,
            policy.require_unique_fingerprint_global,
        ),
        _ => (false, false),
    }
}

async fn find_conflict(
    transaction: &mut Transaction<'_, Postgres>,
    policy: PolicyFlags,
    user_id: Uuid,
    identity: &PreparedIdentity,
    since: DateTime<Utc>,
) -> AppResult<Option<Conflict>> {
    // Preserve the established, deterministic IP-before-fingerprint precedence.
    for value in &identity.values {
        let (per_team, global) = kind_policy(policy, value.kind);
        if !per_team && !global {
            continue;
        }
        let conflict = sqlx::query_as::<_, (Uuid, Option<String>)>(
            r#"WITH roster(team_id, user_id) AS (
                   SELECT team.id, team.captain_id FROM "Teams" team
                   UNION
                   SELECT member.team_id, member.user_id FROM "TeamMembers" member
               )
               SELECT observation.user_id, account.user_name
                 FROM "IdentityObservations" observation
                 JOIN "AspNetUsers" account ON account.id = observation.user_id
                WHERE observation.user_id <> $1
                  AND observation.kind = $2
                  AND observation.value_hash = $3
                  AND observation.observed_at_utc > $4
                  AND (
                        $5
                        OR EXISTS (
                            SELECT 1
                              FROM roster mine
                              JOIN roster theirs
                                ON theirs.team_id = mine.team_id
                               AND theirs.user_id = observation.user_id
                             WHERE mine.user_id = $1
                        )
                  )
                  AND NOT EXISTS (
                        SELECT 1
                          FROM "AntiCheatExemptions" exemption
                         WHERE exemption.user_a = LEAST($1, observation.user_id)
                           AND exemption.user_b = GREATEST($1, observation.user_id)
                           AND exemption.kind = observation.kind
                           AND exemption.value_hash = observation.value_hash
                           AND exemption.created_at_utc <= $4 + INTERVAL '24 hours'
                           AND exemption.expires_at_utc > $4 + INTERVAL '24 hours'
                           AND (exemption.revoked_at_utc IS NULL
                                OR exemption.revoked_at_utc > $4 + INTERVAL '24 hours')
                  )
                ORDER BY observation.observed_at_utc DESC, observation.id DESC
                LIMIT 1"#,
        )
        .bind(user_id)
        .bind(value.kind)
        .bind(&value.hash)
        .bind(since)
        .bind(global)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?;
        if let Some((conflict_user_id, conflict_user_name)) = conflict {
            return Ok(Some(Conflict {
                user_id: conflict_user_id,
                user_name: conflict_user_name,
                value: value.clone(),
            }));
        }
    }
    Ok(None)
}

async fn record_block(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    user_name: Option<&str>,
    conflict: &Conflict,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "AntiCheatBlocks"
             (user_id, user_name, conflict_user_id, conflict_user_name, kind,
              conflicting_value, conflicting_value_hash, occurred_at_utc,
              adjudicated_at_utc, adjudicated_by_user_id, exemption_expires_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, NULL, NULL)"#,
    )
    .bind(user_id)
    .bind(user_name)
    .bind(conflict.user_id)
    .bind(&conflict.user_name)
    .bind(conflict.value.kind)
    .bind(&conflict.value.hint)
    .bind(&conflict.value.hash)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn record_observations(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    identity: &PreparedIdentity,
    source: IdentitySource,
    now: DateTime<Utc>,
    locked_game_ids: Option<&[i32]>,
) -> AppResult<()> {
    for value in &identity.values {
        sqlx::query(
            r#"WITH contexts AS MATERIALIZED (
                   SELECT user_participation.team_id, user_participation.game_id,
                          user_participation.participation_id
                     FROM "UserParticipations" user_participation
                     JOIN "Participations" participation
                       ON participation.id = user_participation.participation_id
                      AND participation.game_id = user_participation.game_id
                      AND participation.team_id = user_participation.team_id
                     JOIN "Teams" team
                       ON team.id = user_participation.team_id
                      AND team.deletion_pending = FALSE
                     JOIN "Games" game ON game.id = user_participation.game_id
                    WHERE user_participation.user_id = $1
                      AND (
                            team.captain_id = $1
                            OR EXISTS (
                                SELECT 1 FROM "TeamMembers" member
                                 WHERE member.team_id = team.id
                                   AND member.user_id = $1
                            )
                      )
                      AND participation.status IN ($9, $10)
                      AND game.deletion_pending = FALSE
                      AND NOT EXISTS (
                            SELECT 1
                              FROM "SuspicionReconciliationState" reconciliation
                             WHERE reconciliation.game_id = game.id
                               AND reconciliation.evidence_closed_at_utc IS NOT NULL
                      )
                      AND game.start_time_utc <= $8
                      AND game.end_time_utc > $8
                      AND ($11::INTEGER[] IS NULL OR game.id = ANY($11))
               ), snapshots AS (
                   SELECT NULL::INTEGER AS team_id, NULL::INTEGER AS game_id,
                          NULL::INTEGER AS participation_id
                   UNION ALL
                   SELECT team_id, game_id, participation_id FROM contexts
               )
               INSERT INTO "IdentityObservations"
                    (user_id, team_id, game_id, participation_id, kind, value_hash,
                     subnet_group_hash, broad_network_hash, value_hint, source,
                     observed_at_utc)
               SELECT $1, team_id, game_id, participation_id, $2, $3, $4, $5,
                      $6, $7, $8
                 FROM snapshots"#,
        )
        .bind(user_id)
        .bind(value.kind)
        .bind(&value.hash)
        .bind(&value.subnet_group_hash)
        .bind(&value.broad_network_hash)
        .bind(&value.hint)
        .bind(source.as_str())
        .bind(now)
        .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
        .bind(crate::utils::enums::ParticipationStatus::Suspended as i16)
        .bind(locked_game_ids)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

async fn record_global_observations(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    identity: &PreparedIdentity,
    source: IdentitySource,
    now: DateTime<Utc>,
) -> AppResult<()> {
    for value in &identity.values {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind, value_hash,
                  subnet_group_hash, broad_network_hash, value_hint, source,
                  observed_at_utc)
               VALUES ($1, NULL, NULL, NULL, $2, $3, $4, $5, $6, $7, $8)"#,
        )
        .bind(user_id)
        .bind(value.kind)
        .bind(&value.hash)
        .bind(&value.subnet_group_hash)
        .bind(&value.broad_network_hash)
        .bind(&value.hint)
        .bind(source.as_str())
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

async fn lock_observation_games(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    additional_game_id: Option<i32>,
) -> AppResult<Vec<i32>> {
    // Lock every linked, unsealed participation Game in primary-key order,
    // independent of its current time window, and keep the locks through
    // commit. Time is assigned only afterward and record_observations applies
    // [start,end) against that locked id set. This prevents a concurrent start
    // edit or natural boundary crossing from escaping the finalizer barrier.
    let game_ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT game.id
             FROM "Games" game
            WHERE game.deletion_pending = FALSE
              AND NOT EXISTS (
                    SELECT 1
                      FROM "SuspicionReconciliationState" reconciliation
                     WHERE reconciliation.game_id = game.id
                       AND reconciliation.evidence_closed_at_utc IS NOT NULL
              )
              AND (
                  game.id = $2
                  OR EXISTS (
                    SELECT 1
                      FROM "UserParticipations" user_participation
                      JOIN "Participations" participation
                        ON participation.id = user_participation.participation_id
                       AND participation.game_id = user_participation.game_id
                       AND participation.team_id = user_participation.team_id
                      JOIN "Teams" team
                        ON team.id = user_participation.team_id
                       AND team.deletion_pending = FALSE
                     WHERE user_participation.user_id = $1
                       AND user_participation.game_id = game.id
                       AND (
                            team.captain_id = $1
                            OR EXISTS (
                                SELECT 1 FROM "TeamMembers" member
                                 WHERE member.team_id = team.id
                                   AND member.user_id = $1
                            )
                       )
                  )
              )
            ORDER BY game.id
            FOR SHARE OF game"#,
    )
    .bind(user_id)
    .bind(additional_game_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(game_ids)
}

async fn database_now(transaction: &mut Transaction<'_, Postgres>) -> AppResult<DateTime<Utc>> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)
}

#[derive(Debug, Clone, Copy)]
struct AdmissionDecision {
    outcome: AdmissionOutcome,
    observed_at: DateTime<Utc>,
}

#[derive(Clone, Copy)]
struct AdmissionContext<'a> {
    policy: PolicyFlags,
    user_id: Uuid,
    user_name: Option<&'a str>,
    identity: &'a PreparedIdentity,
    source: IdentitySource,
}

#[derive(Clone, Copy)]
struct ExistingAccountGuard<'a> {
    security_stamp: &'a str,
    normalized_email: Option<&'a str>,
}

async fn adjudicate_at(
    transaction: &mut Transaction<'_, Postgres>,
    admission: AdmissionContext<'_>,
    now: DateTime<Utc>,
    locked_game_ids: Option<&[i32]>,
) -> AppResult<AdmissionOutcome> {
    let since = now - Duration::hours(IDENTITY_WINDOW_HOURS);
    if let Some(conflict) = find_conflict(
        transaction,
        admission.policy,
        admission.user_id,
        admission.identity,
        since,
    )
    .await?
    {
        record_block(
            transaction,
            admission.user_id,
            admission.user_name,
            &conflict,
            now,
        )
        .await?;
        return Ok(AdmissionOutcome::Blocked);
    }
    record_observations(
        transaction,
        admission.user_id,
        admission.identity,
        admission.source,
        now,
        locked_game_ids,
    )
    .await?;
    Ok(AdmissionOutcome::Accepted)
}

#[cfg(test)]
async fn evaluate_admission(
    transaction: &mut Transaction<'_, Postgres>,
    policy: PolicyFlags,
    user_id: Uuid,
    user_name: Option<&str>,
    identity: &PreparedIdentity,
    source: IdentitySource,
    now: DateTime<Utc>,
) -> AppResult<AdmissionOutcome> {
    policy.validate()?;
    // This user-scoped lock closes the operation-ordering race between a login
    // that accepts a new identity and a concurrent team-roster admission.
    lock_identity_user_scope(transaction, user_id).await?;
    lock_identity_values(transaction, identity).await?;
    let admission = AdmissionContext {
        policy,
        user_id,
        user_name,
        identity,
        source,
    };
    adjudicate_at(transaction, admission, now, None).await
}

async fn evaluate_canonical_admission(
    transaction: &mut Transaction<'_, Postgres>,
    admission: AdmissionContext<'_>,
    account_guard: Option<ExistingAccountGuard<'_>>,
) -> AppResult<AdmissionDecision> {
    admission.policy.validate()?;
    lock_identity_user_scope(transaction, admission.user_id).await?;
    lock_identity_values(transaction, admission.identity).await?;
    let locked_game_ids = lock_observation_games(transaction, admission.user_id, None).await?;
    if let Some(account_guard) = account_guard {
        account_guard::lock_live_existing_account(
            transaction,
            admission.user_id,
            account_guard.security_stamp,
            account_guard.normalized_email,
        )
        .await?;
    }
    // Assign the authoritative timestamp only after any finalizer wait. The
    // context query below then re-evaluates the strict [start,end) window.
    let observed_at = database_now(transaction).await?;
    let outcome =
        adjudicate_at(transaction, admission, observed_at, Some(&locked_game_ids)).await?;
    Ok(AdmissionDecision {
        outcome,
        observed_at,
    })
}

/// Admit and update an existing account in one SQL transaction.  A rejection
/// commits only its audit row; the account's accepted IP/fingerprint and sign-in
/// timestamp remain untouched.
#[allow(clippy::too_many_arguments)]
pub async fn admit_existing_user(
    pool: &sqlx::PgPool,
    config: &AppConfig,
    user_id: Uuid,
    user_name: Option<&str>,
    current_ip: Option<&str>,
    fingerprint: Option<&str>,
    source: IdentitySource,
    fallback_security_stamp: &str,
    expected_normalized_email: Option<&str>,
    captcha_admission: CaptchaAdmission,
) -> AppResult<AdmissionOutcome> {
    let mut transaction = pool.begin().await.map_err(database_error)?;
    let account_policy = lock_and_load_account_policy(&mut transaction, config).await?;
    account_policy.authorize_captcha(captcha_admission)?;
    let policy = account_policy.identity;
    let identity = prepare_identity(
        config.identity_hash_key.as_bytes(),
        current_ip,
        policy
            .fingerprint_required()
            .then_some(fingerprint)
            .flatten(),
    );
    validate_required_identity(policy, &identity)?;
    let decision = evaluate_canonical_admission(
        &mut transaction,
        AdmissionContext {
            policy,
            user_id,
            user_name,
            identity: &identity,
            source,
        },
        Some(ExistingAccountGuard {
            security_stamp: fallback_security_stamp,
            normalized_email: expected_normalized_email,
        }),
    )
    .await?;
    if decision.outcome == AdmissionOutcome::Blocked {
        transaction.commit().await.map_err(database_error)?;
        return Ok(decision.outcome);
    }

    let updated = sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET last_signed_in_utc = $2,
                  security_stamp = COALESCE(NULLIF(security_stamp, ''), $3),
                  ip = COALESCE($4, ip)
            WHERE id = $1"#,
    )
    .bind(user_id)
    .bind(decision.observed_at)
    .bind(fallback_security_stamp)
    .bind(identity.ip.as_deref())
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("User not found"));
    }
    transaction.commit().await.map_err(database_error)?;
    Ok(decision.outcome)
}

/// Evaluate a not-yet-inserted account inside its caller-owned transaction.
/// On acceptance, observations are inserted immediately and roll back if the
/// later account insert fails. On rejection, registration callers retain the
/// same account id in that transaction but issue no session; this lets a later
/// pair-scoped adjudication apply to the account without creating an accepted
/// identity observation.
#[allow(clippy::too_many_arguments)]
pub async fn admit_new_user_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    config: &AppConfig,
    user_id: Uuid,
    user_name: Option<&str>,
    current_ip: Option<&str>,
    fingerprint: Option<&str>,
    source: IdentitySource,
) -> AppResult<AdmissionOutcome> {
    let policy = lock_and_load_admission_policy(transaction).await?;
    let identity = prepare_identity(
        config.identity_hash_key.as_bytes(),
        current_ip,
        policy
            .fingerprint_required()
            .then_some(fingerprint)
            .flatten(),
    );
    validate_required_identity(policy, &identity)?;
    Ok(evaluate_canonical_admission(
        transaction,
        AdmissionContext {
            policy,
            user_id,
            user_name,
            identity: &identity,
            source,
        },
        None,
    )
    .await?
    .outcome)
}

pub fn block_message() -> &'static str {
    "Sign-in was blocked by the account integrity policy. Contact an administrator if this is expected."
}

fn parse_ip(value: &str) -> Option<IpAddr> {
    value.trim().parse().ok()
}

pub fn normalize_ip(ip: IpAddr) -> String {
    normalize_ip_addr(ip).to_string()
}

fn normalize_ip_addr(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        IpAddr::V4(v4) => IpAddr::V4(v4),
    }
}
