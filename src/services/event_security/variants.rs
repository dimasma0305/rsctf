use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;
use bollard::container::{Config, CreateContainerOptions, LogsOptions, RemoveContainerOptions};
use bollard::models::{HostConfig, HostConfigLogConfig};
use bollard::Docker;
use futures::StreamExt;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::enums::ChallengeVariantMode;
use crate::utils::error::{AppError, AppResult};

const GENERATOR_MEMORY_BYTES: i64 = 128 * 1024 * 1024;
const GENERATOR_NANO_CPUS: i64 = 500_000_000;
const GENERATOR_PIDS: i64 = 64;
const GENERATOR_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_GENERATOR_OUTPUT: usize = 1024 * 1024;
const GENERATOR_LOG_MAX_SIZE: &str = "1m";
static GENERATOR_SLOTS: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(2));

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratorInput {
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
    revision: i32,
    seed: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeneratorOutput {
    manifest: serde_json::Value,
    #[serde(default)]
    artifact_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChallengeVariantManifest {
    /// The real answer for this participation. It is retained only in the
    /// immutable variant ledger and trusted sensor snapshot, never returned to
    /// a player or written into telemetry.
    pub flag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
}

impl ChallengeVariantManifest {
    fn validate(&self) -> AppResult<()> {
        let content_bytes = self.content.as_ref().map_or(0, String::len);
        let hints = self.hints.as_deref().unwrap_or_default();
        let hint_bytes = hints.iter().map(String::len).sum::<usize>();
        if !(8..=127).contains(&self.flag.len())
            || content_bytes > 64 * 1024
            || hints.len() > 32
            || hints.iter().any(|hint| hint.len() > 4 * 1024)
            || hint_bytes > 64 * 1024
        {
            return Err(AppError::bad_request(
                "Variant manifest exceeds its flag/content/hint bounds",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct VariantTarget {
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
    revision: i32,
    generator_image: String,
    generator_digest: String,
    generator_build_context_subdir: Option<String>,
    generator_build_status: i16,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeVariantRow {
    pub id: Uuid,
    pub game_id: i32,
    pub challenge_id: i32,
    pub participation_id: i32,
    pub revision: i32,
    pub generator_image: String,
    pub generator_digest: String,
    pub manifest: serde_json::Value,
    pub artifact_hash: Vec<u8>,
    pub determinism_hash: Vec<u8>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub frozen_at_utc: Option<chrono::DateTime<chrono::Utc>>,
}

fn variant_seed(secret: &str, target: &VariantTarget) -> AppResult<[u8; 32]> {
    super::validate_credential_key(secret)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| AppError::internal("initialize variant seed generator"))?;
    mac.update(b"rsctf:challenge-variant:v1\0");
    mac.update(&target.game_id.to_be_bytes());
    mac.update(&target.challenge_id.to_be_bytes());
    mac.update(&target.participation_id.to_be_bytes());
    mac.update(&target.revision.to_be_bytes());
    Ok(mac.finalize().into_bytes().into())
}

fn bounded_append(output: &mut Vec<u8>, chunk: &[u8]) -> AppResult<()> {
    if output.len().saturating_add(chunk.len()) > MAX_GENERATOR_OUTPUT {
        return Err(AppError::payload_too_large(
            "Variant generator output exceeds 1 MiB",
        ));
    }
    output.extend_from_slice(chunk);
    Ok(())
}

fn generator_runtime_image(st: &SharedState, target: &VariantTarget) -> AppResult<String> {
    if target.generator_build_context_subdir.is_none()
        && crate::services::challenge_images::is_repository_digest(&target.generator_image)
        && target.generator_image.ends_with(&target.generator_digest)
    {
        return Ok(target.generator_image.clone());
    }
    if target.generator_build_context_subdir.as_deref()
        == Some(crate::services::git_sync::GENERATOR_CONTEXT_SUBDIR)
        && target.generator_build_status
            == crate::utils::enums::ChallengeBuildStatus::Success as i16
        && target.generator_image == target.generator_digest
        && crate::services::challenge_images::is_local_image_id(&target.generator_digest)
    {
        return crate::services::challenge_images::validate_runtime_reference(
            &target.generator_digest,
            crate::services::container::ContainerBackendKind::Docker,
            st.config.runtime_role,
            crate::services::challenge_images::shared_docker_daemon_acknowledged(),
        );
    }
    Err(AppError::bad_request(
        "Variant generator has no valid immutable repository or trusted local-build identity",
    ))
}

async fn run_generator_once(
    st: &SharedState,
    docker: &Docker,
    target: &VariantTarget,
    input: &GeneratorInput,
) -> AppResult<Vec<u8>> {
    let runtime_image = generator_runtime_image(st, target)?;
    let inspected = docker.inspect_image(&runtime_image).await.map_err(|_| {
        AppError::unavailable(
            "Variant generator image is not present on the trusted generator host",
        )
    })?;
    if !crate::services::challenge_images::inspect_matches_immutable_reference(
        &inspected,
        &runtime_image,
    ) {
        return Err(AppError::conflict(
            "Variant generator image no longer matches its immutable identity",
        ));
    }
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(input)
            .map_err(|error| AppError::internal(format!("encode variant input: {error}")))?,
    );
    let id = Uuid::new_v4().simple().to_string();
    let name = format!("rsctf-variant-{id}");
    let config = Config {
        image: Some(runtime_image),
        env: Some(vec![format!("RSCTF_VARIANT_INPUT={encoded}")]),
        network_disabled: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        labels: Some(HashMap::from([
            ("rsctf.managed".to_string(), "variant-generator".to_string()),
            ("rsctf.operation".to_string(), id),
        ])),
        host_config: Some(HostConfig {
            memory: Some(GENERATOR_MEMORY_BYTES),
            memory_swap: Some(GENERATOR_MEMORY_BYTES),
            nano_cpus: Some(GENERATOR_NANO_CPUS),
            pids_limit: Some(GENERATOR_PIDS),
            readonly_rootfs: Some(true),
            cap_drop: Some(vec!["ALL".to_string()]),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            log_config: Some(HostConfigLogConfig {
                typ: Some("json-file".to_string()),
                config: Some(HashMap::from([
                    ("max-size".to_string(), GENERATOR_LOG_MAX_SIZE.to_string()),
                    ("max-file".to_string(), "1".to_string()),
                ])),
            }),
            tmpfs: Some(HashMap::from([(
                "/tmp".to_string(),
                "rw,noexec,nosuid,nodev,size=16m".to_string(),
            )])),
            auto_remove: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };
    let created = docker
        .create_container(
            Some(CreateContainerOptions {
                name,
                platform: None,
            }),
            config,
        )
        .await
        .map_err(|error| AppError::internal(format!("create variant generator: {error}")))?;
    let container_id = created.id;
    let run = async {
        docker
            .start_container::<String>(&container_id, None)
            .await
            .map_err(|error| AppError::internal(format!("start variant generator: {error}")))?;
        let mut wait = docker.wait_container::<String>(&container_id, None);
        let result = wait
            .next()
            .await
            .ok_or_else(|| AppError::internal("variant generator returned no exit status"))?
            .map_err(|error| AppError::internal(format!("wait for variant generator: {error}")))?;
        if result.status_code != 0 {
            return Err(AppError::bad_request(format!(
                "Variant generator exited with status {}",
                result.status_code
            )));
        }
        let mut output = Vec::new();
        let mut logs = docker.logs::<String>(
            &container_id,
            Some(LogsOptions {
                stdout: true,
                stderr: false,
                ..Default::default()
            }),
        );
        while let Some(chunk) = logs.next().await {
            let chunk = chunk
                .map_err(|error| AppError::internal(format!("read variant output: {error}")))?;
            bounded_append(&mut output, chunk.as_ref())?;
        }
        Ok(output)
    };
    let result = match tokio::time::timeout(GENERATOR_TIMEOUT, run).await {
        Ok(result) => result,
        Err(_) => Err(AppError::unavailable("Variant generator timed out")),
    };
    let cleanup = docker
        .remove_container(
            &container_id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    if let Err(error) = cleanup {
        tracing::warn!(%error, %container_id, "could not remove variant generator container");
    }
    result
}

async fn run_generator_deterministically(
    st: &SharedState,
    docker: &Docker,
    target: &VariantTarget,
    input: &GeneratorInput,
) -> AppResult<(Vec<u8>, [u8; 32])> {
    let first = run_generator_once(st, docker, target, input).await?;
    let second = run_generator_once(st, docker, target, input).await?;
    let first_hash: [u8; 32] = Sha256::digest(&first).into();
    let second_hash: [u8; 32] = Sha256::digest(&second).into();
    if first_hash != second_hash {
        return Err(AppError::conflict(format!(
            "Variant generator is nondeterministic for challenge {} participation {}",
            target.challenge_id, target.participation_id
        )));
    }
    Ok((first, first_hash))
}

fn parse_output(bytes: &[u8]) -> AppResult<(serde_json::Value, [u8; 32])> {
    let output: GeneratorOutput = serde_json::from_slice(bytes)
        .map_err(|_| AppError::bad_request("Variant generator must emit one JSON object"))?;
    let manifest: ChallengeVariantManifest = serde_json::from_value(output.manifest)
        .map_err(|_| AppError::bad_request("Variant generator emitted an invalid manifest"))?;
    manifest.validate()?;
    let normalized = serde_json::to_value(&manifest)
        .map_err(|error| AppError::internal(format!("normalize variant manifest: {error}")))?;
    let canonical = serde_json::to_vec(&normalized)
        .map_err(|error| AppError::internal(format!("encode variant manifest: {error}")))?;
    let hash: [u8; 32] = Sha256::digest(&canonical).into();
    if let Some(expected) = output.artifact_sha256 {
        if expected.to_ascii_lowercase() != hex::encode(hash) {
            return Err(AppError::bad_request(
                "Variant generator artifactSha256 does not match its manifest",
            ));
        }
    }
    Ok((normalized, hash))
}

pub fn decode_manifest(value: &serde_json::Value) -> AppResult<ChallengeVariantManifest> {
    let manifest: ChallengeVariantManifest = serde_json::from_value(value.clone())
        .map_err(|_| AppError::internal("stored challenge variant manifest is invalid"))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Exercise a newly auto-built image through the same sandbox, output bounds,
/// deterministic replay, and manifest parser used by real event generation.
pub(crate) async fn validate_built_variant_generator(
    st: &SharedState,
    image: &str,
    digest: &str,
) -> AppResult<()> {
    let _permit = GENERATOR_SLOTS
        .acquire()
        .await
        .map_err(|_| AppError::unavailable("Variant generator is shutting down"))?;
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| AppError::unavailable(format!("Docker is unavailable: {error}")))?;
    let target = VariantTarget {
        game_id: 1,
        challenge_id: 1,
        participation_id: 1,
        revision: 1,
        generator_image: image.to_string(),
        generator_digest: digest.to_string(),
        generator_build_context_subdir: Some(
            crate::services::git_sync::GENERATOR_CONTEXT_SUBDIR.to_string(),
        ),
        generator_build_status: crate::utils::enums::ChallengeBuildStatus::Success as i16,
    };
    let input = GeneratorInput {
        game_id: 1,
        challenge_id: 1,
        participation_id: 1,
        revision: 1,
        seed: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 32]),
    };
    let (output, _) = run_generator_deterministically(st, &docker, &target, &input).await?;
    parse_output(&output).map(|_| ())
}

async fn load_targets(st: &SharedState, game_id: i32) -> AppResult<Vec<VariantTarget>> {
    sqlx::query_as(
        r#"SELECT challenge.game_id, challenge.id AS challenge_id,
                  participation.id AS participation_id,
                  COALESCE(MAX(existing.revision), 0) + 1 AS revision,
                  challenge.variant_generator_image AS generator_image,
                  challenge.variant_generator_digest AS generator_digest,
                  challenge.variant_generator_build_context_subdir AS generator_build_context_subdir,
                  challenge.variant_generator_build_status AS generator_build_status
             FROM "GameChallenges" challenge
             JOIN "Games" game ON game.id = challenge.game_id
             JOIN "Participations" participation
               ON participation.game_id = challenge.game_id
              AND participation.status IN (1, 3)
             LEFT JOIN "ChallengeVariants" existing
               ON existing.game_id = challenge.game_id
              AND existing.challenge_id = challenge.id
              AND existing.participation_id = participation.id
            WHERE challenge.game_id = $1
              AND challenge.variant_mode = $2
              AND challenge.variant_generator_image IS NOT NULL
              AND challenge.variant_generator_digest IS NOT NULL
              AND (challenge.variant_generator_build_context_subdir IS NULL
                   OR challenge.variant_generator_build_status = 1)
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = 0
              AND challenge."Type" NOT IN (4, 5)
              AND game.deletion_pending = FALSE
              AND clock_timestamp() < game.start_time_utc
              AND NOT EXISTS (
                  SELECT 1 FROM "ChallengeVariants" frozen
                   WHERE frozen.game_id = challenge.game_id
                     AND frozen.challenge_id = challenge.id
                     AND frozen.participation_id = participation.id
                     AND frozen.frozen_at_utc IS NOT NULL
              )
            GROUP BY challenge.game_id, challenge.id, participation.id,
                     challenge.variant_generator_image,
                     challenge.variant_generator_digest,
                     challenge.variant_generator_build_context_subdir,
                     challenge.variant_generator_build_status
            ORDER BY challenge.id, participation.id"#,
    )
    .bind(game_id)
    .bind(ChallengeVariantMode::PerParticipation as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub async fn generate_event_variants(st: &SharedState, game_id: i32) -> AppResult<usize> {
    let targets = load_targets(st, game_id).await?;
    if targets.is_empty() {
        return Ok(0);
    }
    super::validate_credential_key(&st.config.event_vpn_credential_key)?;
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| AppError::unavailable(format!("Docker is unavailable: {error}")))?;
    let mut generated = 0;
    for target in targets {
        let _permit = GENERATOR_SLOTS
            .acquire()
            .await
            .map_err(|_| AppError::unavailable("Variant generator is shutting down"))?;
        let seed = variant_seed(&st.config.event_vpn_credential_key, &target)?;
        let input = GeneratorInput {
            game_id: target.game_id,
            challenge_id: target.challenge_id,
            participation_id: target.participation_id,
            revision: target.revision,
            seed: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed),
        };
        let (first, first_hash) =
            run_generator_deterministically(st, &docker, &target, &input).await?;
        let (manifest, artifact_hash) = parse_output(&first)?;
        let inserted = sqlx::query(
            r#"INSERT INTO "ChallengeVariants"
                 (id, game_id, challenge_id, participation_id, revision,
                  generator_image, generator_digest, seed_hash, manifest,
                  artifact_hash, determinism_hash, frozen_at_utc)
               SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                      clock_timestamp()
                WHERE EXISTS (
                    SELECT 1 FROM "Games" game
                     WHERE game.id = $2 AND clock_timestamp() < game.start_time_utc
                )
               ON CONFLICT DO NOTHING"#,
        )
        .bind(Uuid::now_v7())
        .bind(target.game_id)
        .bind(target.challenge_id)
        .bind(target.participation_id)
        .bind(target.revision)
        .bind(&target.generator_image)
        .bind(&target.generator_digest)
        .bind(Sha256::digest(seed).as_slice())
        .bind(sqlx::types::Json(manifest))
        .bind(artifact_hash.as_slice())
        .bind(first_hash.as_slice())
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        generated += usize::try_from(inserted.rows_affected()).unwrap_or(usize::MAX);
    }
    Ok(generated)
}

pub async fn variant_for_participation(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
) -> AppResult<Option<ChallengeVariantRow>> {
    sqlx::query_as(
        r#"SELECT id, game_id, challenge_id, participation_id, revision,
                  generator_image, generator_digest, manifest, artifact_hash,
                  determinism_hash, frozen_at_utc
             FROM "ChallengeVariants"
            WHERE game_id = $1 AND challenge_id = $2 AND participation_id = $3
              AND frozen_at_utc IS NOT NULL"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(participation_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_profile_is_small_offline_and_read_only() {
        assert_eq!(GENERATOR_MEMORY_BYTES, 128 * 1024 * 1024);
        assert_eq!(GENERATOR_NANO_CPUS, 500_000_000);
        assert_eq!(GENERATOR_PIDS, 64);
        assert_eq!(GENERATOR_TIMEOUT, Duration::from_secs(30));
        assert_eq!(MAX_GENERATOR_OUTPUT, 1024 * 1024);
        assert_eq!(GENERATOR_LOG_MAX_SIZE, "1m");
    }

    #[test]
    fn output_hash_must_match_the_manifest() {
        let manifest = serde_json::json!({"flag": "RSCTF{variant-abc}"});
        let canonical = serde_json::to_vec(&manifest).unwrap();
        let hash = hex::encode(Sha256::digest(canonical));
        let output = serde_json::to_vec(&serde_json::json!({
            "manifest": manifest,
            "artifactSha256": hash,
        }))
        .unwrap();
        assert!(parse_output(&output).is_ok());
    }
}
