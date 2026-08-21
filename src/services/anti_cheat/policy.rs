use std::collections::BTreeMap;

use sqlx::{Postgres, Transaction};

use super::{database_error, PolicyFlags};
use crate::models::internal::configs::AppConfig;
use crate::services::captcha::{CaptchaAdmission, CaptchaSettings};
use crate::services::oauth_config::OAuthSettings;
use crate::utils::error::AppResult;

const IDENTITY_POLICY_LOCK_ID: i64 = 0x4944_504F_4C49_4359; // "IDPOLICY"

#[derive(Clone)]
pub(crate) struct AccountPolicySnapshot {
    pub identity: PolicyFlags,
    pub allow_register: bool,
    pub allow_password_registration: bool,
    pub active_on_register: bool,
    pub email_confirmation_required: bool,
    pub email_domain_list: String,
    captcha: CaptchaSettings,
}

impl AccountPolicySnapshot {
    pub fn authorize_captcha(&self, admission: CaptchaAdmission) -> AppResult<()> {
        self.captcha.authorize(admission)
    }

    pub fn authorize_password_registration(&self, is_first: bool) -> AppResult<()> {
        authorize_password_registration(
            self.allow_register,
            self.allow_password_registration,
            is_first,
        )
    }
}

fn authorize_password_registration(
    allow_register: bool,
    allow_password_registration: bool,
    is_first: bool,
) -> AppResult<()> {
    if is_first {
        return Ok(());
    }
    if !allow_register {
        return Err(crate::utils::error::AppError::bad_request(
            "Registration is disabled",
        ));
    }
    if !allow_password_registration {
        return Err(crate::utils::error::AppError::bad_request(
            "Password registration is disabled; continue with OAuth",
        ));
    }
    Ok(())
}

pub(crate) fn validate_oauth_only_registration(
    account: &AccountPolicySnapshot,
    config: &AppConfig,
    oauth_configured: bool,
) -> AppResult<()> {
    if !account.allow_register || account.allow_password_registration {
        return Ok(());
    }
    if account.identity.fingerprint_required() {
        return Err(crate::utils::error::AppError::bad_request(
            "OAuth-only registration is incompatible with browser fingerprinting",
        ));
    }
    if config.public_url.is_none() {
        return Err(crate::utils::error::AppError::bad_request(
            "OAuth-only registration requires a canonical RSCTF_PUBLIC_URL",
        ));
    }
    if !oauth_configured {
        return Err(crate::utils::error::AppError::bad_request(
            "OAuth-only registration requires a configured Google or Discord provider",
        ));
    }
    Ok(())
}

/// Refuse to serve a deployment whose effective environment/database policy
/// leaves OAuth as the only registration path without a usable OAuth flow.
pub async fn validate_registration_startup(
    pool: &sqlx::PgPool,
    config: &AppConfig,
) -> AppResult<()> {
    let mut transaction = pool.begin().await.map_err(database_error)?;
    let account = lock_and_load_account_policy(&mut transaction, config).await?;
    let oauth_configured = OAuthSettings::load_in_transaction(&mut transaction)
        .await?
        .any_configured();
    validate_oauth_only_registration(&account, config, oauth_configured)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(())
}

/// Reject a fresh password registration before fingerprint work and Argon2.
/// The transaction-locked check remains authoritative; this deliberately cheap
/// snapshot only avoids expensive work for a policy that is already disabled.
pub(crate) async fn preflight_password_registration(
    pool: &sqlx::PgPool,
    config: &AppConfig,
    is_first: bool,
) -> AppResult<()> {
    if is_first {
        return Ok(());
    }
    let keys = [
        "AccountPolicy:AllowRegister",
        "AccountPolicy:AllowPasswordRegistration",
    ];
    let rows = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT config_key, value
             FROM "Configs"
            WHERE config_key = ANY($1)"#,
    )
    .bind(&keys[..])
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    let values: BTreeMap<_, _> = rows.into_iter().collect();
    let bool_value = |key: &str, fallback: bool| {
        values
            .get(key)
            .and_then(|value| value.as_deref())
            .map(|value| value == "true")
            .unwrap_or(fallback)
    };
    authorize_password_registration(
        bool_value("AccountPolicy:AllowRegister", config.account.allow_register),
        bool_value(
            "AccountPolicy:AllowPasswordRegistration",
            config.account.allow_password_registration,
        ),
        false,
    )
}

/// Establish a linearization point for captcha-bearing flows that do not run
/// identity admission (currently password recovery).
pub(crate) async fn authorize_captcha_admission(
    pool: &sqlx::PgPool,
    config: &AppConfig,
    admission: CaptchaAdmission,
) -> AppResult<()> {
    let mut transaction = pool.begin().await.map_err(database_error)?;
    let policy = lock_and_load_account_policy(&mut transaction, config).await?;
    policy.authorize_captcha(admission)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(())
}

fn policy_flags_from_rows(rows: Vec<(String, Option<String>)>) -> AppResult<PolicyFlags> {
    let values: BTreeMap<_, _> = rows.into_iter().collect();
    let enabled = |key: &str| {
        values
            .get(key)
            .and_then(|value| value.as_deref())
            .is_some_and(|value| value == "true")
    };
    let policy = PolicyFlags {
        enable_browser_fingerprint: enabled("AccountPolicy:EnableBrowserFingerprint"),
        require_unique_ip_per_team_user: enabled("AccountPolicy:RequireUniqueIpPerTeamUser"),
        require_unique_fingerprint_per_team_user: enabled(
            "AccountPolicy:RequireUniqueFingerprintPerTeamUser",
        ),
        require_unique_ip_global: enabled("AccountPolicy:RequireUniqueIpGlobal"),
        require_unique_fingerprint_global: enabled("AccountPolicy:RequireUniqueFingerprintGlobal"),
    };
    policy.validate()?;
    Ok(policy)
}

pub async fn load_policy_flags(pool: &sqlx::PgPool) -> AppResult<PolicyFlags> {
    let rows = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT config_key, value
             FROM "Configs"
            WHERE config_key = ANY($1)"#,
    )
    .bind(identity_policy_keys())
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    policy_flags_from_rows(rows)
}

async fn load_policy_flags_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
) -> AppResult<PolicyFlags> {
    let rows = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT config_key, value
             FROM "Configs"
            WHERE config_key = ANY($1)"#,
    )
    .bind(identity_policy_keys())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    policy_flags_from_rows(rows)
}

pub(crate) async fn lock_and_load_admission_policy(
    transaction: &mut Transaction<'_, Postgres>,
) -> AppResult<PolicyFlags> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(IDENTITY_POLICY_LOCK_ID)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    load_policy_flags_in_transaction(transaction).await
}

/// Load one transaction-locked snapshot of every session-affecting policy,
/// including the captcha provider material used to validate a preflight proof.
pub(crate) async fn lock_and_load_account_policy(
    transaction: &mut Transaction<'_, Postgres>,
    config: &AppConfig,
) -> AppResult<AccountPolicySnapshot> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(IDENTITY_POLICY_LOCK_ID)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    load_account_policy_after_lock(transaction, config).await
}

/// Read the canonical account policy after the caller has already acquired the
/// shared or exclusive identity-policy advisory lock.
pub(crate) async fn load_account_policy_after_lock(
    transaction: &mut Transaction<'_, Postgres>,
    config: &AppConfig,
) -> AppResult<AccountPolicySnapshot> {
    let rows = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT config_key, value
             FROM "Configs"
            WHERE config_key LIKE 'AccountPolicy:%'
               OR config_key LIKE 'CaptchaConfig:%'"#,
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    let values: BTreeMap<_, _> = rows.into_iter().collect();
    let bool_value = |key: &str, fallback: bool| {
        values
            .get(key)
            .and_then(|value| value.as_deref())
            .map(|value| value == "true")
            .unwrap_or(fallback)
    };
    let identity = PolicyFlags {
        enable_browser_fingerprint: bool_value("AccountPolicy:EnableBrowserFingerprint", false),
        require_unique_ip_per_team_user: bool_value(
            "AccountPolicy:RequireUniqueIpPerTeamUser",
            false,
        ),
        require_unique_fingerprint_per_team_user: bool_value(
            "AccountPolicy:RequireUniqueFingerprintPerTeamUser",
            false,
        ),
        require_unique_ip_global: bool_value("AccountPolicy:RequireUniqueIpGlobal", false),
        require_unique_fingerprint_global: bool_value(
            "AccountPolicy:RequireUniqueFingerprintGlobal",
            false,
        ),
    };
    identity.validate()?;
    let captcha = CaptchaSettings::from_values(&values, config.account.use_captcha)?;
    Ok(AccountPolicySnapshot {
        identity,
        allow_register: bool_value("AccountPolicy:AllowRegister", config.account.allow_register),
        allow_password_registration: bool_value(
            "AccountPolicy:AllowPasswordRegistration",
            config.account.allow_password_registration,
        ),
        active_on_register: bool_value(
            "AccountPolicy:ActiveOnRegister",
            config.account.active_on_register,
        ),
        email_confirmation_required: bool_value(
            "AccountPolicy:EmailConfirmationRequired",
            config.account.email_confirmation_required,
        ),
        email_domain_list: values
            .get("AccountPolicy:EmailDomainList")
            .and_then(Clone::clone)
            .unwrap_or_default(),
        captcha,
    })
}

/// Serialize an account/captcha policy update against every canonical account
/// admission. The caller must keep this transaction through all related writes.
pub(crate) async fn lock_policy_update(
    transaction: &mut Transaction<'_, Postgres>,
) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(IDENTITY_POLICY_LOCK_ID)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    Ok(())
}

fn identity_policy_keys() -> &'static [&'static str] {
    &[
        "AccountPolicy:EnableBrowserFingerprint",
        "AccountPolicy:RequireUniqueIpPerTeamUser",
        "AccountPolicy:RequireUniqueFingerprintPerTeamUser",
        "AccountPolicy:RequireUniqueIpGlobal",
        "AccountPolicy:RequireUniqueFingerprintGlobal",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account_policy(
        allow_register: bool,
        allow_password_registration: bool,
    ) -> AccountPolicySnapshot {
        AccountPolicySnapshot {
            identity: PolicyFlags::default(),
            allow_register,
            allow_password_registration,
            active_on_register: true,
            email_confirmation_required: false,
            email_domain_list: String::new(),
            captcha: CaptchaSettings::from_values(&BTreeMap::new(), false).unwrap(),
        }
    }

    #[test]
    fn password_registration_policy_preserves_first_admin_bootstrap() {
        let oauth_only = account_policy(true, false);
        assert!(oauth_only.authorize_password_registration(true).is_ok());
        assert!(oauth_only.authorize_password_registration(false).is_err());

        let all_registration_disabled = account_policy(false, true);
        assert!(all_registration_disabled
            .authorize_password_registration(true)
            .is_ok());
        assert!(all_registration_disabled
            .authorize_password_registration(false)
            .is_err());
        assert!(account_policy(true, true)
            .authorize_password_registration(false)
            .is_ok());
    }
}
