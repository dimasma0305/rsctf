//! Immutable stolen-flag evidence and anti-cheat policy adjudication.

use super::*;

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedFindingResult {
    pub operation_id: Uuid,
    pub generation: i64,
    pub status: String,
    pub inserted: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeriveFindingRequest {
    pub operation_id: Uuid,
}

pub async fn derive_event_security_findings(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    Path(game_id): Path<i32>,
    Json(request): Json<DeriveFindingRequest>,
) -> AppResult<RequestResponse<DerivedFindingResult>> {
    let operation = crate::services::suspicion::request_manual_reconciliation(
        &st,
        game_id,
        admin.id,
        request.operation_id,
    )
    .await?;
    let status = match operation.status {
        0 => "Running",
        1 => "Completed",
        _ => "Failed",
    };
    Ok(RequestResponse::ok(DerivedFindingResult {
        operation_id: operation.operation_id,
        generation: operation.generation,
        status: status.to_string(),
        inserted: operation.inserted.unwrap_or(0) as usize,
    }))
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnOverrideResult {
    pub id: Uuid,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expires_at_utc: DateTime<Utc>,
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

pub async fn list_event_vpn_overrides(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<Vec<VpnOverrideModel>>> {
    let overrides = sqlx::query_as::<_, VpnOverrideModel>(
        r#"SELECT id, reason, created_at_utc, expires_at_utc, revoked_at_utc,
                  revoked_at_utc IS NULL
                  AND created_at_utc <= clock_timestamp()
                  AND expires_at_utc > clock_timestamp() AS active
             FROM "EventVpnGateOverrides"
            WHERE game_id = $1
            ORDER BY created_at_utc DESC, id DESC
            LIMIT 100"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RequestResponse::ok(overrides))
}

pub async fn create_event_vpn_override(
    State(st): State<SharedState>,
    admin: AdminUser,
    Path(game_id): Path<i32>,
    Json(request): Json<VpnOverrideRequest>,
) -> AppResult<RequestResponse<VpnOverrideResult>> {
    let reason = request.reason.trim();
    if !(8..=512).contains(&reason.len()) || !(1..=60).contains(&request.duration_minutes) {
        return Err(AppError::bad_request(
            "Override requires an 8 to 512 character reason and 1 to 60 minute duration",
        ));
    }
    let id = Uuid::now_v7();
    let expires_at_utc: DateTime<Utc> = sqlx::query_scalar(
        r#"INSERT INTO "EventVpnGateOverrides"
             (id, game_id, created_by_user_id, reason, expires_at_utc)
           SELECT $1, game.id, $3, $4,
                  clock_timestamp() + make_interval(mins => $5)
             FROM "Games" game
            WHERE game.id = $2 AND game.deletion_pending = FALSE
           RETURNING expires_at_utc"#,
    )
    .bind(id)
    .bind(game_id)
    .bind(admin.0.id)
    .bind(reason)
    .bind(request.duration_minutes)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    crate::services::event_security::invalidate_policy(&st, game_id).await;
    Ok(RequestResponse::ok(VpnOverrideResult {
        id,
        expires_at_utc,
    }))
}

pub async fn revoke_event_vpn_override(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path((game_id, override_id)): Path<(i32, Uuid)>,
) -> AppResult<MessageResponse> {
    let updated = sqlx::query(
        r#"UPDATE "EventVpnGateOverrides"
              SET revoked_at_utc = clock_timestamp()
            WHERE id = $1 AND game_id = $2 AND revoked_at_utc IS NULL"#,
    )
    .bind(override_id)
    .bind(game_id)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("VPN override not found"));
    }
    crate::services::event_security::invalidate_policy(&st, game_id).await;
    Ok(MessageResponse::ok("VPN override revoked"))
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
