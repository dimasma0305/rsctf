use sqlx::{Postgres, Transaction};

use super::{AccountPolicy, CaptchaConfig};
use crate::models::internal::configs::AppConfig;
use crate::services::anti_cheat::{self, PolicyFlags};
use crate::services::captcha::CaptchaSettings;
use crate::utils::error::{AppError, AppResult};

/// Persist the entire account/captcha authorization surface in one
/// transaction. The exclusive policy lock linearizes it against registration,
/// password admission, recovery, OAuth policy checks, and roster admission.
pub(super) async fn save_security_policy(
    pool: &sqlx::PgPool,
    config: &AppConfig,
    account: Option<AccountPolicy>,
    captcha: Option<CaptchaConfig>,
) -> AppResult<()> {
    if let Some(account) = account.as_ref() {
        if account.email_confirmation_required && config.public_url.is_none() {
            return Err(AppError::bad_request(
                "Email confirmation requires a canonical RSCTF_PUBLIC_URL",
            ));
        }
        PolicyFlags {
            enable_browser_fingerprint: account.enable_browser_fingerprint,
            require_unique_ip_per_team_user: account.require_unique_ip_per_team_user,
            require_unique_fingerprint_per_team_user: account
                .require_unique_fingerprint_per_team_user,
            require_unique_ip_global: account.require_unique_ip_global,
            require_unique_fingerprint_global: account.require_unique_fingerprint_global,
        }
        .validate()?;
    }

    let mut transaction = pool.begin().await.map_err(database_error)?;
    anti_cheat::lock_policy_update(&mut transaction).await?;
    if let Some(account) = account {
        write_account_policy(&mut transaction, account).await?;
    }
    if let Some(captcha) = captcha {
        write_captcha_policy(&mut transaction, captcha).await?;
    }

    // Resolve and validate the effective merged state before commit. In
    // particular, an omitted/empty incoming secret preserves the stored one;
    // enabling Turnstile with no effective secret rolls the whole update back.
    CaptchaSettings::load_in_transaction(&mut transaction, config.account.use_captcha).await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(())
}

async fn write_account_policy(
    transaction: &mut Transaction<'_, Postgres>,
    account: AccountPolicy,
) -> AppResult<()> {
    let values = [
        (
            "AccountPolicy:AllowRegister",
            account.allow_register.to_string(),
        ),
        (
            "AccountPolicy:ActiveOnRegister",
            account.active_on_register.to_string(),
        ),
        ("AccountPolicy:UseCaptcha", account.use_captcha.to_string()),
        (
            "AccountPolicy:EmailConfirmationRequired",
            account.email_confirmation_required.to_string(),
        ),
        ("AccountPolicy:EmailDomainList", account.email_domain_list),
        (
            "AccountPolicy:EnableBrowserFingerprint",
            account.enable_browser_fingerprint.to_string(),
        ),
        (
            "AccountPolicy:RequireUniqueIpPerTeamUser",
            account.require_unique_ip_per_team_user.to_string(),
        ),
        (
            "AccountPolicy:RequireUniqueFingerprintPerTeamUser",
            account.require_unique_fingerprint_per_team_user.to_string(),
        ),
        (
            "AccountPolicy:RequireUniqueIpGlobal",
            account.require_unique_ip_global.to_string(),
        ),
        (
            "AccountPolicy:RequireUniqueFingerprintGlobal",
            account.require_unique_fingerprint_global.to_string(),
        ),
    ];
    for (key, value) in values {
        upsert(transaction, key, value).await?;
    }
    Ok(())
}

async fn write_captcha_policy(
    transaction: &mut Transaction<'_, Postgres>,
    captcha: CaptchaConfig,
) -> AppResult<()> {
    upsert(transaction, "CaptchaConfig:Provider", captcha.provider).await?;
    if let Some(site_key) = captcha
        .site_key
        .filter(|site_key| !site_key.trim().is_empty())
    {
        upsert(transaction, "CaptchaConfig:SiteKey", site_key).await?;
    }
    if let Some(secret) = captcha.secret_key.filter(|secret| !secret.is_empty()) {
        upsert(transaction, "CaptchaConfig:SecretKey", secret).await?;
    }
    if let Some(hash_pow) = captcha.hash_pow {
        upsert(
            transaction,
            "CaptchaConfig:HashPow:Difficulty",
            hash_pow.difficulty.to_string(),
        )
        .await?;
    }
    Ok(())
}

async fn upsert(
    transaction: &mut Transaction<'_, Postgres>,
    key: &str,
    value: String,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key, value, cache_keys)
           VALUES ($1, $2, NULL)
           ON CONFLICT (config_key) DO UPDATE SET value = EXCLUDED.value"#,
    )
    .bind(key)
    .bind(value)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;
    use crate::controllers::admin::settings::HashPowConfig;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn invalid_captcha_combinations_roll_back_and_existing_secret_is_preserved() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_captcha_policy_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin_pool)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "Configs" (
                   config_key TEXT PRIMARY KEY,
                   value TEXT,
                   cache_keys TEXT
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut config = AppConfig::from_env();
        config.account.use_captcha = false;

        config.public_url = None;
        let mut confirmation = AccountPolicy::default();
        confirmation.email_confirmation_required = true;
        let missing_origin = save_security_policy(&pool, &config, Some(confirmation), None)
            .await
            .expect_err("email confirmation was enabled without a public origin");
        assert_eq!(missing_origin.status(), axum::http::StatusCode::BAD_REQUEST);
        let persisted: i64 = sqlx::query_scalar(r#"SELECT COUNT(*)::bigint FROM "Configs""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(persisted, 0);
        config.public_url = Some("https://ctf.example".to_string());

        save_security_policy(&pool, &config, None, Some(captcha("HashPow", None, 18)))
            .await
            .unwrap();
        for invalid in [
            captcha("None", None, 18),
            captcha("UnknownProvider", None, 18),
            captcha("CloudflareTurnstile", None, 18),
            CaptchaConfig {
                site_key: Some(String::new()),
                ..captcha("CloudflareTurnstile", Some("secret-without-site"), 18)
            },
        ] {
            let mut enabled = AccountPolicy::default();
            enabled.use_captcha = true;
            let error = save_security_policy(&pool, &config, Some(enabled), Some(invalid))
                .await
                .expect_err("invalid enabled captcha combination committed");
            assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
            let rows = sqlx::query_as::<_, (String, Option<String>)>(
                r#"SELECT config_key, value FROM "Configs"
                    WHERE config_key IN (
                        'AccountPolicy:UseCaptcha', 'CaptchaConfig:Provider'
                    )
                    ORDER BY config_key"#,
            )
            .fetch_all(&pool)
            .await
            .unwrap();
            assert_eq!(
                rows,
                vec![(
                    "CaptchaConfig:Provider".to_string(),
                    Some("HashPow".to_string())
                )]
            );
        }

        let mut disabled = AccountPolicy::default();
        disabled.use_captcha = false;
        save_security_policy(
            &pool,
            &config,
            Some(disabled),
            Some(captcha("CloudflareTurnstile", Some("stored-secret"), 18)),
        )
        .await
        .unwrap();
        let mut enabled = AccountPolicy::default();
        enabled.use_captcha = true;
        save_security_policy(
            &pool,
            &config,
            Some(enabled),
            Some(CaptchaConfig {
                site_key: Some(String::new()),
                ..captcha("CloudflareTurnstile", Some(""), 18)
            }),
        )
        .await
        .expect("empty incoming secret should preserve the existing secret");
        let saved = sqlx::query_as::<_, (String, String, String)>(
            r#"SELECT
                   (SELECT value FROM "Configs"
                     WHERE config_key='AccountPolicy:UseCaptcha'),
                   (SELECT value FROM "Configs"
                     WHERE config_key='CaptchaConfig:SecretKey'),
                   (SELECT value FROM "Configs"
                     WHERE config_key='CaptchaConfig:SiteKey')"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            saved,
            (
                "true".to_string(),
                "stored-secret".to_string(),
                "turnstile-site".to_string()
            )
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin_pool)
            .await
            .unwrap();
    }

    fn captcha(provider: &str, secret_key: Option<&str>, difficulty: i32) -> CaptchaConfig {
        CaptchaConfig {
            provider: provider.to_string(),
            site_key: (provider == "CloudflareTurnstile").then(|| "turnstile-site".to_string()),
            secret_key: secret_key.map(str::to_string),
            hash_pow: Some(HashPowConfig { difficulty }),
            has_secret_key: secret_key.is_some_and(|secret| !secret.is_empty()),
        }
    }
}
