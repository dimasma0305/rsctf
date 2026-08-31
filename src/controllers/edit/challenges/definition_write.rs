//! Atomic ordinary challenge-definition writes.

use sqlx::{Postgres, QueryBuilder};

use super::*;

pub(super) struct DefinitionWriteOptions {
    pub active_topology_flip: bool,
    pub final_enabled: bool,
    pub workload_update: Option<Option<JsonValue>>,
    pub invalidated_build_status: Option<ChallengeBuildStatus>,
    pub leaving_managed_generator: bool,
}

pub(super) async fn update(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    expected_revision: i64,
    base: &game_challenge::Model,
    model: &ChallengeUpdateModel,
    options: DefinitionWriteOptions,
) -> AppResult<game_challenge::Model> {
    if !(1..=9_007_199_254_740_990).contains(&expected_revision) {
        return Err(AppError::bad_request(
            "expectedRevision must be a positive safe integer",
        ));
    }

    let mut updated = base.clone();
    let mut query = QueryBuilder::<Postgres>::new(r#"UPDATE "GameChallenges" SET "#);
    let mut fields = 0usize;
    macro_rules! assign {
        ($column:literal, $value:expr) => {{
            if fields > 0 {
                query.push(", ");
            }
            fields += 1;
            query.push($column).push(" = ").push_bind($value);
        }};
    }

    if let Some(value) = model.title.as_ref() {
        updated.title = value.clone();
        assign!("title", value.clone());
    }
    if let Some(value) = model.content.as_ref() {
        updated.content = value.clone();
        assign!("content", value.clone());
    }
    if let Some(value) = model.flag_template.as_ref() {
        let value = (!value.trim().is_empty()).then(|| value.clone());
        updated.flag_template = value.clone();
        assign!("flag_template", value);
    }
    if let Some(value) = model.category {
        updated.category = value;
        assign!("category", value as i16);
    }
    if let Some(value) = model.hints.as_ref() {
        let value = serde_json::to_value(value).unwrap_or(JsonValue::Null);
        updated.hints = Some(value.clone());
        assign!("hints", value);
    }
    if let Some(value) = model.is_enabled {
        updated.is_enabled = value;
        assign!("is_enabled", value);
    } else if options.active_topology_flip {
        updated.is_enabled = options.final_enabled;
        assign!("is_enabled", options.final_enabled);
    }
    if let Some(value) = model.file_name.as_ref() {
        updated.file_name = Some(value.clone());
        assign!("file_name", value.clone());
    }
    if let Some(value) = model.deadline_utc {
        let value = (value.timestamp() != 0).then_some(value);
        updated.deadline_utc = value;
        assign!("deadline_utc", value);
    }
    if let Some(value) = model.submission_limit {
        updated.submission_limit = value;
        assign!("submission_limit", value);
    }
    if let Some(value) = model.container_image.as_ref() {
        let value = value.trim().to_string();
        updated.container_image = Some(value.clone());
        assign!("container_image", value);
    }
    if let Some(value) = options.invalidated_build_status {
        updated.build_status = value;
        updated.build_image_digest = None;
        updated.last_build_log = None;
        assign!("build_status", value as i16);
        assign!("build_image_digest", Option::<String>::None);
        assign!("last_build_log", Option::<String>::None);
    }
    if let Some(value) = model.memory_limit {
        updated.memory_limit = Some(value);
        assign!("memory_limit", value);
    }
    if let Some(value) = model.cpu_count {
        updated.cpu_count = Some(value);
        assign!("cpu_count", value);
    }
    if let Some(value) = model.storage_limit {
        updated.storage_limit = Some(value);
        assign!("storage_limit", value);
    }
    if let Some(value) = model.expose_port {
        updated.expose_port = Some(value);
        assign!("expose_port", value);
    }
    if let Some(value) = options.workload_update {
        updated.workload_spec = value.clone();
        assign!("workload_spec", value);
    }
    if let Some(value) = model.original_score {
        updated.original_score = value;
        assign!("original_score", value);
    }
    if let Some(value) = model.min_score_rate {
        updated.min_score_rate = value;
        assign!("min_score_rate", value);
    }
    if let Some(value) = model.difficulty {
        updated.difficulty = value;
        assign!("difficulty", value);
    }
    if let Some(value) = model.score_curve {
        updated.score_curve = value;
        assign!("score_curve", value as i16);
    }
    if let Some(value) = model.enable_traffic_capture {
        updated.enable_traffic_capture = value;
        assign!("enable_traffic_capture", value);
    }
    if let Some(value) = model.disable_blood_bonus {
        updated.disable_blood_bonus = value;
        assign!("disable_blood_bonus", value);
    }
    if let Some(value) = model.network_mode {
        updated.network_mode = Some(value);
        assign!("network_mode", value as i16);
    }

    let shared = base.challenge_type == ChallengeType::StaticContainer
        && model
            .enable_shared_container
            .unwrap_or(base.enable_shared_container);
    if shared != base.enable_shared_container {
        updated.enable_shared_container = shared;
        assign!("enable_shared_container", shared);
    }
    if let Some(value) = model.ad_checker_image.as_ref() {
        let value = value.trim().to_string();
        updated.ad_checker_image = Some(value.clone());
        assign!("ad_checker_image", value);
    }
    if let Some(value) = model.ad_allow_egress {
        updated.ad_allow_egress = value;
        assign!("ad_allow_egress", value);
    }
    if let Some(value) = model.ad_allow_self_reset {
        updated.ad_allow_self_reset = value;
        assign!("ad_allow_self_reset", value);
    }
    if let Some(value) = model.ad_ssh_requires_flag {
        updated.ad_ssh_requires_flag = value;
        assign!("ad_ssh_requires_flag", value);
    }
    if let Some(value) = model.ad_self_hosted {
        updated.ad_self_hosted = value;
        assign!("ad_self_hosted", value);
    }
    if let Some(value) = model.ad_scoring_weight {
        updated.ad_scoring_weight = value;
        assign!("ad_scoring_weight", value);
    }
    if let Some(value) = model.variant_mode {
        updated.variant_mode = value;
        assign!("variant_mode", value as i16);
    }
    if options.leaving_managed_generator {
        updated.variant_generator_build_context_subdir = None;
        updated.variant_generator_build_status = ChallengeBuildStatus::None;
        updated.variant_generator_last_build_log = None;
        assign!(
            "variant_generator_build_context_subdir",
            Option::<String>::None
        );
        assign!(
            "variant_generator_build_status",
            ChallengeBuildStatus::None as i16
        );
        assign!("variant_generator_last_build_log", Option::<String>::None);
        if model.variant_generator_image.is_none() {
            updated.variant_generator_image = None;
            assign!("variant_generator_image", Option::<String>::None);
        }
        if model.variant_generator_digest.is_none() {
            updated.variant_generator_digest = None;
            assign!("variant_generator_digest", Option::<String>::None);
        }
    }
    if let Some(value) = model.variant_generator_image.as_ref() {
        let value = value.trim();
        let value = (!value.is_empty()).then(|| value.to_owned());
        updated.variant_generator_image = value.clone();
        assign!("variant_generator_image", value);
    }
    if let Some(value) = model.variant_generator_digest.as_ref() {
        let value = value.trim();
        if !value.is_empty()
            && (!value.starts_with("sha256:")
                || value.len() != 71
                || !value[7..]
                    .chars()
                    .all(|character| character.is_ascii_hexdigit()))
        {
            return Err(AppError::bad_request(
                "Variant generator digest must be sha256:<64 lowercase hex characters>.",
            ));
        }
        let value = (!value.is_empty()).then(|| value.to_ascii_lowercase());
        updated.variant_generator_digest = value.clone();
        assign!("variant_generator_digest", value);
    }
    if let Some(value) = model.solve_receipt_mode {
        updated.solve_receipt_mode = value;
        assign!("solve_receipt_mode", value as i16);
    }
    if let Some(value) = model.receipt_verifier_identity.as_ref() {
        let value = value.trim();
        let value = (!value.is_empty()).then(|| value.to_owned());
        updated.receipt_verifier_identity = value.clone();
        assign!("receipt_verifier_identity", value);
    }

    if fields > 0 {
        query.push(", ");
    }
    query
        .push("revision = revision + 1 WHERE id = ")
        .push_bind(challenge_id)
        .push(" AND game_id = ")
        .push_bind(game_id)
        .push(" AND revision = ")
        .push_bind(expected_revision)
        .push(" AND deletion_pending = FALSE");
    let result = query
        .build()
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if result.rows_affected() != 1 {
        let revision: Option<i64> = sqlx::query_scalar(
            r#"SELECT revision FROM "GameChallenges" WHERE id = $1 AND game_id = $2"#,
        )
        .bind(challenge_id)
        .bind(game_id)
        .fetch_optional(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        return match revision {
            None => Err(AppError::not_found("Challenge not found")),
            Some(_) => Err(AppError::conflict(
                "Challenge revision changed; reload and retry the edit",
            )),
        };
    }
    updated.revision = expected_revision + 1;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    #[test]
    fn definition_write_has_one_revision_cas() {
        let source = include_str!("definition_write.rs");
        assert!(source.contains("revision = revision + 1"));
        assert!(source.contains("AND revision = "));
        assert!(source.contains("deletion_pending = FALSE"));
    }
}
