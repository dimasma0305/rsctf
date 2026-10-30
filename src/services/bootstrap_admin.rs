//! First-run Docker setup. Tokens are generated once, shared by replicas, and
//! removed in the same transaction that creates the first administrator.
use std::fmt;

use sqlx::{Executor, PgPool, Postgres};

use crate::utils::codec::random_hex;
use crate::utils::error::{AppError, AppResult};

pub const TOKEN_ENV: &str = "RSCTF_BOOTSTRAP_TOKEN";

pub fn auto_enabled() -> anyhow::Result<bool> {
    match std::env::var("RSCTF_BOOTSTRAP_AUTO").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("false" | "0" | "") => Ok(false),
        Ok("true" | "1") => Ok(true),
        _ => anyhow::bail!("RSCTF_BOOTSTRAP_AUTO must be true or false"),
    }
}

pub fn configured_token() -> Option<String> {
    std::env::var(TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty())
}

pub async fn token<'e, E>(executor: E) -> AppResult<Option<String>>
where
    E: Executor<'e, Database = Postgres>,
{
    resolve_token(executor, configured_token()).await
}

pub(crate) async fn consume_token<'e, E>(executor: E) -> AppResult<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(r#"DELETE FROM "BootstrapSetup" WHERE singleton"#)
        .execute(executor)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn resolve_token<'e, E>(executor: E, configured: Option<String>) -> AppResult<Option<String>>
where
    E: Executor<'e, Database = Postgres>,
{
    if let Some(value) = configured {
        return Ok(Some(value));
    }
    sqlx::query_scalar(r#"SELECT token FROM "BootstrapSetup" WHERE singleton"#)
        .fetch_optional(executor)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

pub struct SetupNotice {
    token: String,
}

impl fmt::Debug for SetupNotice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SetupNotice(<redacted>)")
    }
}

impl SetupNotice {
    /// This is the intentional credential announcement requested for Docker
    /// first boot. The URL never contains the token, and ordinary Debug is redacted.
    pub fn print(&self, public_url: &str) {
        eprintln!(
            "\nRSCTF FIRST-TIME SETUP\nOpen: {}/account/register?bootstrap=1\nSetup token: {}\nCopy the token into the form and create your administrator.\nThis token stops authorizing administrator setup after the first account is created.\n",
            public_url.trim_end_matches('/'), self.token
        );
    }
}

/// Called only after a public server has bound its socket and is ready to serve.
/// Registration and first-run initialization share the same admission lock.
pub async fn prepare(
    pool: &PgPool,
    configured: Option<&str>,
) -> anyhow::Result<Option<SetupNotice>> {
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(crate::services::mail_outbox::REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await?;
    let has_users: bool = sqlx::query_scalar(r#"SELECT EXISTS (SELECT 1 FROM "AspNetUsers")"#)
        .fetch_one(&mut *transaction)
        .await?;
    if has_users {
        transaction.commit().await?;
        return Ok(None);
    }
    let generated;
    let value = if let Some(value) = configured.filter(|value| !value.is_empty()) {
        anyhow::ensure!(
            value.len() >= 32,
            "RSCTF_BOOTSTRAP_TOKEN must contain at least 32 bytes"
        );
        value
    } else {
        generated = random_hex(32);
        &generated
    };
    sqlx::query(
        r#"INSERT INTO "BootstrapSetup" (singleton, token) VALUES (TRUE, $1)
           ON CONFLICT (singleton) DO NOTHING"#,
    )
    .bind(value)
    .execute(&mut *transaction)
    .await?;
    // An explicit operator rotation while setup is pending must display the
    // same code that registration will accept, without rotating generated codes.
    if configured.is_some_and(|value| !value.is_empty()) {
        sqlx::query(
            r#"UPDATE "BootstrapSetup" SET token = $1
               WHERE singleton AND token <> $1"#,
        )
        .bind(value)
        .execute(&mut *transaction)
        .await?;
    }
    // Re-announce the same pending code on restart. Lost container logs must
    // not lock an owner out before their first administrator exists.
    let token: String = sqlx::query_scalar(r#"SELECT token FROM "BootstrapSetup" WHERE singleton"#)
        .fetch_one(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(Some(SetupNotice { token }))
}

#[cfg(test)]
#[path = "bootstrap_admin_tests.rs"]
mod tests;
