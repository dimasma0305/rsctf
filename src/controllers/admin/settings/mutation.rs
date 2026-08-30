//! One revision-fenced platform-settings mutation and its durable branding stage.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};

use super::*;

const MAX_SETTING_UPDATES: usize = 64;
const MAX_SETTING_BYTES: usize = 64 * 1024;
const MAX_OPERATION_RETENTION_DELETE: i64 = 128;
const MAX_JS_REVISION: i64 = 9_007_199_254_740_991;

type ConfigUpdates = BTreeMap<String, Option<String>>;

#[derive(sqlx::FromRow)]
struct ExistingOperation {
    actor_user_id: Option<Uuid>,
    request_digest: Vec<u8>,
    expected_revision: i64,
    result_revision: i64,
    branding_hash: Option<String>,
}

/// Reconcile a committed response after the browser lost the original reply.
pub async fn get_settings_operation(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    Path(operation_id): Path<Uuid>,
) -> AppResult<RequestResponse<SettingsMutationResult>> {
    let operation = sqlx::query_as::<_, ExistingOperation>(
        r#"SELECT actor_user_id, request_digest, expected_revision,
                  result_revision, branding_hash
             FROM "PlatformSettingsOperations"
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_optional(st.pg())
    .await
    .map_err(database_error)?
    .filter(|operation| operation.actor_user_id == Some(admin.id))
    .ok_or_else(|| AppError::not_found("Settings operation not found"))?;
    Ok(RequestResponse::ok(SettingsMutationResult {
        operation_id,
        revision: operation.result_revision,
        branding_hash: operation.branding_hash,
    }))
}

/// Stage branding without making it authoritative. The settings operation
/// consumes this exact reference or leaves it available for an exact retry.
pub async fn stage_branding(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    Path(operation_id): Path<Uuid>,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<SettingsBrandingStageResult>> {
    let _upload_reservation =
        crate::utils::upload::reserve_buffered(crate::utils::upload::IMAGE_BODY_BYTES)?;
    let mut upload = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::bad_request(format!("multipart error: {error}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let name = field.file_name().unwrap_or("logo").to_string();
        let bytes = field
            .bytes()
            .await
            .map_err(|error| AppError::bad_request(format!("could not read file: {error}")))?;
        upload = Some((name, bytes.to_vec()));
        break;
    }
    let (name, bytes) = upload.ok_or_else(|| AppError::bad_request("No file provided"))?;
    if bytes.is_empty() {
        return Err(AppError::bad_request("File is empty"));
    }
    if bytes.len() > MAX_LOGO_BYTES {
        return Err(AppError::payload_too_large("Logo exceeds the 3 MiB limit"));
    }
    let branding_hash = crate::utils::codec::sha256_hex(&bytes);
    let request_digest = Sha256::digest(&bytes).to_vec();
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(database_error)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('rsctf:settings-branding:' || $1::uuid::text, 0))",
    )
    .bind(operation_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;

    if let Some((actor_user_id, completed_hash)) =
        sqlx::query_as::<_, (Option<Uuid>, Option<String>)>(
            r#"SELECT actor_user_id, branding_hash
             FROM "PlatformSettingsOperations"
            WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
    {
        if actor_user_id != Some(admin.id)
            || completed_hash.as_deref() != Some(branding_hash.as_str())
        {
            return Err(AppError::conflict(
                "This settings operation already has a different result",
            ));
        }
        transaction.commit().await.map_err(database_error)?;
        return Ok(RequestResponse::ok(SettingsBrandingStageResult {
            operation_id,
            branding_hash,
        }));
    }

    if let Some((actor_user_id, saved_digest, saved_hash)) =
        sqlx::query_as::<_, (Option<Uuid>, Vec<u8>, String)>(
            r#"SELECT actor_user_id, request_digest, blob_hash
                 FROM "PlatformSettingsBrandingStaging"
                WHERE operation_id = $1
                FOR UPDATE"#,
        )
        .bind(operation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
    {
        if actor_user_id != Some(admin.id)
            || saved_digest != request_digest
            || saved_hash != branding_hash
        {
            return Err(AppError::conflict(
                "This settings operation already staged different branding",
            ));
        }
    } else {
        let (blob, _) = crate::services::blob_refs::store_and_acquire_in_transaction(
            st.storage.as_ref(),
            &mut transaction,
            &name,
            &bytes,
        )
        .await?;
        sqlx::query(
            r#"INSERT INTO "PlatformSettingsBrandingStaging"
                 (operation_id, actor_user_id, request_digest, blob_hash)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(operation_id)
        .bind(admin.id)
        .bind(&request_digest)
        .bind(&blob.hash)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    }

    transaction.commit().await.map_err(database_error)?;
    if let Err(error) =
        crate::services::settings_branding::purge_expired(st.pg(), st.storage.as_ref(), 16).await
    {
        tracing::warn!(%error, "settings branding retention sweep failed after staging");
    }
    Ok(RequestResponse::ok(SettingsBrandingStageResult {
        operation_id,
        branding_hash,
    }))
}

/// Persist every supplied section in one SQL transaction. The settings-state
/// row is the revision fence and also serializes competing administrator tabs.
pub async fn update_config(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    Json(mut model): Json<ConfigEditModel>,
) -> AppResult<RequestResponse<SettingsMutationResult>> {
    let operation_id = model
        .operation_id
        .ok_or_else(|| AppError::bad_request("operationId is required"))?;
    if operation_id.is_nil() {
        return Err(AppError::bad_request("operationId is required"));
    }
    let expected_revision = model
        .expected_revision
        .filter(|revision| (0..MAX_JS_REVISION).contains(revision))
        .ok_or_else(|| AppError::bad_request("expectedRevision is invalid"))?;
    let branding_action = model.branding_action;
    let mut updates = collect_relational_updates(&mut model)?;
    let has_security_update =
        model.account_policy.is_some() || model.captcha.is_some() || model.o_auth.is_some();
    let donations_enabled = model.donations.as_ref().map(|donations| donations.enabled);
    if let Some(donations) = model.donations.take() {
        extend_updates(
            &mut updates,
            crate::services::donations::prepare_config_updates(&donations)?,
        )?;
    }

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(database_error)?;
    let current_revision = lock_settings_revision(&mut transaction).await?;
    crate::services::anti_cheat::lock_policy_update(&mut transaction).await?;
    let security_updates = security_policy::prepare_security_updates(
        &mut transaction,
        st.config.as_ref(),
        model.account_policy.take(),
        model.captcha.take(),
        model.o_auth.take(),
    )
    .await?;
    extend_updates(&mut updates, security_updates)?;

    let existing = load_operation(&mut transaction, operation_id).await?;
    let branding_hash = resolve_branding_hash(
        &mut transaction,
        operation_id,
        admin.id,
        branding_action,
        existing.as_ref(),
    )
    .await?;
    match branding_action {
        BrandingAction::Keep => {}
        BrandingAction::Set => {
            let hash = branding_hash.clone().ok_or_else(|| {
                AppError::conflict("Upload branding for this operation before saving")
            })?;
            updates.insert("GlobalConfig:LogoHash".to_string(), Some(hash.clone()));
            updates.insert("GlobalConfig:FaviconHash".to_string(), Some(hash));
        }
        BrandingAction::Clear => {
            updates.insert("GlobalConfig:LogoHash".to_string(), None);
            updates.insert("GlobalConfig:FaviconHash".to_string(), None);
        }
    }
    validate_aggregate(&updates)?;
    let request_digest = settings_request_digest(expected_revision, branding_action, &updates)?;

    if let Some(existing) = existing {
        if existing.actor_user_id != Some(admin.id)
            || existing.expected_revision != expected_revision
            || existing.request_digest != request_digest
        {
            return Err(AppError::conflict(
                "This settings operation ID was used for a different request",
            ));
        }
        transaction.commit().await.map_err(database_error)?;
        return Ok(RequestResponse::ok(SettingsMutationResult {
            operation_id,
            revision: existing.result_revision,
            branding_hash: existing.branding_hash,
        }));
    }
    if current_revision != expected_revision {
        return Err(AppError::conflict(format!(
            "Settings changed in another tab; current revision is {current_revision}"
        )));
    }
    if updates.is_empty() && branding_action == BrandingAction::Keep {
        return Err(AppError::bad_request("No settings changes were supplied"));
    }

    let old_branding = if branding_action == BrandingAction::Keep {
        BTreeSet::new()
    } else {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('rsctf:branding-logo', 0))")
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        sqlx::query_scalar::<_, String>(
            r#"SELECT value
                 FROM "Configs"
                WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
                  AND value IS NOT NULL
                FOR UPDATE"#,
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?
        .into_iter()
        .collect()
    };

    write_config_updates(&mut transaction, &updates).await?;
    if has_security_update {
        security_policy::validate_effective_policy(&mut transaction, st.config.as_ref()).await?;
    }
    if let Some(enabled) = donations_enabled {
        crate::services::donations::validate_effective_config(&mut transaction, enabled).await?;
    }

    if branding_action == BrandingAction::Set {
        sqlx::query(
            r#"DELETE FROM "PlatformSettingsBrandingStaging"
                WHERE operation_id = $1 AND actor_user_id = $2"#,
        )
        .bind(operation_id)
        .bind(admin.id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    }
    for old_hash in &old_branding {
        if Some(old_hash) != branding_hash.as_ref() {
            crate::services::blob_refs::release_direct_hash_locked(&mut transaction, old_hash)
                .await?;
        }
    }
    if branding_action == BrandingAction::Set
        && old_branding.contains(branding_hash.as_deref().unwrap_or_default())
    {
        // The stage acquired an extra reference to the already-authoritative
        // bytes. Consuming it must return that extra reference.
        crate::services::blob_refs::release_direct_hash_locked(
            &mut transaction,
            branding_hash.as_deref().unwrap_or_default(),
        )
        .await?;
    }

    let result_revision = sqlx::query_scalar::<_, i64>(
        r#"UPDATE "PlatformSettingsState"
              SET revision = revision + 1, updated_at = clock_timestamp()
            WHERE singleton = 1 AND revision = $1
            RETURNING revision"#,
    )
    .bind(expected_revision)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"INSERT INTO "PlatformSettingsOperations"
             (operation_id, actor_user_id, request_digest, expected_revision,
              result_revision, branding_hash)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(operation_id)
    .bind(admin.id)
    .bind(&request_digest)
    .bind(expected_revision)
    .bind(result_revision)
    .bind(&branding_hash)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"WITH expired AS (
               SELECT operation_id FROM "PlatformSettingsOperations"
                WHERE completed_at < clock_timestamp() - interval '30 days'
                ORDER BY completed_at, operation_id
                LIMIT $1
           )
           DELETE FROM "PlatformSettingsOperations" operation
            USING expired
            WHERE operation.operation_id = expired.operation_id"#,
    )
    .bind(MAX_OPERATION_RETENTION_DELETE)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;

    if has_security_update {
        st.captcha_settings.invalidate().await;
    }
    if donations_enabled.is_some() {
        crate::services::donations::invalidate(st.cache.as_ref()).await;
    }
    for old_hash in old_branding {
        if Some(&old_hash) == branding_hash.as_ref() {
            continue;
        }
        if let Err(error) = crate::services::blob_refs::purge_if_unreferenced(
            st.pg(),
            st.storage.as_ref(),
            &old_hash,
        )
        .await
        {
            tracing::warn!(%error, hash = %old_hash, "old settings branding purge failed");
        }
    }
    Ok(RequestResponse::ok(SettingsMutationResult {
        operation_id,
        revision: result_revision,
        branding_hash,
    }))
}

fn collect_relational_updates(model: &mut ConfigEditModel) -> AppResult<ConfigUpdates> {
    let mut updates = BTreeMap::new();
    if let Some(global) = model.global_config.take() {
        insert_text(
            &mut updates,
            "GlobalConfig:Title",
            Some(global.title),
            128,
            false,
        )?;
        insert_text(
            &mut updates,
            "GlobalConfig:Slogan",
            Some(global.slogan),
            256,
            false,
        )?;
        insert_text(
            &mut updates,
            "GlobalConfig:Description",
            global.description,
            2_048,
            true,
        )?;
        insert_text(
            &mut updates,
            "GlobalConfig:FooterInfo",
            global.footer_info,
            2_048,
            true,
        )?;
        let custom_theme = global.custom_theme.unwrap_or_default().trim().to_string();
        if !custom_theme.is_empty()
            && !(custom_theme.len() == 7
                && custom_theme.starts_with('#')
                && custom_theme[1..]
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(AppError::bad_request(
                "Custom theme must be a #RRGGBB color",
            ));
        }
        updates.insert("GlobalConfig:CustomTheme".to_string(), Some(custom_theme));
        updates.insert(
            "GlobalConfig:ApiEncryption".to_string(),
            Some(global.api_encryption.to_string()),
        );
        let _ = (global.logo_hash, global.favicon_hash);
    }
    if let Some(policy) = model.container_policy.take() {
        policy.validate()?;
        updates.extend([
            (
                "ContainerPolicy:AutoDestroyOnLimitReached".to_string(),
                Some(policy.auto_destroy_on_limit_reached.to_string()),
            ),
            (
                "ContainerPolicy:MaxExerciseContainerCountPerUser".to_string(),
                Some(policy.max_exercise_container_count_per_user.to_string()),
            ),
            (
                "ContainerPolicy:DefaultLifetime".to_string(),
                Some(policy.default_lifetime.to_string()),
            ),
            (
                "ContainerPolicy:ExtensionDuration".to_string(),
                Some(policy.extension_duration.to_string()),
            ),
            (
                "ContainerPolicy:RenewalWindow".to_string(),
                Some(policy.renewal_window.to_string()),
            ),
            (
                "ContainerPolicy:BuildImagesOnDemand".to_string(),
                Some(policy.build_images_on_demand.to_string()),
            ),
            (
                "ContainerPolicy:ImageCleanupEnabled".to_string(),
                Some(policy.image_cleanup_enabled.to_string()),
            ),
            (
                "ContainerPolicy:ImageIdleRetentionHours".to_string(),
                Some(policy.image_idle_retention_hours.to_string()),
            ),
            (
                "ContainerPolicy:BuildCacheRetentionHours".to_string(),
                Some(policy.build_cache_retention_hours.to_string()),
            ),
            (
                "ContainerPolicy:MinimumFreeStorageGiB".to_string(),
                Some(policy.minimum_free_storage_gib.to_string()),
            ),
        ]);
    }
    if let Some(email) = model.email.take() {
        insert_text(
            &mut updates,
            "EmailConfig:UserName",
            Some(email.user_name),
            320,
            false,
        )?;
        insert_secret(
            &mut updates,
            "EmailConfig:Password",
            Some(email.password),
            2_048,
        )?;
        insert_text(
            &mut updates,
            "EmailConfig:SenderAddress",
            email.sender_address,
            320,
            false,
        )?;
        insert_text(
            &mut updates,
            "EmailConfig:SenderName",
            email.sender_name,
            256,
            false,
        )?;
        if let Some(smtp) = email.smtp {
            if !(1..=65_535).contains(&smtp.port) {
                return Err(AppError::bad_request(
                    "SMTP port must be between 1 and 65535",
                ));
            }
            insert_text(
                &mut updates,
                "EmailConfig:Smtp:Host",
                Some(smtp.host),
                253,
                false,
            )?;
            updates.insert(
                "EmailConfig:Smtp:Port".to_string(),
                Some(smtp.port.to_string()),
            );
            updates.insert(
                "EmailConfig:Smtp:BypassCertVerify".to_string(),
                Some(smtp.bypass_cert_verify.to_string()),
            );
        }
    }
    if let Some(registry) = model.registry.take() {
        insert_text(
            &mut updates,
            "RegistryConfig:ServerAddress",
            registry.server_address,
            512,
            false,
        )?;
        insert_text(
            &mut updates,
            "RegistryConfig:UserName",
            registry.user_name,
            256,
            false,
        )?;
        insert_secret(
            &mut updates,
            "RegistryConfig:Password",
            registry.password,
            2_048,
        )?;
    }
    if let Some(registry) = model.build_registry.take() {
        updates.insert(
            "BuildRegistryConfig:PushOnBuild".to_string(),
            Some(registry.push_on_build.to_string()),
        );
        insert_text(
            &mut updates,
            "BuildRegistryConfig:Server",
            registry.server,
            512,
            false,
        )?;
        insert_text(
            &mut updates,
            "BuildRegistryConfig:Namespace",
            registry.namespace,
            256,
            false,
        )?;
        insert_text(
            &mut updates,
            "BuildRegistryConfig:Username",
            registry.username,
            256,
            false,
        )?;
        insert_secret(
            &mut updates,
            "BuildRegistryConfig:Password",
            registry.password,
            2_048,
        )?;
    }
    if let Some(provider) = model.container_provider.take() {
        let mode = provider
            .get("portMappingType")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::bad_request("portMappingType is required"))?;
        if !matches!(mode, "Default" | "PlatformProxy") {
            return Err(AppError::bad_request("portMappingType is invalid"));
        }
        updates.insert(
            "ContainerProvider:PortMappingType".to_string(),
            Some(mode.to_string()),
        );
    }
    validate_security_fields(model)?;
    Ok(updates)
}

fn validate_security_fields(model: &mut ConfigEditModel) -> AppResult<()> {
    if let Some(account) = model.account_policy.as_mut() {
        account.email_domain_list = canonical_email_domains(&account.email_domain_list)?;
    }
    if let Some(captcha) = model.captcha.as_mut() {
        if !matches!(
            captcha.provider.as_str(),
            "None" | "HashPow" | "CloudflareTurnstile"
        ) {
            return Err(AppError::bad_request("Captcha provider is invalid"));
        }
        validate_optional(&captcha.site_key, "Captcha site key", 1_024, false)?;
        validate_optional(&captcha.secret_key, "Captcha secret key", 2_048, false)?;
        if captcha
            .hash_pow
            .as_ref()
            .is_some_and(|hash_pow| !(8..=48).contains(&hash_pow.difficulty))
        {
            return Err(AppError::bad_request(
                "Hash proof-of-work difficulty must be between 8 and 48",
            ));
        }
    }
    if let Some(oauth) = model.o_auth.as_ref() {
        for (value, name, maximum) in [
            (&oauth.google_client_id, "Google client ID", 512),
            (&oauth.google_client_secret, "Google client secret", 2_048),
            (&oauth.discord_client_id, "Discord client ID", 512),
            (&oauth.discord_client_secret, "Discord client secret", 2_048),
        ] {
            validate_optional(value, name, maximum, false)?;
        }
    }
    Ok(())
}

fn canonical_email_domains(input: &str) -> AppResult<String> {
    if input.as_bytes().len() > 4_096 {
        return Err(AppError::payload_too_large(
            "Email domain list exceeds 4096 UTF-8 bytes",
        ));
    }
    let mut domains = BTreeSet::new();
    for domain in input.split([',', '\n', '\r']) {
        let domain = domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            continue;
        }
        if domain.len() > 253
            || domain.chars().any(char::is_whitespace)
            || domain.chars().any(char::is_control)
        {
            return Err(AppError::bad_request("Email domain list is invalid"));
        }
        domains.insert(domain);
        if domains.len() > 128 {
            return Err(AppError::payload_too_large(
                "Email domain list has more than 128 entries",
            ));
        }
    }
    Ok(domains.into_iter().collect::<Vec<_>>().join("\n"))
}

fn insert_text(
    updates: &mut ConfigUpdates,
    key: &str,
    value: Option<String>,
    maximum: usize,
    multiline: bool,
) -> AppResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    validate_text(&value, key, maximum, multiline)?;
    updates.insert(key.to_string(), Some(value.trim().to_string()));
    Ok(())
}

fn insert_secret(
    updates: &mut ConfigUpdates,
    key: &str,
    value: Option<String>,
    maximum: usize,
) -> AppResult<()> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    validate_text(&value, key, maximum, false)?;
    updates.insert(key.to_string(), Some(value));
    Ok(())
}

fn validate_optional(
    value: &Option<String>,
    name: &str,
    maximum: usize,
    multiline: bool,
) -> AppResult<()> {
    if let Some(value) = value {
        validate_text(value, name, maximum, multiline)?;
    }
    Ok(())
}

fn validate_text(value: &str, name: &str, maximum: usize, multiline: bool) -> AppResult<()> {
    if value.as_bytes().len() > maximum {
        return Err(AppError::payload_too_large(format!(
            "{name} exceeds {maximum} UTF-8 bytes"
        )));
    }
    if value.chars().any(|character| {
        character.is_control() && !(multiline && matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(AppError::bad_request(format!(
            "{name} contains unsupported control characters"
        )));
    }
    Ok(())
}

fn extend_updates(
    target: &mut ConfigUpdates,
    updates: Vec<(String, Option<String>)>,
) -> AppResult<()> {
    for (key, value) in updates {
        if target.insert(key.clone(), value).is_some() {
            return Err(AppError::internal(format!(
                "duplicate settings update key: {key}"
            )));
        }
    }
    Ok(())
}

fn validate_aggregate(updates: &ConfigUpdates) -> AppResult<()> {
    if updates.len() > MAX_SETTING_UPDATES {
        return Err(AppError::payload_too_large("Too many settings fields"));
    }
    let bytes = updates.iter().fold(0usize, |total, (key, value)| {
        total
            .saturating_add(key.len())
            .saturating_add(value.as_ref().map_or(0, String::len))
    });
    if bytes > MAX_SETTING_BYTES {
        return Err(AppError::payload_too_large(
            "Settings update exceeds 64 KiB",
        ));
    }
    Ok(())
}

fn settings_request_digest(
    expected_revision: i64,
    branding_action: BrandingAction,
    updates: &ConfigUpdates,
) -> AppResult<Vec<u8>> {
    let canonical = serde_json::to_vec(&(expected_revision, branding_action, updates))
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(Sha256::digest(canonical).to_vec())
}

async fn lock_settings_revision(transaction: &mut Transaction<'_, Postgres>) -> AppResult<i64> {
    sqlx::query_scalar(
        r#"SELECT revision FROM "PlatformSettingsState"
            WHERE singleton = 1
            FOR UPDATE"#,
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)
}

async fn load_operation(
    transaction: &mut Transaction<'_, Postgres>,
    operation_id: Uuid,
) -> AppResult<Option<ExistingOperation>> {
    sqlx::query_as(
        r#"SELECT actor_user_id, request_digest, expected_revision,
                  result_revision, branding_hash
             FROM "PlatformSettingsOperations"
            WHERE operation_id = $1
            FOR UPDATE"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)
}

async fn resolve_branding_hash(
    transaction: &mut Transaction<'_, Postgres>,
    operation_id: Uuid,
    actor_user_id: Uuid,
    action: BrandingAction,
    existing: Option<&ExistingOperation>,
) -> AppResult<Option<String>> {
    if action != BrandingAction::Set {
        return Ok(None);
    }
    let staged = sqlx::query_as::<_, (Option<Uuid>, String)>(
        r#"SELECT actor_user_id, blob_hash
             FROM "PlatformSettingsBrandingStaging"
            WHERE operation_id = $1
            FOR UPDATE"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    if let Some((owner, hash)) = staged {
        if owner != Some(actor_user_id) {
            return Err(AppError::conflict(
                "Branding stage belongs to a different administrator",
            ));
        }
        if existing.is_some_and(|operation| operation.branding_hash.as_ref() != Some(&hash)) {
            return Err(AppError::conflict(
                "Branding stage does not match this completed operation",
            ));
        }
        return Ok(Some(hash));
    }
    Ok(existing.and_then(|operation| operation.branding_hash.clone()))
}

async fn write_config_updates(
    transaction: &mut Transaction<'_, Postgres>,
    updates: &ConfigUpdates,
) -> AppResult<()> {
    if updates.is_empty() {
        return Ok(());
    }
    let keys = updates.keys().cloned().collect::<Vec<_>>();
    let values = updates.values().cloned().collect::<Vec<_>>();
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key, value, cache_keys)
           SELECT key, value, NULL::jsonb
             FROM UNNEST($1::text[], $2::text[]) AS incoming(key, value)
           ON CONFLICT (config_key) DO UPDATE SET value = EXCLUDED.value"#,
    )
    .bind(keys)
    .bind(values)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
#[path = "mutation_tests.rs"]
mod tests;
