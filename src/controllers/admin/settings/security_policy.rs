use sqlx::{Postgres, Transaction};

use super::{AccountPolicy, CaptchaConfig, OAuthConfig};
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
    oauth: Option<OAuthConfig>,
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
    if let Some(oauth) = oauth {
        write_oauth_policy(&mut transaction, oauth).await?;
    }

    // Resolve and validate the effective merged state before commit. In
    // particular, an omitted/empty incoming secret preserves the stored one;
    // enabling Turnstile with no effective secret rolls the whole update back.
    CaptchaSettings::load_in_transaction(&mut transaction, config.account.use_captcha).await?;
    let account = anti_cheat::load_account_policy_after_lock(&mut transaction, config).await?;
    let oauth_configured =
        crate::services::oauth_config::OAuthSettings::load_in_transaction(&mut transaction)
            .await?
            .any_configured();
    anti_cheat::validate_oauth_only_registration(&account, config, oauth_configured)?;
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
            "AccountPolicy:AllowPasswordRegistration",
            account.allow_password_registration.to_string(),
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

async fn write_oauth_policy(
    transaction: &mut Transaction<'_, Postgres>,
    oauth: OAuthConfig,
) -> AppResult<()> {
    let client_id_keys = ["OAuthConfig:GoogleClientId", "OAuthConfig:DiscordClientId"];
    let persisted_client_ids = sqlx::query_scalar::<_, String>(
        r#"SELECT config_key
             FROM "Configs"
            WHERE config_key = ANY($1)"#,
    )
    .bind(&client_id_keys[..])
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    for (key, environment_key, value) in [
        (
            "OAuthConfig:GoogleClientId",
            "RSCTF_GOOGLE_CLIENT_ID",
            oauth.google_client_id,
        ),
        (
            "OAuthConfig:DiscordClientId",
            "RSCTF_DISCORD_CLIENT_ID",
            oauth.discord_client_id,
        ),
    ] {
        if let Some(value) = value {
            let already_persisted = persisted_client_ids.iter().any(|saved| saved == key);
            let fallback = std::env::var(environment_key).ok();
            if should_persist_client_id(already_persisted, &value, fallback.as_deref()) {
                upsert(transaction, key, value).await?;
            }
        }
    }
    for (key, value) in [
        ("OAuthConfig:GoogleClientSecret", oauth.google_client_secret),
        (
            "OAuthConfig:DiscordClientSecret",
            oauth.discord_client_secret,
        ),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            upsert(transaction, key, value).await?;
        }
    }
    Ok(())
}

fn should_persist_client_id(
    already_persisted: bool,
    incoming: &str,
    environment_fallback: Option<&str>,
) -> bool {
    if already_persisted {
        return true;
    }
    let environment_fallback = environment_fallback
        .map(str::trim)
        .filter(|value| !value.is_empty());
    environment_fallback != Some(incoming.trim())
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
        let confirmation = AccountPolicy {
            email_confirmation_required: true,
            ..AccountPolicy::default()
        };
        let missing_origin = save_security_policy(&pool, &config, Some(confirmation), None, None)
            .await
            .expect_err("email confirmation was enabled without a public origin");
        assert_eq!(missing_origin.status(), axum::http::StatusCode::BAD_REQUEST);
        let persisted: i64 = sqlx::query_scalar(r#"SELECT COUNT(*)::bigint FROM "Configs""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(persisted, 0);
        config.public_url = Some("https://ctf.example".to_string());

        save_security_policy(
            &pool,
            &config,
            None,
            Some(captcha("HashPow", None, 18)),
            None,
        )
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
            let enabled = AccountPolicy {
                use_captcha: true,
                ..AccountPolicy::default()
            };
            let error = save_security_policy(&pool, &config, Some(enabled), Some(invalid), None)
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

        let disabled = AccountPolicy {
            use_captcha: false,
            ..AccountPolicy::default()
        };
        save_security_policy(
            &pool,
            &config,
            Some(disabled),
            Some(captcha("CloudflareTurnstile", Some("stored-secret"), 18)),
            None,
        )
        .await
        .unwrap();
        let enabled = AccountPolicy {
            use_captcha: true,
            ..AccountPolicy::default()
        };
        save_security_policy(
            &pool,
            &config,
            Some(enabled),
            Some(CaptchaConfig {
                site_key: Some(String::new()),
                ..captcha("CloudflareTurnstile", Some(""), 18)
            }),
            None,
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

        sqlx::query(
            r#"INSERT INTO "Configs" (config_key, value, cache_keys)
               VALUES
                 ('OAuthConfig:GoogleClientId', '', NULL),
                 ('OAuthConfig:GoogleClientSecret', '', NULL),
                 ('OAuthConfig:DiscordClientId', '', NULL),
                 ('OAuthConfig:DiscordClientSecret', '', NULL)
               ON CONFLICT (config_key) DO UPDATE SET value = EXCLUDED.value"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        config.public_url = None;
        let missing_oauth_origin = save_security_policy(
            &pool,
            &config,
            Some(AccountPolicy {
                allow_password_registration: false,
                use_captcha: false,
                ..AccountPolicy::default()
            }),
            None,
            Some(oauth("google-id", "google-secret")),
        )
        .await
        .expect_err("OAuth-only registration committed without a public origin");
        assert_eq!(
            missing_oauth_origin.status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        config.public_url = Some("https://ctf.example".to_string());

        let oauth_only = AccountPolicy {
            allow_password_registration: false,
            use_captcha: false,
            ..AccountPolicy::default()
        };
        let missing_provider = save_security_policy(&pool, &config, Some(oauth_only), None, None)
            .await
            .expect_err("OAuth-only registration committed without a provider");
        assert_eq!(
            missing_provider.status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let password_registration: String = sqlx::query_scalar(
            r#"SELECT value FROM "Configs"
                WHERE config_key='AccountPolicy:AllowPasswordRegistration'"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(password_registration, "true");

        save_security_policy(
            &pool,
            &config,
            Some(AccountPolicy {
                allow_password_registration: false,
                use_captcha: false,
                ..AccountPolicy::default()
            }),
            None,
            Some(oauth("google-id", "google-secret")),
        )
        .await
        .expect("configured OAuth provider should enable OAuth-only registration");
        let effective = crate::services::oauth_config::OAuthSettings::load(&pool)
            .await
            .unwrap();
        assert!(effective.google_configured());
        let fast_rejection =
            crate::services::anti_cheat::preflight_password_registration(&pool, &config, false)
                .await
                .expect_err("OAuth-only policy did not reject the cheap password preflight");
        assert_eq!(fast_rejection.status(), axum::http::StatusCode::BAD_REQUEST);
        crate::services::anti_cheat::preflight_password_registration(&pool, &config, true)
            .await
            .expect("first-administrator bootstrap must bypass the fast policy rejection");
        config.public_url = None;
        let persisted_startup =
            crate::services::anti_cheat::validate_registration_startup(&pool, &config)
                .await
                .expect_err("persisted OAuth-only policy started without a public origin");
        assert_eq!(
            persisted_startup.status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        sqlx::query(
            r#"DELETE FROM "Configs"
                WHERE config_key='AccountPolicy:AllowPasswordRegistration'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        config.account.allow_password_registration = false;
        let fallback_startup =
            crate::services::anti_cheat::validate_registration_startup(&pool, &config)
                .await
                .expect_err("environment OAuth-only policy started without a public origin");
        assert_eq!(
            fallback_startup.status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        config.public_url = Some("https://ctf.example".to_string());
        crate::services::anti_cheat::validate_registration_startup(&pool, &config)
            .await
            .expect("usable OAuth-only policy must pass startup validation");

        let fingerprint_conflict = save_security_policy(
            &pool,
            &config,
            Some(AccountPolicy {
                allow_password_registration: false,
                enable_browser_fingerprint: true,
                use_captcha: false,
                ..AccountPolicy::default()
            }),
            None,
            None,
        )
        .await
        .expect_err("fingerprinting committed with OAuth-only registration");
        assert_eq!(
            fingerprint_conflict.status(),
            axum::http::StatusCode::BAD_REQUEST
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

    fn oauth(client_id: &str, client_secret: &str) -> OAuthConfig {
        OAuthConfig {
            google_client_id: Some(client_id.to_string()),
            google_client_secret: Some(client_secret.to_string()),
            discord_client_id: None,
            discord_client_secret: None,
            has_google_client_secret: false,
            has_discord_client_secret: false,
        }
    }

    #[test]
    fn unchanged_environment_client_ids_are_not_promoted_to_database_overrides() {
        assert!(!should_persist_client_id(
            false,
            "deployment-id",
            Some(" deployment-id ")
        ));
        assert!(should_persist_client_id(
            false,
            "admin-override",
            Some("deployment-id")
        ));
        assert!(should_persist_client_id(false, "", Some("deployment-id")));
        assert!(should_persist_client_id(false, "new-id", None));
        assert!(should_persist_client_id(
            true,
            "deployment-id",
            Some("deployment-id")
        ));
    }
}
