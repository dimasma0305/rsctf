//! edit: divisions (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

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
    pub challenge_configs: Vec<DivisionChallengeConfigModel>,
}

/// RSCTF `DivisionChallengeConfig` (Api.ts) — a per-challenge permission override.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DivisionChallengeConfigModel {
    pub challenge_id: i32,
    pub permissions: i32,
}

impl DivisionDetailModel {
    /// Build the wire DTO for a division, loading its persisted challenge configs.
    async fn from_model(st: &SharedState, d: division::Model) -> AppResult<Self> {
        let challenge_configs = division_challenge_config::Entity::find()
            .filter(division_challenge_config::Column::DivisionId.eq(d.id))
            .order_by_asc(division_challenge_config::Column::ChallengeId)
            .all(&st.db)
            .await?
            .into_iter()
            .map(|c| DivisionChallengeConfigModel {
                challenge_id: c.challenge_id,
                permissions: c.permissions,
            })
            .collect();
        Ok(Self {
            id: d.id,
            name: d.name,
            invite_code: d.invite_code,
            default_permissions: d.default_permissions,
            challenge_configs,
        })
    }
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

    // Upsert each provided (challenge, permissions) row.
    for c in configs {
        let permissions = c.permissions.unwrap_or(GamePermission::ALL);
        sqlx::query(
            r#"INSERT INTO "DivisionChallengeConfigs"
                 (division_id, challenge_id, permissions)
               VALUES ($1, $2, $3)
               ON CONFLICT (division_id, challenge_id) DO UPDATE
                 SET permissions = EXCLUDED.permissions"#,
        )
        .bind(division_id)
        .bind(c.challenge_id)
        .bind(permissions)
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
            "Division permissions are locked after A&D/KotH epoch scoring has started.",
        ));
    }
    Ok(())
}

/// Lock and validate the scoring-affecting half of a division update while the
/// caller owns the per-game engine fence. Round preparation takes the same
/// distributed lock before publishing either official scoring boundary, so an
/// update linearizes wholly before that boundary or observes it and is rejected.
async fn guard_division_policy_update(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    division_id: i32,
    requested_default_permissions: Option<i32>,
    requested_challenge_configs: Option<&[DivisionChallengeConfigInput]>,
) -> AppResult<()> {
    let current_default_permissions: Option<i32> = sqlx::query_scalar(
        r#"SELECT default_permissions FROM "Divisions"
            WHERE id = $1 AND game_id = $2
            FOR UPDATE"#,
    )
    .bind(division_id)
    .bind(game_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let current_default_permissions =
        current_default_permissions.ok_or_else(|| AppError::not_found("Division not found"))?;
    let scoring_started = ad_epoch_scoring_started_locked(connection, game_id).await?;

    let current_challenge_configs = if scoring_started && requested_challenge_configs.is_some() {
        sqlx::query_as::<_, (i32, i32)>(
            r#"SELECT challenge_id, permissions
                 FROM "DivisionChallengeConfigs"
                WHERE division_id = $1
                ORDER BY challenge_id"#,
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
        &current_challenge_configs,
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
    let challenge_ids: Vec<i32> =
        sqlx::query_scalar(r#"SELECT id FROM "GameChallenges" WHERE game_id = $1"#)
            .bind(game_id)
            .fetch_all(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    for challenge_id in challenge_ids {
        st.cache
            .remove(&format!(
                "effperm:v3:{game_id}:{division_id}:{challenge_id}"
            ))
            .await;
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
    let divisions = division::Entity::find()
        .filter(division::Column::GameId.eq(id))
        .order_by_asc(division::Column::Id)
        .all(&st.db)
        .await?;
    let mut dtos = Vec::with_capacity(divisions.len());
    for d in divisions {
        dtos.push(DivisionDetailModel::from_model(&st, d).await?);
    }
    Ok(RequestResponse::ok(dtos))
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
    validate_challenge_configs(&st, id, model.challenge_configs.as_deref()).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
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
    let created = division::Entity::find_by_id(created_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::internal("Created division disappeared"))?;
    invalidate_division_caches(&st, id, created_id).await?;
    Ok(RequestResponse::ok(
        DivisionDetailModel::from_model(&st, created).await?,
    ))
}

/// `PUT /api/edit/games/{id}/divisions/{divisionId}`
pub async fn update_division(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, division_id)): Path<(i32, i32)>,
    Json(model): Json<DivisionEditModel>,
) -> AppResult<RequestResponse<DivisionDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    validate_challenge_configs(&st, id, model.challenge_configs.as_deref()).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    guard_division_policy_update(
        control.transaction_mut(),
        id,
        division_id,
        model.default_permissions,
        model.challenge_configs.as_deref(),
    )
    .await?;
    // This exclusive parent lock is the authorization linearization point.
    // In-flight submissions hold FOR SHARE on the same row until commit.
    let updated_id: Option<i32> = sqlx::query_scalar(
        r#"UPDATE "Divisions" SET
               name = COALESCE($3, name),
               invite_code = COALESCE($4, invite_code),
               default_permissions = COALESCE($5, default_permissions)
             WHERE id = $1 AND game_id = $2
         RETURNING id"#,
    )
    .bind(division_id)
    .bind(id)
    .bind(&model.name)
    .bind(&model.invite_code)
    .bind(model.default_permissions)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let updated_id = updated_id.ok_or_else(|| AppError::not_found("Division not found"))?;
    apply_challenge_configs(
        control.transaction_mut(),
        updated_id,
        model.challenge_configs,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let updated = division::Entity::find_by_id(updated_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::internal("Updated division disappeared"))?;
    invalidate_division_caches(&st, id, updated_id).await?;
    Ok(RequestResponse::ok(
        DivisionDetailModel::from_model(&st, updated).await?,
    ))
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
