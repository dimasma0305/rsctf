//! Global / account / container config + platform logo upload.

use super::*;

mod mutation;
mod security_policy;

pub use mutation::{get_settings_operation, stage_branding, update_config};

pub use crate::services::container_policy::ContainerPolicy;
pub use crate::services::donations::{DonationConfig, DonationProvider};

pub(crate) const DEFAULT_CONTAINER_PORT_MAPPING: &str = "PlatformProxy";

pub(crate) fn normalized_container_port_mapping(value: Option<&str>) -> &'static str {
    match value {
        Some("Default") => "Default",
        Some("PlatformProxy") => "PlatformProxy",
        _ => DEFAULT_CONTAINER_PORT_MAPPING,
    }
}

// ─── Config ──────────────────────────────────────────────────────────────────

/// RSCTF `GlobalConfig`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GlobalConfig {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub slogan: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub footer_info: Option<String>,
    #[serde(default)]
    pub custom_theme: Option<String>,
    #[serde(default)]
    pub api_encryption: bool,
    #[serde(default)]
    pub logo_hash: Option<String>,
    #[serde(default)]
    pub favicon_hash: Option<String>,
}

/// RSCTF `AccountPolicy`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AccountPolicy {
    pub allow_register: bool,
    pub allow_password_registration: bool,
    pub active_on_register: bool,
    pub use_captcha: bool,
    pub email_confirmation_required: bool,
    pub email_domain_list: String,
    pub enable_browser_fingerprint: bool,
    pub require_unique_ip_per_team_user: bool,
    pub require_unique_fingerprint_per_team_user: bool,
    pub require_unique_ip_global: bool,
    pub require_unique_fingerprint_global: bool,
}

impl Default for AccountPolicy {
    fn default() -> Self {
        Self {
            allow_register: true,
            allow_password_registration: true,
            active_on_register: true,
            use_captcha: false,
            email_confirmation_required: false,
            email_domain_list: String::new(),
            enable_browser_fingerprint: false,
            require_unique_ip_per_team_user: false,
            require_unique_fingerprint_per_team_user: false,
            require_unique_ip_global: false,
            require_unique_fingerprint_global: false,
        }
    }
}

fn default_smtp_host() -> String {
    "127.0.0.1".to_string()
}
fn default_smtp_port() -> i32 {
    587
}
fn default_difficulty() -> i32 {
    18
}
fn default_provider() -> String {
    "None".to_string()
}

/// RSCTF `SmtpConfig` (the subset the settings UI edits — the C# type also
/// carries `SecureSocketOption`, which the client contract omits).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpConfig {
    #[serde(default = "default_smtp_host")]
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: i32,
    #[serde(default)]
    pub bypass_cert_verify: bool,
}

/// RSCTF `EmailConfig`. `password` empty = leave the stored value unchanged;
/// `has_password` / `is_configured` are read-only surrogates (ignored on write).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailConfig {
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub sender_address: Option<String>,
    #[serde(default)]
    pub sender_name: Option<String>,
    #[serde(default)]
    pub smtp: Option<SmtpConfig>,
    #[serde(default)]
    pub has_password: bool,
    #[serde(default)]
    pub is_configured: bool,
}

/// RSCTF `HashPowConfig`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HashPowConfig {
    #[serde(default = "default_difficulty")]
    pub difficulty: i32,
}

/// RSCTF `CaptchaConfig`. `provider` is the string enum (`None` / `HashPow` /
/// `CloudflareTurnstile`); `secret_key` empty = leave the stored value unchanged.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptchaConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub site_key: Option<String>,
    #[serde(default)]
    pub secret_key: Option<String>,
    #[serde(default)]
    pub hash_pow: Option<HashPowConfig>,
    #[serde(default)]
    pub has_secret_key: bool,
}

/// RSCTF `RegistryConfig` (private-image pull credentials). `password` empty =
/// leave the stored value unchanged.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryConfig {
    #[serde(default)]
    pub server_address: Option<String>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub has_password: bool,
    #[serde(default)]
    pub is_configured: bool,
}

/// RSCTF `OAuthConfig`. Client secrets empty = leave the stored value unchanged.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthConfig {
    #[serde(default)]
    pub google_client_id: Option<String>,
    #[serde(default)]
    pub google_client_secret: Option<String>,
    #[serde(default)]
    pub discord_client_id: Option<String>,
    #[serde(default)]
    pub discord_client_secret: Option<String>,
    #[serde(default)]
    pub has_google_client_secret: bool,
    #[serde(default)]
    pub has_discord_client_secret: bool,
}

/// Read-only view of the environment-managed reverse-proxy trust boundary.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTrustConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub forward_x_forwarded_for: bool,
    #[serde(default)]
    pub forward_x_forwarded_host: bool,
    #[serde(default)]
    pub forward_x_forwarded_proto: bool,
    #[serde(default = "default_forward_limit")]
    pub forward_limit: i32,
    #[serde(default)]
    pub trusted_networks_csv: String,
    #[serde(default)]
    pub trusted_proxies_csv: String,
}

fn default_forward_limit() -> i32 {
    1
}
fn default_true() -> bool {
    true
}

fn runtime_proxy_trust_config() -> ProxyTrustConfig {
    let trusted_networks = crate::services::anti_cheat::configured_trusted_proxy_cidrs();
    ProxyTrustConfig {
        enabled: !trusted_networks.is_empty(),
        forward_x_forwarded_for: !trusted_networks.is_empty(),
        forward_x_forwarded_host: false,
        forward_x_forwarded_proto: false,
        forward_limit: 1,
        trusted_networks_csv: trusted_networks.join("\n"),
        trusted_proxies_csv: String::new(),
    }
}

/// RSCTF `BuildRegistryConfig` (auto-build image push destination). `password`
/// empty = leave the stored value unchanged.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildRegistryConfig {
    #[serde(default)]
    pub push_on_build: bool,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub has_password: bool,
    #[serde(default)]
    pub is_configured: bool,
}

/// RSCTF `ConfigEditModel`. All editable sections are strongly typed so GET
/// round-trips exactly what PUT persisted (secrets excepted — see below). The
/// `container_provider` exposes a read-only backend summary plus the mutable
/// `portMappingType` preference persisted by `get_config` / `update_config`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigEditModel {
    /// Present on GET and required as `expectedRevision` on PUT.
    #[serde(default, skip_deserializing)]
    pub revision: i64,
    /// Stable browser intent used to make an ambiguous retry exact.
    #[serde(default)]
    pub operation_id: Option<Uuid>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    #[serde(default)]
    pub branding_action: BrandingAction,
    #[serde(default)]
    pub account_policy: Option<AccountPolicy>,
    #[serde(default)]
    pub global_config: Option<GlobalConfig>,
    #[serde(default)]
    pub container_policy: Option<ContainerPolicy>,
    #[serde(default)]
    pub build_registry: Option<BuildRegistryConfig>,
    #[serde(default)]
    pub email: Option<EmailConfig>,
    #[serde(default)]
    pub captcha: Option<CaptchaConfig>,
    #[serde(default, rename = "oAuth")]
    pub o_auth: Option<OAuthConfig>,
    #[serde(default)]
    pub registry: Option<RegistryConfig>,
    #[serde(default)]
    pub donations: Option<DonationConfig>,
    /// Runtime-derived from `RSCTF_TRUSTED_PROXY_CIDRS`; incoming values are
    /// ignored so older clients can still round-trip the full settings model.
    #[serde(default, skip_deserializing)]
    pub proxy_trust: Option<ProxyTrustConfig>,
    #[serde(default)]
    pub container_provider: Option<Value>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum BrandingAction {
    #[default]
    Keep,
    Set,
    Clear,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsMutationResult {
    pub operation_id: Uuid,
    pub revision: i64,
    pub branding_hash: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsBrandingStageResult {
    pub operation_id: Uuid,
    pub branding_hash: String,
}

/// `GET /api/admin/config` — assemble the `ConfigEditModel` from the flat
/// `Configs` key/value table (only the globalConfig section is persisted here;
/// the rest fall back to defaults).
pub async fn get_config(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<ConfigEditModel>> {
    let rows = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT config_key, value
             FROM "Configs"
            WHERE config_key LIKE 'GlobalConfig:%'
               OR config_key LIKE 'AccountPolicy:%'
               OR config_key LIKE 'ContainerPolicy:%'
               OR config_key LIKE 'EmailConfig:%'
               OR config_key LIKE 'CaptchaConfig:%'
               OR config_key LIKE 'OAuthConfig:%'
               OR config_key LIKE 'RegistryConfig:%'
               OR config_key LIKE 'BuildRegistryConfig:%'
               OR config_key LIKE 'DonationConfig:%'
               OR config_key = 'ContainerProvider:PortMappingType'
            ORDER BY config_key
            LIMIT 129"#,
    )
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.len() > 128
        || rows.iter().fold(0usize, |total, (key, value)| {
            total
                .saturating_add(key.len())
                .saturating_add(value.as_ref().map_or(0, String::len))
        }) > 128 * 1024
    {
        return Err(AppError::internal(
            "Stored platform settings exceed the bounded admin projection",
        ));
    }
    let map: BTreeMap<String, Option<String>> = rows.into_iter().collect();

    let get = |key: &str| map.get(key).cloned().flatten();
    // Persisted values are lowercase `bool::to_string()` (matching the existing
    // `GlobalConfig:ApiEncryption` convention) — fall back to a supplied default
    // when the key was never written.
    let get_bool = |key: &str, default: bool| get(key).map(|v| v == "true").unwrap_or(default);
    let get_i32 =
        |key: &str, default: i32| get(key).and_then(|v| v.parse().ok()).unwrap_or(default);

    let global = GlobalConfig {
        title: get("GlobalConfig:Title").unwrap_or_default(),
        slogan: get("GlobalConfig:Slogan").unwrap_or_default(),
        description: get("GlobalConfig:Description"),
        footer_info: get("GlobalConfig:FooterInfo"),
        custom_theme: get("GlobalConfig:CustomTheme"),
        api_encryption: get_bool("GlobalConfig:ApiEncryption", false),
        logo_hash: get("GlobalConfig:LogoHash"),
        favicon_hash: get("GlobalConfig:FaviconHash"),
    };

    let defaults = AccountPolicy {
        allow_register: st.config.account.allow_register,
        allow_password_registration: st.config.account.allow_password_registration,
        active_on_register: st.config.account.active_on_register,
        use_captcha: st.config.account.use_captcha,
        email_confirmation_required: st.config.account.email_confirmation_required,
        ..AccountPolicy::default()
    };
    let account = AccountPolicy {
        allow_register: get_bool("AccountPolicy:AllowRegister", defaults.allow_register),
        allow_password_registration: get_bool(
            "AccountPolicy:AllowPasswordRegistration",
            defaults.allow_password_registration,
        ),
        active_on_register: get_bool(
            "AccountPolicy:ActiveOnRegister",
            defaults.active_on_register,
        ),
        use_captcha: get_bool("AccountPolicy:UseCaptcha", defaults.use_captcha),
        email_confirmation_required: get_bool(
            "AccountPolicy:EmailConfirmationRequired",
            defaults.email_confirmation_required,
        ),
        email_domain_list: get("AccountPolicy:EmailDomainList").unwrap_or_default(),
        enable_browser_fingerprint: get_bool(
            "AccountPolicy:EnableBrowserFingerprint",
            defaults.enable_browser_fingerprint,
        ),
        require_unique_ip_per_team_user: get_bool(
            "AccountPolicy:RequireUniqueIpPerTeamUser",
            defaults.require_unique_ip_per_team_user,
        ),
        require_unique_fingerprint_per_team_user: get_bool(
            "AccountPolicy:RequireUniqueFingerprintPerTeamUser",
            defaults.require_unique_fingerprint_per_team_user,
        ),
        require_unique_ip_global: get_bool(
            "AccountPolicy:RequireUniqueIpGlobal",
            defaults.require_unique_ip_global,
        ),
        require_unique_fingerprint_global: get_bool(
            "AccountPolicy:RequireUniqueFingerprintGlobal",
            defaults.require_unique_fingerprint_global,
        ),
    };

    let cd = ContainerPolicy::default();
    let container = ContainerPolicy {
        auto_destroy_on_limit_reached: get_bool(
            "ContainerPolicy:AutoDestroyOnLimitReached",
            cd.auto_destroy_on_limit_reached,
        ),
        max_exercise_container_count_per_user: get_i32(
            "ContainerPolicy:MaxExerciseContainerCountPerUser",
            cd.max_exercise_container_count_per_user,
        ),
        default_lifetime: get_i32("ContainerPolicy:DefaultLifetime", cd.default_lifetime),
        extension_duration: get_i32("ContainerPolicy:ExtensionDuration", cd.extension_duration),
        renewal_window: get_i32("ContainerPolicy:RenewalWindow", cd.renewal_window),
        build_images_on_demand: get_bool(
            "ContainerPolicy:BuildImagesOnDemand",
            cd.build_images_on_demand,
        ),
        image_cleanup_enabled: get_bool(
            "ContainerPolicy:ImageCleanupEnabled",
            cd.image_cleanup_enabled,
        ),
        image_idle_retention_hours: get_i32(
            "ContainerPolicy:ImageIdleRetentionHours",
            cd.image_idle_retention_hours,
        ),
        build_cache_retention_hours: get_i32(
            "ContainerPolicy:BuildCacheRetentionHours",
            cd.build_cache_retention_hours,
        ),
        minimum_free_storage_gib: get_i32(
            "ContainerPolicy:MinimumFreeStorageGiB",
            cd.minimum_free_storage_gib,
        ),
    };

    // --- Advanced sections (round-trip what update_config persisted) ---------
    // Secret values are blanked on read (mirrors RSCTF's safe-copy): the `hasX`
    // surrogate carries presence so the UI shows "(configured)" without ever
    // shipping the stored secret. A stored secret is any non-empty value.
    let has = |key: &str| get(key).map(|v| !v.is_empty()).unwrap_or(false);

    let smtp_host = get("EmailConfig:Smtp:Host").unwrap_or_else(default_smtp_host);
    let smtp_port = get_i32("EmailConfig:Smtp:Port", default_smtp_port());
    let email_sender = get("EmailConfig:SenderAddress");
    let email = EmailConfig {
        user_name: get("EmailConfig:UserName").unwrap_or_default(),
        password: String::new(),
        sender_address: email_sender.clone(),
        sender_name: get("EmailConfig:SenderName"),
        smtp: Some(SmtpConfig {
            host: smtp_host.clone(),
            port: smtp_port,
            bypass_cert_verify: get_bool("EmailConfig:Smtp:BypassCertVerify", false),
        }),
        has_password: has("EmailConfig:Password"),
        is_configured: email_sender.map(|s| !s.is_empty()).unwrap_or(false)
            && !smtp_host.is_empty()
            && smtp_port > 0,
    };

    let captcha = CaptchaConfig {
        provider: get("CaptchaConfig:Provider").unwrap_or_else(default_provider),
        site_key: get("CaptchaConfig:SiteKey"),
        secret_key: None,
        hash_pow: Some(HashPowConfig {
            difficulty: get_i32("CaptchaConfig:HashPow:Difficulty", default_difficulty()),
        }),
        has_secret_key: has("CaptchaConfig:SecretKey"),
    };

    let reg_server = get("RegistryConfig:ServerAddress");
    let registry = RegistryConfig {
        server_address: reg_server.clone(),
        user_name: get("RegistryConfig:UserName"),
        password: None,
        has_password: has("RegistryConfig:Password"),
        is_configured: reg_server.map(|s| !s.is_empty()).unwrap_or(false),
    };

    let effective_oauth = crate::services::oauth_config::OAuthSettings::load(st.pg()).await?;
    let o_auth = OAuthConfig {
        google_client_id: effective_oauth
            .provider("google")
            .map(|provider| provider.client_id.clone()),
        google_client_secret: None,
        discord_client_id: effective_oauth
            .provider("discord")
            .map(|provider| provider.client_id.clone()),
        discord_client_secret: None,
        has_google_client_secret: effective_oauth.google_configured(),
        has_discord_client_secret: effective_oauth.discord_configured(),
    };

    let proxy_trust = runtime_proxy_trust_config();

    let br_push = get_bool("BuildRegistryConfig:PushOnBuild", false);
    let br_server = get("BuildRegistryConfig:Server");
    let build_registry = BuildRegistryConfig {
        push_on_build: br_push,
        server: br_server.clone(),
        namespace: get("BuildRegistryConfig:Namespace"),
        username: get("BuildRegistryConfig:Username"),
        password: None,
        has_password: has("BuildRegistryConfig:Password"),
        is_configured: br_push && br_server.map(|s| !s.is_empty()).unwrap_or(false),
    };
    let donations = crate::services::donations::admin_config(&map);

    // Read-only container-provider summary (mirrors RSCTF `ContainerProviderInfoModel`).
    // rsctf only persists the port-mapping mode; `type`/`trafficCapture` are best-effort
    // (Docker backend, no global traffic-capture toggle — that lives per-challenge).
    let stored_port_mapping = get("ContainerProvider:PortMappingType");
    let port_mapping_type =
        normalized_container_port_mapping(stored_port_mapping.as_deref()).to_string();
    let container_provider = serde_json::json!({
        "type": "Docker",
        "portMappingType": port_mapping_type,
        "trafficCapture": false,
        "kubernetesNamespace": null,
        "imagePullPolicy": null,
    });

    let revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT revision FROM "PlatformSettingsState" WHERE singleton = 1"#,
    )
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(RequestResponse::ok(ConfigEditModel {
        revision,
        operation_id: None,
        expected_revision: None,
        branding_action: BrandingAction::Keep,
        account_policy: Some(account),
        global_config: Some(global),
        container_policy: Some(container),
        build_registry: Some(build_registry),
        email: Some(email),
        captcha: Some(captcha),
        o_auth: Some(o_auth),
        registry: Some(registry),
        donations: Some(donations),
        proxy_trust: Some(proxy_trust),
        container_provider: Some(container_provider),
    }))
}

/// Reads the admin-set container port-mapping mode
/// (`ContainerProvider:PortMappingType`), returning one of the
/// `ContainerPortMappingType` wire strings — `"Default"` (direct host:port) or
/// `"PlatformProxy"` (wsrx-proxied) — and defaulting to `"PlatformProxy"` when
/// the key is absent or invalid. Container creation calls this to decide whether a fresh
/// instance is marked `is_proxy` (so `Container::entry()` hands the client a
/// proxy guid instead of a host:port); `GET /api/info` advertises the same mode.
pub(crate) async fn container_port_mapping(st: &SharedState) -> String {
    let value = config::Entity::find_by_id("ContainerProvider:PortMappingType".to_string())
        .one(&st.db)
        .await
        .ok()
        .flatten()
        .and_then(|c| c.value);
    normalized_container_port_mapping(value.as_deref()).to_string()
}

/// Maximum logo upload size (mirrors RSCTF's 3 MiB cap).
const MAX_LOGO_BYTES: usize = crate::utils::upload::IMAGE_FILE_BYTES;

/// `POST /api/admin/config/logo` (multipart, field `file`) — platform logo
/// upload. Stores the blob in `st.storage`, records a `Files` row, and persists
/// its content hash as the `GlobalConfig:LogoHash` / `GlobalConfig:FaviconHash`
/// config values (the client serves it via `/assets/{hash}/...`). RSCTF resizes
/// to 640/256 px; here the original bytes are used for both.
pub async fn logo_upload(
    State(st): State<SharedState>,
    _admin: AdminUser,
    mut multipart: Multipart,
) -> AppResult<MessageResponse> {
    let _upload_reservation =
        crate::utils::upload::reserve_buffered(crate::utils::upload::IMAGE_BODY_BYTES)?;
    let mut data: Option<(String, Vec<u8>)> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        if field.name() == Some("file") {
            let name = field.file_name().unwrap_or("logo").to_string();
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad_request(format!("could not read file: {e}")))?;
            data = Some((name, bytes.to_vec()));
            break;
        }
    }

    let (name, bytes) = data.ok_or_else(|| AppError::bad_request("No file provided"))?;
    if bytes.is_empty() {
        return Err(AppError::bad_request("File is empty"));
    }
    if bytes.len() > MAX_LOGO_BYTES {
        return Err(AppError::bad_request("File is too large"));
    }

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"SELECT revision FROM "PlatformSettingsState"
            WHERE singleton = 1 FOR UPDATE"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    // The two keys are one branding operation. Serialize them so concurrent
    // replicas cannot leave LogoHash and FaviconHash on different uploads.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('rsctf:branding-logo', 0))")
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let old_hashes: std::collections::BTreeSet<String> = sqlx::query_scalar(
        r#"SELECT value
             FROM "Configs"
            WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
              AND value IS NOT NULL
            FOR UPDATE"#,
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .collect();
    let (blob, _) = crate::services::blob_refs::store_and_acquire_in_transaction(
        st.storage.as_ref(),
        &mut transaction,
        &name,
        &bytes,
    )
    .await?;
    let new_hash = blob.hash.clone();
    for key in ["GlobalConfig:LogoHash", "GlobalConfig:FaviconHash"] {
        sqlx::query(
            r#"INSERT INTO "Configs" (config_key, value, cache_keys)
               VALUES ($1, $2, NULL)
               ON CONFLICT (config_key) DO UPDATE SET value = EXCLUDED.value"#,
        )
        .bind(key)
        .bind(&blob.hash)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    for old_hash in &old_hashes {
        crate::services::blob_refs::release_direct_hash_locked(&mut transaction, old_hash).await?;
    }
    sqlx::query(
        r#"UPDATE "PlatformSettingsState"
              SET revision = revision + 1, updated_at = clock_timestamp()
            WHERE singleton = 1"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    for old_hash in old_hashes {
        if old_hash == new_hash {
            continue;
        }
        if let Err(error) = crate::services::blob_refs::purge_if_unreferenced(
            st.pg(),
            st.storage.as_ref(),
            &old_hash,
        )
        .await
        {
            tracing::warn!(%error, hash = %old_hash, "old branding blob purge failed");
        }
    }

    Ok(MessageResponse::ok(""))
}

/// `DELETE /api/admin/config/logo` — clear the platform logo.
pub async fn logo_delete(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<MessageResponse> {
    let old_hashes = clear_branding_hashes(st.pg()).await?;
    for old_hash in old_hashes {
        if let Err(error) = crate::services::blob_refs::purge_if_unreferenced(
            st.pg(),
            st.storage.as_ref(),
            &old_hash,
        )
        .await
        {
            tracing::warn!(%error, hash = %old_hash, "deleted branding blob purge failed");
        }
    }
    Ok(MessageResponse::ok(""))
}

/// Clear the two config keys owned by one branding upload and return each old
/// logical blob reference exactly once. Upload acquires one reference even
/// though both keys point at it, so deleting a shared hash must likewise
/// release it once rather than once per key.
async fn clear_branding_hashes(
    pool: &sqlx::PgPool,
) -> AppResult<std::collections::BTreeSet<String>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"SELECT revision FROM "PlatformSettingsState"
            WHERE singleton = 1 FOR UPDATE"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('rsctf:branding-logo', 0))")
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let old_hashes = sqlx::query_scalar::<_, String>(
        r#"SELECT value
             FROM "Configs"
            WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
              AND value IS NOT NULL
            FOR UPDATE"#,
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .collect::<std::collections::BTreeSet<_>>();
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key, value, cache_keys) VALUES
               ('GlobalConfig:LogoHash', NULL, NULL),
               ('GlobalConfig:FaviconHash', NULL, NULL)
           ON CONFLICT (config_key) DO UPDATE SET value = NULL"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    for old_hash in &old_hashes {
        crate::services::blob_refs::release_direct_hash_locked(&mut transaction, old_hash).await?;
    }
    if !old_hashes.is_empty() {
        sqlx::query(
            r#"UPDATE "PlatformSettingsState"
                  SET revision = revision + 1, updated_at = clock_timestamp()
                WHERE singleton = 1"#,
        )
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(old_hashes)
}

#[cfg(test)]
mod branding_tests;

#[cfg(test)]
mod tests {
    use super::{normalized_container_port_mapping, ConfigEditModel};

    #[test]
    fn platform_proxy_is_the_safe_default() {
        assert_eq!(normalized_container_port_mapping(None), "PlatformProxy");
        assert_eq!(normalized_container_port_mapping(Some("")), "PlatformProxy");
        assert_eq!(
            normalized_container_port_mapping(Some("invalid")),
            "PlatformProxy"
        );
        assert_eq!(
            normalized_container_port_mapping(Some("Default")),
            "Default"
        );
    }

    #[test]
    fn port_mapping_preference_is_admin_editable() {
        let model: ConfigEditModel = serde_json::from_value(serde_json::json!({
            "containerProvider": { "portMappingType": "Default" }
        }))
        .unwrap();
        assert_eq!(
            model.container_provider.unwrap()["portMappingType"],
            "Default"
        );
    }

    #[test]
    fn proxy_trust_is_read_only_on_settings_input() {
        let model: ConfigEditModel = serde_json::from_value(serde_json::json!({
            "globalConfig": { "title": "updated" },
            "proxyTrust": {
                "enabled": true,
                "trustedNetworksCsv": "0.0.0.0/0"
            }
        }))
        .unwrap();

        assert!(model.proxy_trust.is_none());
        assert_eq!(model.global_config.unwrap().title, "updated");
    }
}
