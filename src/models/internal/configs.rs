//! Application configuration, loaded from environment variables with
//! sensible defaults so the server boots for local development.

use std::env;
use std::fmt;
use std::str::FromStr;

/// Which responsibilities this copy of the single rsctf binary owns.
///
/// `all` deliberately preserves the historical one-process deployment. The
/// narrower roles let larger installations run more copies of the same image
/// without every HTTP replica also trying to own the round engine or host
/// networking.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeRole {
    #[default]
    All,
    Web,
    Control,
    Engine,
    Network,
    Migrate,
}

/// Static role capabilities used both by the composition root and readiness
/// metadata. These are intentionally coarse: operators choose a supported
/// topology instead of assembling arbitrary combinations of internal services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub api: bool,
    pub health: bool,
    pub maintenance: bool,
    pub round_engine: bool,
    pub network: bool,
    pub migrations: bool,
}

impl RuntimeRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Web => "web",
            Self::Control => "control",
            Self::Engine => "engine",
            Self::Network => "network",
            Self::Migrate => "migrate",
        }
    }

    pub const fn capabilities(self) -> RuntimeCapabilities {
        match self {
            Self::All => RuntimeCapabilities {
                api: true,
                health: true,
                maintenance: true,
                round_engine: true,
                network: true,
                migrations: true,
            },
            Self::Web => RuntimeCapabilities {
                api: true,
                health: true,
                maintenance: false,
                round_engine: false,
                network: false,
                migrations: false,
            },
            Self::Control => RuntimeCapabilities {
                api: true,
                health: true,
                maintenance: true,
                round_engine: true,
                network: true,
                migrations: false,
            },
            Self::Engine => RuntimeCapabilities {
                api: false,
                health: true,
                maintenance: true,
                round_engine: true,
                network: false,
                migrations: false,
            },
            Self::Network => RuntimeCapabilities {
                // BYOC agents connect over an HTTP WebSocket route, so a
                // network owner must serve the API router. Deployments should
                // route only network-owned paths to this role.
                api: true,
                health: true,
                maintenance: false,
                // Network-bound (BYOC) games must advance on the replica that
                // owns their live tunnel registry. The scheduler is scoped to
                // those games; managed-container games stay with `engine`.
                round_engine: true,
                network: true,
                migrations: false,
            },
            Self::Migrate => RuntimeCapabilities {
                api: false,
                health: false,
                maintenance: false,
                round_engine: false,
                network: false,
                migrations: true,
            },
        }
    }

    /// Stable, allocation-free representation for readiness response metadata.
    pub const fn capability_header(self) -> &'static str {
        match self {
            Self::All => "api,health,maintenance,round-engine,network,migrations",
            Self::Web => "api,health",
            Self::Control => "api,health,maintenance,round-engine,network",
            Self::Engine => "health,maintenance,round-engine",
            Self::Network => "api,health,network,network-rounds",
            Self::Migrate => "migrations",
        }
    }
}

impl fmt::Display for RuntimeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RuntimeRole {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "web" => Ok(Self::Web),
            "control" => Ok(Self::Control),
            "engine" => Ok(Self::Engine),
            "network" => Ok(Self::Network),
            "migrate" => Ok(Self::Migrate),
            _ => anyhow::bail!(
                "invalid RSCTF_ROLE {value:?}; expected all, web, control, engine, network, or migrate"
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub runtime_role: RuntimeRole,
    runtime_role_error: Option<String>,
    pub bind_addr: String,
    pub database_url: String,
    pub redis_url: Option<String>,
    pub jwt_secret: String,
    pub jwt_ttl_secs: i64,
    jwt_ttl_error: Option<String>,
    /// Emit the session cookie with `Secure`. Disable only for explicit local
    /// development over plain HTTP.
    pub cookie_secure: bool,
    /// Canonical browser-facing base URL. When absent, request security derives
    /// the expected scheme from `cookie_secure` and the request Host header.
    pub public_url: Option<String>,
    pub storage_root: String,
    pub account: AccountPolicy,
    pub global: GlobalConfig,
}

#[derive(Debug, Clone)]
pub struct AccountPolicy {
    pub allow_register: bool,
    pub email_confirmation_required: bool,
    pub admin_confirmation_required: bool,
    /// Whether newly-registered accounts are active immediately.
    pub active_on_register: bool,
    pub use_captcha: bool,
}

#[derive(Debug, Clone)]
pub struct GlobalConfig {
    pub title: String,
    pub slogan: String,
    pub footer_info: Option<String>,
}

impl Default for AccountPolicy {
    fn default() -> Self {
        Self {
            allow_register: true,
            email_confirmation_required: false,
            admin_confirmation_required: false,
            active_on_register: true,
            use_captcha: false,
        }
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            title: "rsctf".into(),
            slogan: "Capture. Compete. Conquer.".into(),
            footer_info: None,
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .and_then(|v| match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

impl AppConfig {
    pub fn from_env() -> Self {
        let (runtime_role, runtime_role_error) = match env::var("RSCTF_ROLE") {
            Ok(value) => match value.parse::<RuntimeRole>() {
                Ok(role) => (role, None),
                Err(error) => (RuntimeRole::All, Some(error.to_string())),
            },
            Err(_) => (RuntimeRole::All, None),
        };
        let (jwt_ttl_secs, jwt_ttl_error) = parse_jwt_ttl(env::var("RSCTF_JWT_TTL_SECS").ok());
        Self {
            // Invalid values are rejected by `validate`. Keeping `from_env`
            // infallible preserves its existing public contract.
            runtime_role,
            runtime_role_error,
            bind_addr: env_or("RSCTF_BIND", "0.0.0.0:8080"),
            database_url: env_or(
                "RSCTF_DATABASE_URL",
                "postgres://postgres:postgres@localhost:5432/rsctf",
            ),
            redis_url: env::var("RSCTF_REDIS_URL").ok(),
            jwt_secret: env_or("RSCTF_JWT_SECRET", "insecure-dev-secret-change-me"),
            jwt_ttl_secs,
            jwt_ttl_error,
            cookie_secure: env_bool("RSCTF_COOKIE_SECURE", true),
            public_url: env::var("RSCTF_PUBLIC_URL")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            storage_root: env_or("RSCTF_STORAGE_ROOT", "./files"),
            account: AccountPolicy {
                allow_register: env_bool("RSCTF_ALLOW_REGISTER", true),
                email_confirmation_required: env_bool("RSCTF_EMAIL_CONFIRM", false),
                admin_confirmation_required: env_bool("RSCTF_ADMIN_CONFIRM", false),
                active_on_register: env_bool("RSCTF_ACTIVE_ON_REGISTER", true),
                use_captcha: env_bool("RSCTF_USE_CAPTCHA", false),
            },
            global: GlobalConfig::default(),
        }
    }

    /// Reject secrets that make every session forgeable. This is deliberately a
    /// startup error instead of a warning: a public service must never silently
    /// run with a repository-known signing key.
    pub fn validate(&self) -> anyhow::Result<()> {
        self.validate_runtime_role()?;
        validate_jwt_secret(&self.jwt_secret)?;
        if let Some(error) = self.jwt_ttl_error.as_deref() {
            anyhow::bail!(error.to_string());
        }
        if self.jwt_ttl_secs <= 0 {
            anyhow::bail!("RSCTF_JWT_TTL_SECS must be a positive integer");
        }
        if let Some(public_url) = self.public_url.as_deref() {
            validate_public_url(public_url)?;
        }
        Ok(())
    }

    /// Role validation is separate so the migration-only process can reject a
    /// typo without requiring HTTP-only configuration such as a JWT secret.
    pub fn validate_runtime_role(&self) -> anyhow::Result<()> {
        if let Some(error) = self.runtime_role_error.as_deref() {
            anyhow::bail!(error.to_string());
        }
        Ok(())
    }
}

fn parse_jwt_ttl(value: Option<String>) -> (i64, Option<String>) {
    let Some(value) = value else {
        return (604_800, None);
    };
    match value.trim().parse::<i64>() {
        Ok(ttl) => (ttl, None),
        Err(_) => (
            604_800,
            Some("RSCTF_JWT_TTL_SECS must be a positive integer".to_string()),
        ),
    }
}

fn validate_public_url(value: &str) -> anyhow::Result<()> {
    let uri = value
        .parse::<axum::http::Uri>()
        .map_err(|_| anyhow::anyhow!("RSCTF_PUBLIC_URL must be an absolute HTTP(S) URL"))?;
    let authority = uri.authority().map(|value| value.as_str());
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || authority.is_none_or(|value| value.contains('@'))
    {
        anyhow::bail!("RSCTF_PUBLIC_URL must be an absolute HTTP(S) URL");
    }
    Ok(())
}

fn validate_jwt_secret(secret: &str) -> anyhow::Result<()> {
    let secret = secret.trim();
    const KNOWN_INSECURE: &[&str] = &["insecure-dev-secret-change-me", "change-me-in-production"];
    if KNOWN_INSECURE.contains(&secret) {
        anyhow::bail!("RSCTF_JWT_SECRET is set to a repository-known insecure default");
    }
    if secret.len() < 32 {
        anyhow::bail!("RSCTF_JWT_SECRET must contain at least 32 bytes of entropy");
    }
    Ok(())
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_jwt_ttl, validate_jwt_secret, validate_public_url, RuntimeRole};

    #[test]
    fn rejects_known_or_short_jwt_secrets() {
        assert!(validate_jwt_secret("change-me-in-production").is_err());
        assert!(validate_jwt_secret("insecure-dev-secret-change-me").is_err());
        assert!(validate_jwt_secret("short").is_err());
    }

    #[test]
    fn accepts_long_random_jwt_secret() {
        assert!(validate_jwt_secret("0123456789abcdef0123456789abcdef").is_ok());
    }

    #[test]
    fn invalid_jwt_ttl_is_not_silently_replaced() {
        assert_eq!(parse_jwt_ttl(None), (604_800, None));
        assert_eq!(parse_jwt_ttl(Some(" 60 ".to_string())), (60, None));
        let (_, error) = parse_jwt_ttl(Some("forever".to_string()));
        assert!(error.is_some());
    }

    #[test]
    fn public_url_requires_an_http_origin() {
        assert!(validate_public_url("https://ctf.example").is_ok());
        assert!(validate_public_url("http://localhost:8080").is_ok());
        assert!(validate_public_url("ctf.example").is_err());
        assert!(validate_public_url("javascript:alert(1)").is_err());
        assert!(validate_public_url("https://user@ctf.example").is_err());
    }

    #[test]
    fn runtime_roles_parse_case_insensitively() {
        for (value, expected) in [
            ("all", RuntimeRole::All),
            (" WEB ", RuntimeRole::Web),
            ("control", RuntimeRole::Control),
            ("Engine", RuntimeRole::Engine),
            ("network", RuntimeRole::Network),
            ("migrate", RuntimeRole::Migrate),
        ] {
            assert_eq!(value.parse::<RuntimeRole>().unwrap(), expected);
        }
        assert!("worker".parse::<RuntimeRole>().is_err());
        assert!("".parse::<RuntimeRole>().is_err());
    }

    #[test]
    fn runtime_role_capabilities_are_narrow_and_scale_to_one() {
        let all = RuntimeRole::All.capabilities();
        assert!(all.api && all.health && all.maintenance);
        assert!(all.round_engine && all.network && all.migrations);

        let web = RuntimeRole::Web.capabilities();
        assert!(web.api && web.health);
        assert!(!web.maintenance && !web.round_engine && !web.network && !web.migrations);

        let control = RuntimeRole::Control.capabilities();
        assert!(control.api && control.health && control.maintenance);
        assert!(control.round_engine && control.network && !control.migrations);

        let engine = RuntimeRole::Engine.capabilities();
        assert!(!engine.api && engine.health && engine.maintenance);
        assert!(engine.round_engine && !engine.network && !engine.migrations);

        let network = RuntimeRole::Network.capabilities();
        assert!(network.api && network.health && network.network && network.round_engine);
        assert!(!network.maintenance && !network.migrations);

        let migrate = RuntimeRole::Migrate.capabilities();
        assert!(!migrate.api && !migrate.health && !migrate.maintenance);
        assert!(!migrate.round_engine && !migrate.network && migrate.migrations);
    }
}
