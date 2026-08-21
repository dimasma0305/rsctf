use std::collections::BTreeMap;

use sqlx::{Postgres, Transaction};

use super::{database_error, PolicyFlags};
use crate::models::internal::configs::AppConfig;
use crate::services::captcha::{CaptchaAdmission, CaptchaSettings};
use crate::utils::error::AppResult;

const IDENTITY_POLICY_LOCK_ID: i64 = 0x4944_504F_4C49_4359; // "IDPOLICY"

#[derive(Clone)]
pub(crate) struct AccountPolicySnapshot {
    pub identity: PolicyFlags,
    pub allow_register: bool,
    pub active_on_register: bool,
    pub email_confirmation_required: bool,
    pub email_domain_list: String,
    captcha: CaptchaSettings,
}

impl AccountPolicySnapshot {
    pub fn authorize_captcha(&self, admission: CaptchaAdmission) -> AppResult<()> {
        self.captcha.authorize(admission)
    }
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
