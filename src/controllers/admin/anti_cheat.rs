//! Immutable stolen-flag evidence and anti-cheat policy adjudication.

use super::*;
use sha2::{Digest, Sha256};

const MAX_PAGE_SIZE: u64 = 500;
const MAX_PAGE_OFFSET: u64 = 1_000_000;

fn bounded_page(count: u64, skip: u64) -> (i64, i64) {
    (
        count.clamp(1, MAX_PAGE_SIZE) as i64,
        skip.min(MAX_PAGE_OFFSET) as i64,
    )
}

// ─── Cheat reports ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationModel {
    pub id: i32,
    pub team: Option<TeamModel>,
    pub status: ParticipationStatus,
    pub division: Option<String>,
    pub division_id: Option<i32>,
}

/// One canonical stolen-flag incident. Behavioral `SuspicionEvents` are
/// intentionally reported only by the per-game suspicion roster.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatInfoModel {
    pub owned_team: Option<ParticipationModel>,
    pub submit_team: Option<ParticipationModel>,
    pub submission: Option<crate::controllers::game::SubmissionModel>,
}

fn participation_model(
    id: i32,
    team_id: i32,
    team_name: String,
    avatar_hash: Option<String>,
    status: i16,
    division_id: Option<i32>,
    division: Option<String>,
) -> AppResult<ParticipationModel> {
    Ok(ParticipationModel {
        id,
        team: Some(TeamModel {
            id: team_id,
            name: team_name,
            avatar: crate::controllers::game::cheat::cheat_avatar_url(&avatar_hash),
        }),
        status: crate::controllers::game::cheat::cheat_participation_status(status)?,
        division,
        division_id,
    })
}

fn cheat_info_model(
    row: crate::controllers::game::cheat::CheatIncidentRow,
) -> AppResult<CheatInfoModel> {
    let owned_team = participation_model(
        row.source_participation_id,
        row.source_team_id,
        row.source_team_name,
        row.source_avatar_hash,
        row.source_status,
        row.source_division_id,
        row.source_division_name,
    )?;
    let submit_team_name = row.submit_team_name.clone();
    let submit_team = participation_model(
        row.submit_participation_id,
        row.submit_team_id,
        row.submit_team_name,
        row.submit_avatar_hash,
        row.submit_status,
        row.submit_division_id,
        row.submit_division_name,
    )?;
    let submission = crate::controllers::game::SubmissionModel {
        answer: row.answer,
        status: crate::controllers::game::cheat::cheat_answer_result(row.answer_status)?,
        time: row.submit_time_utc,
        user: row.user_name,
        team: Some(submit_team_name),
        challenge: Some(row.challenge_title),
    };
    Ok(CheatInfoModel {
        owned_team: Some(owned_team),
        submit_team: Some(submit_team),
        submission: Some(submission),
    })
}

/// `GET /api/admin/cheat-reports` — immutable stolen-flag incidents only,
/// newest first with stable id tie-breaking.
pub async fn cheat_reports(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<RequestResponse<Vec<CheatInfoModel>>> {
    let (count, skip) = bounded_page(q.count, q.skip);
    let data =
        crate::controllers::game::cheat::load_cheat_incident_rows(st.pg(), None, Some(count), skip)
            .await?
            .into_iter()
            .map(cheat_info_model)
            .collect::<AppResult<Vec<_>>>()?;
    Ok(RequestResponse::ok(data))
}

// ─── Anti-cheat blocks ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AntiCheatBlockModel {
    pub id: i32,
    pub user_id: String,
    pub user_name: Option<String>,
    pub conflict_user_id: Option<String>,
    pub conflict_user_name: Option<String>,
    pub kind: String,
    pub conflicting_value: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub occurred_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub adjudicated_at_utc: Option<DateTime<Utc>>,
    pub adjudicated_by_user_id: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub exemption_expires_at_utc: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct AntiCheatBlockRow {
    id: i32,
    user_id: Uuid,
    user_name: Option<String>,
    conflict_user_id: Option<Uuid>,
    conflict_user_name: Option<String>,
    kind: String,
    conflicting_value: Option<String>,
    occurred_at_utc: DateTime<Utc>,
    adjudicated_at_utc: Option<DateTime<Utc>>,
    adjudicated_by_user_id: Option<Uuid>,
    exemption_expires_at_utc: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntiCheatBlocksQuery {
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
}

impl From<AntiCheatBlockRow> for AntiCheatBlockModel {
    fn from(row: AntiCheatBlockRow) -> Self {
        let conflicting_value = row
            .conflicting_value
            .as_deref()
            .map(|value| crate::services::anti_cheat::redacted_identity_hint(&row.kind, value));
        Self {
            id: row.id,
            user_id: row.user_id.to_string(),
            user_name: row.user_name,
            conflict_user_id: row.conflict_user_id.map(|user| user.to_string()),
            conflict_user_name: row.conflict_user_name,
            kind: row.kind,
            conflicting_value,
            occurred_at_utc: row.occurred_at_utc,
            adjudicated_at_utc: row.adjudicated_at_utc,
            adjudicated_by_user_id: row.adjudicated_by_user_id.map(|user| user.to_string()),
            exemption_expires_at_utc: row.exemption_expires_at_utc,
        }
    }
}

/// `GET /api/admin/anticheatblocks?count=&skip=` — retained conflict history.
pub async fn list_anti_cheat_blocks(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<AntiCheatBlocksQuery>,
) -> AppResult<RequestResponse<Vec<AntiCheatBlockModel>>> {
    let (count, skip) = bounded_page(q.count, q.skip);
    let rows = sqlx::query_as::<_, AntiCheatBlockRow>(
        r#"SELECT id, user_id, user_name, conflict_user_id, conflict_user_name,
                  kind, conflicting_value, occurred_at_utc, adjudicated_at_utc,
                  adjudicated_by_user_id, exemption_expires_at_utc
             FROM "AntiCheatBlocks"
            ORDER BY occurred_at_utc DESC, id DESC
            LIMIT $1 OFFSET $2"#,
    )
    .bind(count)
    .bind(skip)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RequestResponse::ok(
        rows.into_iter().map(Into::into).collect(),
    ))
}

/// `DELETE /api/admin/anticheatblocks/{id}` — retain the audit row and grant a
/// seven-day exemption scoped to its exact account pair, kind and value hash.
pub async fn delete_anti_cheat_block(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<i32>,
) -> AppResult<MessageResponse> {
    let grant =
        crate::services::anti_cheat::exempt_block(st.pg(), st.config.as_ref(), id, admin.id)
            .await?;
    Ok(MessageResponse::ok(format!(
        "Exemption granted until {}.",
        grant.expires_at_utc.to_rfc3339()
    )))
}

// ─── Bounded event-network telemetry and evidence fusion ──────────────────

pub async fn derive_event_security_findings(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(game_id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<(
    StatusCode,
    RequestResponse<crate::services::control_jobs::ControlJobModel>,
)> {
    let operation = crate::controllers::edit::control_jobs::operation_id(&headers)?;
    let input = serde_json::json!({ "gameId": game_id });
    let fingerprint = crate::controllers::edit::control_jobs::fingerprint(&input)?;
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        crate::services::control_jobs::ControlJobKind::SecurityDerivation,
        &format!("game:{game_id}"),
        game_id,
        None,
        operation,
        &fingerprint,
        input,
    )
    .await?;
    crate::services::control_jobs::kick(st);
    Ok((StatusCode::ACCEPTED, RequestResponse::ok(job)))
}

pub async fn fused_event_security_breakdown(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path((game_id, participation_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<crate::services::event_security::FusedEvidenceBreakdown>> {
    Ok(RequestResponse::ok(
        crate::services::event_security::fused_breakdown(&st, game_id, participation_id).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingReviewRequest {
    pub status: crate::services::event_security::FindingReviewStatus,
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn review_event_security_finding(
    State(st): State<SharedState>,
    admin: AdminUser,
    Path((game_id, finding_id)): Path<(i32, i64)>,
    Json(request): Json<FindingReviewRequest>,
) -> AppResult<MessageResponse> {
    crate::services::event_security::review_finding(
        &st,
        game_id,
        finding_id,
        admin.0.id,
        request.status,
        request.note.as_deref(),
    )
    .await?;
    Ok(MessageResponse::ok("Finding review recorded"))
}

#[derive(Debug, Deserialize)]
pub struct ReasonRequest {
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryPurgeResult {
    pub rows_removed: i64,
    pub logical_bytes_removed: i64,
}

pub async fn purge_event_security_telemetry(
    State(st): State<SharedState>,
    admin: AdminUser,
    Path(game_id): Path<i32>,
    Json(request): Json<ReasonRequest>,
) -> AppResult<RequestResponse<TelemetryPurgeResult>> {
    let (rows_removed, logical_bytes_removed) =
        crate::services::event_security::purge_game_telemetry(
            &st,
            game_id,
            admin.0.id,
            &request.reason,
        )
        .await?;
    Ok(RequestResponse::ok(TelemetryPurgeResult {
        rows_removed,
        logical_bytes_removed,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnOverrideRequest {
    pub reason: String,
    pub duration_minutes: i32,
    pub operation_id: Uuid,
    pub expected_policy_revision: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnOverrideResult {
    pub id: Uuid,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expires_at_utc: DateTime<Utc>,
    pub policy_revision: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnOverrideRevokeRequest {
    pub operation_id: Uuid,
    pub expected_policy_revision: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct VpnOverrideModel {
    pub id: Uuid,
    pub reason: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub created_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expires_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub revoked_at_utc: Option<DateTime<Utc>>,
    pub active: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnOverrideList {
    pub policy_revision: i64,
    pub active_limit: i32,
    pub overrides: Vec<VpnOverrideModel>,
}

#[derive(sqlx::FromRow)]
struct StoredOverrideOperation {
    actor_user_id: Uuid,
    action: String,
    override_id: Uuid,
    request_digest: Vec<u8>,
    result_revision: i64,
    expires_at_utc: DateTime<Utc>,
}

const MAX_ACTIVE_VPN_OVERRIDES: i64 = 16;
const VPN_OVERRIDE_HISTORY_LIMIT: i64 = 100;
const VPN_OVERRIDE_MAINTENANCE_LIMIT: i64 = 128;

fn override_request_digest(action: &str, reason: &str, duration: i32, id: Option<Uuid>) -> Vec<u8> {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:event-vpn-override-operation:v1\0");
    digest.update(action.as_bytes());
    digest.update([0]);
    digest.update(reason.as_bytes());
    digest.update(duration.to_be_bytes());
    if let Some(id) = id {
        digest.update(id.as_bytes());
    }
    digest.finalize().to_vec()
}

async fn stored_override_operation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    operation_id: Uuid,
) -> AppResult<Option<StoredOverrideOperation>> {
    sqlx::query_as::<_, StoredOverrideOperation>(
        r#"SELECT operation.actor_user_id, operation.action, operation.override_id,
                  operation.request_digest, operation.result_revision,
                  override.expires_at_utc
             FROM "EventVpnOverrideOperations" operation
             JOIN "EventVpnGateOverrides" override
               ON override.id = operation.override_id
            WHERE operation.game_id = $1 AND operation.operation_id = $2"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn maintain_override_history(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> AppResult<()> {
    sqlx::query(
        r#"WITH expired AS (
               SELECT game_id, operation_id
                 FROM "EventVpnOverrideOperations"
                WHERE created_at_utc < clock_timestamp() - INTERVAL '30 days'
                ORDER BY created_at_utc, game_id, operation_id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
           )
           DELETE FROM "EventVpnOverrideOperations" operation
            USING expired
            WHERE operation.game_id = expired.game_id
              AND operation.operation_id = expired.operation_id"#,
    )
    .bind(VPN_OVERRIDE_MAINTENANCE_LIMIT)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"WITH expired AS (
               SELECT override.id
                 FROM "EventVpnGateOverrides" override
                WHERE (override.revoked_at_utc IS NOT NULL
                       OR override.expires_at_utc <= clock_timestamp())
                  AND override.created_at_utc
                      < clock_timestamp() - INTERVAL '30 days'
                  AND NOT EXISTS (
                      SELECT 1 FROM "EventVpnOverrideOperations" operation
                       WHERE operation.override_id = override.id
                  )
                ORDER BY override.created_at_utc, override.id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
           )
           DELETE FROM "EventVpnGateOverrides" override
            USING expired
            WHERE override.id = expired.id"#,
    )
    .bind(VPN_OVERRIDE_MAINTENANCE_LIMIT)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub async fn list_event_vpn_overrides(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<VpnOverrideList>> {
    let policy_revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT vpn_policy_revision FROM "Games"
            WHERE id = $1 AND deletion_pending = FALSE"#,
    )
    .bind(game_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let overrides = sqlx::query_as::<_, VpnOverrideModel>(
        r#"WITH recent_history AS MATERIALIZED (
               SELECT id
                 FROM "EventVpnGateOverrides"
                WHERE game_id = $1
                  AND (revoked_at_utc IS NOT NULL
                       OR expires_at_utc <= clock_timestamp())
                ORDER BY created_at_utc DESC, id DESC
                LIMIT $2
           )
           SELECT id, reason, created_at_utc, expires_at_utc, revoked_at_utc,
                  revoked_at_utc IS NULL
                  AND created_at_utc <= clock_timestamp()
                  AND expires_at_utc > clock_timestamp() AS active
             FROM "EventVpnGateOverrides"
            WHERE game_id = $1
              AND (revoked_at_utc IS NULL AND expires_at_utc > clock_timestamp()
                   OR id IN (SELECT id FROM recent_history))
            ORDER BY active DESC, created_at_utc DESC, id DESC"#,
    )
    .bind(game_id)
    .bind(VPN_OVERRIDE_HISTORY_LIMIT)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RequestResponse::ok(VpnOverrideList {
        policy_revision,
        active_limit: MAX_ACTIVE_VPN_OVERRIDES as i32,
        overrides,
    }))
}

pub async fn create_event_vpn_override(
    State(st): State<SharedState>,
    admin: AdminUser,
    Path(game_id): Path<i32>,
    Json(request): Json<VpnOverrideRequest>,
) -> AppResult<RequestResponse<VpnOverrideResult>> {
    let reason = request.reason.trim();
    if request.operation_id.is_nil()
        || request.expected_policy_revision < 1
        || !(8..=512).contains(&reason.len())
        || !(1..=60).contains(&request.duration_minutes)
    {
        return Err(AppError::bad_request(
            "Override requires an operation ID, observed policy revision, an 8 to 512 byte reason, and 1 to 60 minute duration",
        ));
    }
    let digest = override_request_digest("create", reason, request.duration_minutes, None);
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let current_revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT vpn_policy_revision FROM "Games"
            WHERE id = $1 AND deletion_pending = FALSE
            FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    if let Some(stored) =
        stored_override_operation(&mut transaction, game_id, request.operation_id).await?
    {
        if stored.actor_user_id != admin.0.id
            || stored.action != "create"
            || stored.request_digest != digest
        {
            return Err(AppError::conflict(
                "The operation ID is already bound to a different VPN override intent",
            ));
        }
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(RequestResponse::ok(VpnOverrideResult {
            id: stored.override_id,
            expires_at_utc: stored.expires_at_utc,
            policy_revision: stored.result_revision,
        }));
    }
    if request.expected_policy_revision != current_revision {
        return Err(AppError::conflict(format!(
            "VPN policy changed; current revision is {current_revision}"
        )));
    }
    let active_count = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*)::bigint FROM "EventVpnGateOverrides"
            WHERE game_id = $1 AND revoked_at_utc IS NULL
              AND expires_at_utc > clock_timestamp()"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if active_count >= MAX_ACTIVE_VPN_OVERRIDES {
        return Err(AppError::conflict(format!(
            "This event already has the maximum of {MAX_ACTIVE_VPN_OVERRIDES} active VPN bypasses"
        )));
    }
    let policy_revision = current_revision + 1;
    let id = Uuid::now_v7();
    let expires_at_utc: DateTime<Utc> = sqlx::query_scalar(
        r#"INSERT INTO "EventVpnGateOverrides"
             (id, game_id, created_by_user_id, reason, expires_at_utc, policy_revision)
           VALUES ($1, $2, $3, $4,
                   clock_timestamp() + make_interval(mins => $5), $6)
           RETURNING expires_at_utc"#,
    )
    .bind(id)
    .bind(game_id)
    .bind(admin.0.id)
    .bind(reason)
    .bind(request.duration_minutes)
    .bind(policy_revision)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"UPDATE "Games" SET vpn_policy_revision = $2 WHERE id = $1"#)
        .bind(game_id)
        .bind(policy_revision)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"INSERT INTO "EventVpnOverrideOperations"
             (game_id, operation_id, actor_user_id, action, override_id,
              request_digest, result_revision)
           VALUES ($1, $2, $3, 'create', $4, $5, $6)"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .bind(admin.0.id)
    .bind(id)
    .bind(&digest)
    .bind(policy_revision)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    maintain_override_history(&mut transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::services::event_security::invalidate_policy(&st, game_id).await;
    Ok(RequestResponse::ok(VpnOverrideResult {
        id,
        expires_at_utc,
        policy_revision,
    }))
}

pub async fn revoke_event_vpn_override(
    State(st): State<SharedState>,
    admin: AdminUser,
    Path((game_id, override_id)): Path<(i32, Uuid)>,
    Json(request): Json<VpnOverrideRevokeRequest>,
) -> AppResult<RequestResponse<VpnOverrideResult>> {
    if request.operation_id.is_nil() || request.expected_policy_revision < 1 {
        return Err(AppError::bad_request(
            "Revoke requires an operation ID and observed policy revision",
        ));
    }
    let digest = override_request_digest("revoke", "", 0, Some(override_id));
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let current_revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT vpn_policy_revision FROM "Games"
            WHERE id = $1 AND deletion_pending = FALSE FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    if let Some(stored) =
        stored_override_operation(&mut transaction, game_id, request.operation_id).await?
    {
        if stored.actor_user_id != admin.0.id
            || stored.action != "revoke"
            || stored.override_id != override_id
            || stored.request_digest != digest
        {
            return Err(AppError::conflict(
                "The operation ID is already bound to a different VPN override intent",
            ));
        }
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(RequestResponse::ok(VpnOverrideResult {
            id: stored.override_id,
            expires_at_utc: stored.expires_at_utc,
            policy_revision: stored.result_revision,
        }));
    }
    if request.expected_policy_revision != current_revision {
        return Err(AppError::conflict(format!(
            "VPN policy changed; current revision is {current_revision}"
        )));
    }
    let row = sqlx::query_as::<_, (DateTime<Utc>, Option<DateTime<Utc>>, bool)>(
        r#"SELECT expires_at_utc, revoked_at_utc,
                  revoked_at_utc IS NULL AND expires_at_utc > clock_timestamp()
             FROM "EventVpnGateOverrides"
            WHERE id = $1 AND game_id = $2 FOR UPDATE"#,
    )
    .bind(override_id)
    .bind(game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("VPN override not found"))?;
    let changes_authorization = row.2;
    let result_revision = if changes_authorization {
        current_revision + 1
    } else {
        current_revision
    };
    if row.1.is_none() {
        sqlx::query(
            r#"UPDATE "EventVpnGateOverrides"
                  SET revoked_at_utc = clock_timestamp(), revoked_by_user_id = $3,
                      revoke_policy_revision = $4
                WHERE id = $1 AND game_id = $2 AND revoked_at_utc IS NULL"#,
        )
        .bind(override_id)
        .bind(game_id)
        .bind(admin.0.id)
        .bind(result_revision)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if changes_authorization {
        sqlx::query(r#"UPDATE "Games" SET vpn_policy_revision = $2 WHERE id = $1"#)
            .bind(game_id)
            .bind(result_revision)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    sqlx::query(
        r#"INSERT INTO "EventVpnOverrideOperations"
             (game_id, operation_id, actor_user_id, action, override_id,
              request_digest, result_revision)
           VALUES ($1, $2, $3, 'revoke', $4, $5, $6)"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .bind(admin.0.id)
    .bind(override_id)
    .bind(&digest)
    .bind(result_revision)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    maintain_override_history(&mut transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if changes_authorization {
        crate::services::event_security::invalidate_policy(&st, game_id).await;
    }
    Ok(RequestResponse::ok(VpnOverrideResult {
        id: override_id,
        expires_at_utc: row.0,
        policy_revision: result_revision,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_is_bounded_before_reaching_postgres() {
        assert_eq!(bounded_page(0, 0), (1, 0));
        assert_eq!(bounded_page(u64::MAX, u64::MAX), (500, 1_000_000));
    }

    #[test]
    fn vpn_override_operation_digest_binds_every_semantic_input() {
        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let create = override_request_digest("create", "incident response", 15, None);
        assert_eq!(
            create,
            override_request_digest("create", "incident response", 15, None)
        );
        assert_ne!(
            create,
            override_request_digest("create", "incident response", 16, None)
        );
        assert_ne!(
            create,
            override_request_digest("create", "different reason", 15, None)
        );
        assert_ne!(create, override_request_digest("revoke", "", 0, Some(id)));
    }

    #[test]
    fn retained_block_wire_shape_uses_millis_and_redacts_identity_values() {
        let occurred_at_utc = "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let adjudicated_at_utc = "2026-01-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let exemption_expires_at_utc = "2026-01-09T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let adjudicator = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
        let model = AntiCheatBlockModel::from(AntiCheatBlockRow {
            id: 7,
            user_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            user_name: Some("blocked".to_string()),
            conflict_user_id: Some(
                Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
            ),
            conflict_user_name: Some("conflict".to_string()),
            kind: "Ip".to_string(),
            conflicting_value: Some("198.51.100.42".to_string()),
            occurred_at_utc,
            adjudicated_at_utc: Some(adjudicated_at_utc),
            adjudicated_by_user_id: Some(adjudicator),
            exemption_expires_at_utc: Some(exemption_expires_at_utc),
        });
        let value = serde_json::to_value(model).unwrap();
        assert_eq!(value["conflictingValue"], "198.51.100.x");
        assert_eq!(value["occurredAtUtc"], occurred_at_utc.timestamp_millis());
        assert_eq!(
            value["adjudicatedAtUtc"],
            adjudicated_at_utc.timestamp_millis()
        );
        assert_eq!(value["adjudicatedByUserId"], adjudicator.to_string());
        assert_eq!(
            value["exemptionExpiresAtUtc"],
            exemption_expires_at_utc.timestamp_millis()
        );
    }
}
