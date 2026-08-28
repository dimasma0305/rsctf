//! edit: divisions (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

const MAX_DIVISIONS: i64 = 256;
const MAX_DIVISION_CONFIGS: usize = 512;
const MAX_DIVISION_NAME_BYTES: usize = 128;
const MAX_DIVISION_INVITE_BYTES: usize = 256;

/// RSCTF `Division` (Api.ts) — camelCase wire shape. The raw `division::Model`
/// is snake_case and leaks the `gameId` column (`[JsonIgnore]` in RSCTF), so
/// every division handler maps through this DTO instead.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DivisionDetailModel {
    pub id: i32,
    pub name: String,
    pub invite_code: Option<String>,
    /// `GamePermission` bit-flags (numeric, matching Api.ts `GamePermission`).
    pub default_permissions: i32,
    pub revision: i64,
    pub policy_revision: i64,
    pub challenge_configs: Vec<DivisionChallengeConfigModel>,
}

/// RSCTF `DivisionChallengeConfig` (Api.ts) — a per-challenge permission override.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DivisionChallengeConfigModel {
    pub challenge_id: i32,
    pub permissions: i32,
}

#[derive(sqlx::FromRow)]
struct DivisionDetailRow {
    id: i32,
    name: String,
    invite_code: Option<String>,
    default_permissions: i32,
    revision: i64,
    policy_revision: i64,
    config_count: i64,
    challenge_configs: Value,
}

impl TryFrom<DivisionDetailRow> for DivisionDetailModel {
    type Error = AppError;

    fn try_from(row: DivisionDetailRow) -> AppResult<Self> {
        if row.config_count as usize > MAX_DIVISION_CONFIGS {
            return Err(AppError::payload_too_large(format!(
                "A division exposes more than {MAX_DIVISION_CONFIGS} challenge overrides"
            )));
        }
        Ok(Self {
            id: row.id,
            name: row.name,
            invite_code: row.invite_code,
            default_permissions: row.default_permissions,
            revision: row.revision,
            policy_revision: row.policy_revision,
            challenge_configs: serde_json::from_value(row.challenge_configs)
                .map_err(|error| AppError::internal(error.to_string()))?,
        })
    }
}

const DIVISION_DETAIL_SQL: &str = r#"
    SELECT division.id, division.name, division.invite_code,
           division.default_permissions, division.revision,
           division.policy_revision,
           COUNT(config.challenge_id)::bigint AS config_count,
           COALESCE(
               jsonb_agg(jsonb_build_object(
                   'challengeId', config.challenge_id,
                   'permissions', config.permissions
               ) ORDER BY config.challenge_id)
                   FILTER (WHERE config.challenge_id IS NOT NULL),
               '[]'::jsonb
           ) AS challenge_configs
      FROM "Divisions" division
      LEFT JOIN LATERAL (
          SELECT challenge_id, permissions
            FROM "DivisionChallengeConfigs"
           WHERE division_id = division.id
           ORDER BY challenge_id
           LIMIT 513
      ) config ON TRUE
     WHERE division.game_id = $1 AND ($2::integer IS NULL OR division.id = $2)
     GROUP BY division.id
     ORDER BY division.id
     LIMIT $3
"#;

async fn load_division_details(
    pool: &sqlx::PgPool,
    game_id: i32,
    division_id: Option<i32>,
) -> AppResult<Vec<DivisionDetailModel>> {
    let rows = sqlx::query_as::<_, DivisionDetailRow>(DIVISION_DETAIL_SQL)
        .bind(game_id)
        .bind(division_id)
        .bind(if division_id.is_some() {
            1
        } else {
            MAX_DIVISIONS + 1
        })
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if division_id.is_none() && rows.len() as i64 > MAX_DIVISIONS {
        return Err(AppError::payload_too_large(format!(
            "An event may expose at most {MAX_DIVISIONS} divisions"
        )));
    }
    rows.into_iter().map(TryInto::try_into).collect()
}

fn valid_permission_mask(value: i32) -> bool {
    const KNOWN: i32 = GamePermission::JOIN_GAME
        | GamePermission::RANK_OVERALL
        | GamePermission::REQUIRE_REVIEW
        | GamePermission::VIEW_CHALLENGE
        | GamePermission::SUBMIT_FLAGS
        | GamePermission::GET_SCORE
        | GamePermission::GET_BLOOD
        | GamePermission::AFFECT_DYNAMIC_SCORE;
    value == GamePermission::ALL || (value >= 0 && value & !KNOWN == 0)
}

fn validate_division_input(
    name: Option<&str>,
    invite_code: Option<&str>,
    default_permissions: Option<i32>,
    configs: Option<&[DivisionChallengeConfigInput]>,
) -> AppResult<()> {
    if name.is_some_and(|value| value.trim().is_empty() || value.len() > MAX_DIVISION_NAME_BYTES)
        || invite_code.is_some_and(|value| value.len() > MAX_DIVISION_INVITE_BYTES)
        || default_permissions.is_some_and(|value| !valid_permission_mask(value))
    {
        return Err(AppError::bad_request(
            "Invalid division field or permission mask",
        ));
    }
    let Some(configs) = configs else {
        return Ok(());
    };
    if configs.len() > MAX_DIVISION_CONFIGS {
        return Err(AppError::payload_too_large(format!(
            "A division may override at most {MAX_DIVISION_CONFIGS} challenges"
        )));
    }
    let mut ids = std::collections::HashSet::with_capacity(configs.len());
    for config in configs {
        if !ids.insert(config.challenge_id)
            || !valid_permission_mask(config.permissions.unwrap_or(GamePermission::ALL))
        {
            return Err(AppError::bad_request(
                "Division challenge overrides must be unique and use valid permission masks",
            ));
        }
    }
    Ok(())
}

/// Apply a division's inbound `challengeConfigs`, mirroring RSCTF
/// `Division.UpdateChallengeConfigs`:
/// - `None` (field absent) → touch nothing;
/// - `Some([])` → remove every per-challenge config for the division;
/// - `Some([...])` → delete the rows for challenges NOT in the set, then upsert
///   each provided `(challengeId, permissions)` (permissions default `All`).
async fn validate_challenge_configs(
    st: &SharedState,
    game_id: i32,
    configs: Option<&[DivisionChallengeConfigInput]>,
) -> AppResult<()> {
    let Some(configs) = configs else {
        return Ok(());
    };
    let keep_ids: Vec<i32> = configs.iter().map(|c| c.challenge_id).collect();
    let unique_ids: std::collections::HashSet<i32> = keep_ids.iter().copied().collect();
    if !unique_ids.is_empty() {
        let ids: Vec<i32> = unique_ids.iter().copied().collect();
        let valid: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint FROM "GameChallenges"
                WHERE game_id = $1 AND id = ANY($2)"#,
        )
        .bind(game_id)
        .bind(&ids)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if valid != ids.len() as i64 {
            return Err(AppError::bad_request(
                "Division challenge configuration references another game.",
            ));
        }
    }
    Ok(())
}

async fn apply_challenge_configs(
    connection: &mut sqlx::PgConnection,
    division_id: i32,
    configs: Option<Vec<DivisionChallengeConfigInput>>,
) -> AppResult<()> {
    let Some(configs) = configs else {
        return Ok(());
    };
    let keep_ids: Vec<i32> = configs.iter().map(|c| c.challenge_id).collect();

    // Keep the delete + complete replacement in the transaction that owns the
    // parent Division row lock. Submitters take a shared lock on that same row,
    // so they can never authorize against a half-applied permission set.
    if keep_ids.is_empty() {
        sqlx::query(r#"DELETE FROM "DivisionChallengeConfigs" WHERE division_id = $1"#)
            .bind(division_id)
            .execute(&mut *connection)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    } else {
        sqlx::query(
            r#"DELETE FROM "DivisionChallengeConfigs"
                WHERE division_id = $1 AND NOT (challenge_id = ANY($2))"#,
        )
        .bind(division_id)
        .bind(&keep_ids)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    if !configs.is_empty() {
        let challenge_ids = configs
            .iter()
            .map(|config| config.challenge_id)
            .collect::<Vec<_>>();
        let permissions = configs
            .iter()
            .map(|config| config.permissions.unwrap_or(GamePermission::ALL))
            .collect::<Vec<_>>();
        sqlx::query(
            r#"INSERT INTO "DivisionChallengeConfigs"
                 (division_id, challenge_id, permissions)
               SELECT $1, desired.challenge_id, desired.permissions
                 FROM UNNEST($2::integer[], $3::integer[])
                      AS desired(challenge_id, permissions)
               ON CONFLICT (division_id, challenge_id) DO UPDATE
                 SET permissions = EXCLUDED.permissions"#,
        )
        .bind(division_id)
        .bind(&challenge_ids)
        .bind(&permissions)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

fn normalized_challenge_configs(
    configs: &[DivisionChallengeConfigInput],
) -> std::collections::BTreeMap<i32, i32> {
    configs
        .iter()
        .map(|config| {
            (
                config.challenge_id,
                config.permissions.unwrap_or(GamePermission::ALL),
            )
        })
        .collect()
}

fn ensure_scored_division_policy_unchanged(
    scoring_started: bool,
    current_default_permissions: i32,
    current_challenge_configs: &std::collections::BTreeMap<i32, i32>,
    requested_default_permissions: Option<i32>,
    requested_challenge_configs: Option<&[DivisionChallengeConfigInput]>,
) -> AppResult<()> {
    if !scoring_started {
        return Ok(());
    }
    let default_changed = requested_default_permissions
        .is_some_and(|permissions| permissions != current_default_permissions);
    let configs_changed = requested_challenge_configs
        .is_some_and(|configs| normalized_challenge_configs(configs) != *current_challenge_configs);
    if default_changed || configs_changed {
        return Err(AppError::bad_request(
            "Division scoring permissions are locked after competition scoring has started.",
        ));
    }
    Ok(())
}

#[cfg(test)]
async fn guard_division_policy_update(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    division_id: i32,
    requested_default_permissions: Option<i32>,
    requested_challenge_configs: Option<&[DivisionChallengeConfigInput]>,
) -> AppResult<()> {
    let scoring_started = competition_scoring_started_locked(connection, game_id).await?;
    let current_default_permissions: i32 = sqlx::query_scalar(
        r#"SELECT default_permissions FROM "Divisions"
            WHERE id = $1 AND game_id = $2 FOR UPDATE"#,
    )
    .bind(division_id)
    .bind(game_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Division not found"))?;
    let current_configs = if requested_challenge_configs.is_some() {
        sqlx::query_as::<_, (i32, i32)>(
            r#"SELECT challenge_id, permissions FROM "DivisionChallengeConfigs"
                WHERE division_id = $1 ORDER BY challenge_id"#,
        )
        .bind(division_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .into_iter()
        .collect()
    } else {
        std::collections::BTreeMap::new()
    };
    ensure_scored_division_policy_unchanged(
        scoring_started,
        current_default_permissions,
        &current_configs,
        requested_default_permissions,
        requested_challenge_configs,
    )
}

/// Permission edits affect every projection of a game's standings. Evict both
/// permission caches and all role-stable board snapshots immediately.
async fn invalidate_division_caches(
    st: &SharedState,
    game_id: i32,
    division_id: i32,
) -> AppResult<()> {
    for key in [
        format!("div_default:v3:{game_id}:{division_id}"),
        format!("div_overrides:v3:{game_id}:{division_id}"),
    ] {
        st.cache.remove(&key).await;
    }
    flush_game_scoreboards(st, game_id).await;
    Ok(())
}

/// `GET /api/edit/games/{id}/divisions`
pub async fn get_divisions(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<DivisionDetailModel>>> {
    manager_or_admin(&st, &user, id).await?;
    load_game(&st, id).await?;
    Ok(RequestResponse::ok(
        load_division_details(st.pg(), id, None).await?,
    ))
}

/// `POST /api/edit/games/{id}/divisions`
pub async fn create_division(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<DivisionCreateModel>,
) -> AppResult<RequestResponse<DivisionDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    load_game(&st, id).await?;
    validate_division_input(
        Some(&model.name),
        model.invite_code.as_deref(),
        model.default_permissions,
        model.challenge_configs.as_deref(),
    )?;
    validate_challenge_configs(&st, id, model.challenge_configs.as_deref()).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    if competition_scoring_started_locked(control.transaction_mut(), id).await? {
        return Err(AppError::bad_request(
            "Divisions cannot be added after competition scoring has started.",
        ));
    }
    let created_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Divisions" (game_id, name, invite_code, default_permissions)
           VALUES ($1, $2, $3, $4) RETURNING id"#,
    )
    .bind(id)
    .bind(&model.name)
    .bind(&model.invite_code)
    .bind(model.default_permissions.unwrap_or(GamePermission::ALL))
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    apply_challenge_configs(
        control.transaction_mut(),
        created_id,
        model.challenge_configs,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    invalidate_division_caches(&st, id, created_id).await?;
    let created = load_division_details(st.pg(), id, Some(created_id))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::internal("Created division disappeared"))?;
    Ok(RequestResponse::ok(created))
}

/// `PUT /api/edit/games/{id}/divisions/{divisionId}`
pub async fn update_division(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, division_id)): Path<(i32, i32)>,
    Json(model): Json<DivisionEditModel>,
) -> AppResult<RequestResponse<DivisionDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    if model.operation_id.is_nil() || model.expected_revision < 1 {
        return Err(AppError::bad_request(
            "Division update requires an operation ID and observed revision",
        ));
    }
    validate_division_input(
        model.name.as_deref(),
        model.invite_code.as_deref(),
        model.default_permissions,
        model.challenge_configs.as_deref(),
    )?;
    validate_challenge_configs(&st, id, model.challenge_configs.as_deref()).await?;
    let request_digest = Sha256::digest(
        serde_json::to_vec(&model).map_err(|error| AppError::internal(error.to_string()))?,
    )
    .to_vec();
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    let current = sqlx::query_as::<_, (String, Option<String>, i32, i64, i64)>(
        r#"SELECT name, invite_code, default_permissions, revision, policy_revision
             FROM "Divisions"
            WHERE id = $1 AND game_id = $2 FOR UPDATE"#,
    )
    .bind(division_id)
    .bind(id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Division not found"))?;
    let stored = sqlx::query_as::<_, (Uuid, Vec<u8>, i64)>(
        r#"SELECT actor_user_id, request_digest, result_revision
             FROM "DivisionUpdateOperations"
            WHERE division_id = $1 AND operation_id = $2"#,
    )
    .bind(division_id)
    .bind(model.operation_id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some((actor, digest, result_revision)) = stored {
        if actor != user.id || digest != request_digest {
            return Err(AppError::conflict(
                "The operation ID is already bound to another division update",
            ));
        }
        if result_revision != current.3 {
            return Err(AppError::conflict(format!(
                "A newer division revision {} is authoritative",
                current.3
            )));
        }
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let detail = load_division_details(st.pg(), id, Some(division_id))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::not_found("Division not found"))?;
        return Ok(RequestResponse::ok(detail));
    }
    if model.expected_revision != current.3 {
        return Err(AppError::conflict(format!(
            "Division changed; current revision is {}",
            current.3
        )));
    }
    let current_configs = sqlx::query_as::<_, (i32, i32)>(
        r#"SELECT challenge_id, permissions FROM "DivisionChallengeConfigs"
            WHERE division_id = $1 ORDER BY challenge_id"#,
    )
    .bind(division_id)
    .fetch_all(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .collect::<std::collections::BTreeMap<_, _>>();
    let requested_configs = model
        .challenge_configs
        .as_deref()
        .map(normalized_challenge_configs);
    let name_changed = model
        .name
        .as_deref()
        .is_some_and(|value| value.trim() != current.0.as_str());
    let invite_changed = model
        .invite_code
        .as_deref()
        .is_some_and(|value| Some(value) != current.1.as_deref());
    let metadata_changed = name_changed || invite_changed;
    let configs_changed = requested_configs
        .as_ref()
        .is_some_and(|configs| configs != &current_configs);
    let policy_changed = model
        .default_permissions
        .is_some_and(|value| value != current.2)
        || configs_changed;
    let scoring_started = competition_scoring_started_locked(control.transaction_mut(), id).await?;
    ensure_scored_division_policy_unchanged(
        scoring_started,
        current.2,
        &current_configs,
        model.default_permissions,
        model.challenge_configs.as_deref(),
    )?;
    let result_revision = current.3 + i64::from(metadata_changed || policy_changed);
    let policy_revision = current.4 + i64::from(policy_changed);
    if metadata_changed || policy_changed {
        sqlx::query(
            r#"UPDATE "Divisions" SET
                   name = COALESCE($3, name), invite_code = COALESCE($4, invite_code),
                   default_permissions = COALESCE($5, default_permissions),
                   revision = $6, policy_revision = $7
                 WHERE id = $1 AND game_id = $2"#,
        )
        .bind(division_id)
        .bind(id)
        .bind(model.name.as_deref().map(str::trim))
        .bind(&model.invite_code)
        .bind(model.default_permissions)
        .bind(result_revision)
        .bind(policy_revision)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if configs_changed {
            apply_challenge_configs(
                control.transaction_mut(),
                division_id,
                model.challenge_configs,
            )
            .await?;
        }
    }
    sqlx::query(
        r#"INSERT INTO "DivisionUpdateOperations"
             (division_id, operation_id, actor_user_id, request_digest,
              expected_revision, result_revision)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(division_id)
    .bind(model.operation_id)
    .bind(user.id)
    .bind(&request_digest)
    .bind(model.expected_revision)
    .bind(result_revision)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"WITH expired AS (
               SELECT division_id, operation_id
                 FROM "DivisionUpdateOperations"
                WHERE created_at_utc < clock_timestamp() - INTERVAL '30 days'
                ORDER BY created_at_utc, division_id, operation_id
                LIMIT 128
           )
           DELETE FROM "DivisionUpdateOperations" operation
            USING expired
            WHERE operation.division_id = expired.division_id
              AND operation.operation_id = expired.operation_id"#,
    )
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if policy_changed || name_changed {
        invalidate_division_caches(&st, id, division_id).await?;
    }
    let updated = load_division_details(st.pg(), id, Some(division_id))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::internal("Updated division disappeared"))?;
    Ok(RequestResponse::ok(updated))
}

/// `DELETE /api/edit/games/{id}/divisions/{divisionId}` — void.
pub async fn delete_division(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, division_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    if competition_scoring_started_locked(control.transaction_mut(), id).await? {
        return Err(AppError::bad_request(
            "Divisions cannot be deleted after competition scoring has started.",
        ));
    }
    let existing_id: Option<i32> = sqlx::query_scalar(
        r#"SELECT id FROM "Divisions"
            WHERE id = $1 AND game_id = $2
            FOR UPDATE"#,
    )
    .bind(division_id)
    .bind(id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if existing_id.is_none() {
        return Err(AppError::not_found("Division not found"));
    }
    let participants: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "Participations"
            WHERE game_id = $1 AND division_id = $2"#,
    )
    .bind(id)
    .bind(division_id)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if participants != 0 {
        return Err(AppError::bad_request(
            "Move or remove all participants before deleting this division.",
        ));
    }
    sqlx::query(r#"DELETE FROM "DivisionChallengeConfigs" WHERE division_id = $1"#)
        .bind(division_id)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "Divisions" WHERE id = $1 AND game_id = $2"#)
        .bind(division_id)
        .bind(id)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    invalidate_division_caches(&st, id, division_id).await?;
    Ok(MessageResponse::ok(""))
}

#[cfg(test)]
#[path = "divisions_tests.rs"]
mod tests;

// ============================================================================
//  Attack & Defense live console
//
//  DB-backed operator console — the Rust port of RSCTF `AdAdminController`'s
//  State / AdvanceRound / ScoringPause / ToggleChallenge surface. Everything the
//  DB can answer (round timing, per-(team × challenge) service grid, current
//  flags, last SLA verdict, scoring-pause state, challenge enablement) is
//  computed here; the genuinely-Kubernetes bits (live container spin-up, shell,
//  snapshot tarballs) stay as well-typed valid responses — never a 4xx.
// ============================================================================
