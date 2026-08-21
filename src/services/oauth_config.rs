//! Effective Google/Discord OAuth credentials.
//!
//! Admin settings override startup environment values. An explicitly persisted
//! empty value disables that provider field instead of falling back to the
//! environment, while a missing setting preserves the deployment fallback.

use std::collections::BTreeMap;

use sqlx::{PgPool, Postgres, Transaction};

use crate::utils::error::{AppError, AppResult};

const OAUTH_CONFIG_KEYS: &[&str] = &[
    "OAuthConfig:GoogleClientId",
    "OAuthConfig:GoogleClientSecret",
    "OAuthConfig:DiscordClientId",
    "OAuthConfig:DiscordClientSecret",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OAuthProviderCredentials {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OAuthSettings {
    google: Option<OAuthProviderCredentials>,
    discord: Option<OAuthProviderCredentials>,
}

impl OAuthSettings {
    pub(crate) async fn load(pool: &PgPool) -> AppResult<Self> {
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            r#"SELECT config_key, value
                 FROM "Configs"
                WHERE config_key = ANY($1)"#,
        )
        .bind(OAUTH_CONFIG_KEYS)
        .fetch_all(pool)
        .await
        .map_err(database_error)?;
        Ok(Self::from_rows(rows, environment_value))
    }

    pub(crate) async fn load_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
    ) -> AppResult<Self> {
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            r#"SELECT config_key, value
                 FROM "Configs"
                WHERE config_key = ANY($1)"#,
        )
        .bind(OAUTH_CONFIG_KEYS)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
        Ok(Self::from_rows(rows, environment_value))
    }

    pub(crate) fn provider(&self, provider: &str) -> Option<&OAuthProviderCredentials> {
        match provider {
            "google" => self.google.as_ref(),
            "discord" => self.discord.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn google_configured(&self) -> bool {
        self.google.is_some()
    }

    pub(crate) fn discord_configured(&self) -> bool {
        self.discord.is_some()
    }

    pub(crate) fn any_configured(&self) -> bool {
        self.google_configured() || self.discord_configured()
    }

    fn from_rows(
        rows: Vec<(String, Option<String>)>,
        fallback: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let values: BTreeMap<_, _> = rows.into_iter().collect();
        Self {
            google: provider_credentials(&values, "Google", "GOOGLE", &fallback),
            discord: provider_credentials(&values, "Discord", "DISCORD", &fallback),
        }
    }
}

fn provider_credentials(
    values: &BTreeMap<String, Option<String>>,
    config_name: &str,
    env_name: &str,
    fallback: &impl Fn(&str) -> Option<String>,
) -> Option<OAuthProviderCredentials> {
    let client_id = effective_value(
        values,
        &format!("OAuthConfig:{config_name}ClientId"),
        &format!("RSCTF_{env_name}_CLIENT_ID"),
        fallback,
    )?;
    let client_secret = effective_value(
        values,
        &format!("OAuthConfig:{config_name}ClientSecret"),
        &format!("RSCTF_{env_name}_CLIENT_SECRET"),
        fallback,
    )?;
    Some(OAuthProviderCredentials {
        client_id,
        client_secret,
    })
}

fn effective_value(
    values: &BTreeMap<String, Option<String>>,
    config_key: &str,
    env_key: &str,
    fallback: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let value = match values.get(config_key) {
        Some(value) => value.clone(),
        None => fallback(env_key),
    }?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn environment_value(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_credentials_override_fallback_and_empty_values_disable() {
        let fallback = |key: &str| Some(format!("fallback-{key}"));
        let configured = OAuthSettings::from_rows(
            vec![
                (
                    "OAuthConfig:GoogleClientId".to_string(),
                    Some("db-id".to_string()),
                ),
                (
                    "OAuthConfig:GoogleClientSecret".to_string(),
                    Some("db-secret".to_string()),
                ),
                (
                    "OAuthConfig:DiscordClientId".to_string(),
                    Some(String::new()),
                ),
            ],
            fallback,
        );

        assert_eq!(
            configured.provider("google"),
            Some(&OAuthProviderCredentials {
                client_id: "db-id".to_string(),
                client_secret: "db-secret".to_string(),
            })
        );
        assert!(!configured.discord_configured());
        assert!(configured.any_configured());
    }
}
