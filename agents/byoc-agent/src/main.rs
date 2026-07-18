//! Team-side agent for RSCTF self-hosted Attack & Defense services.
//!
//! The agent opens one outbound WebSocket to RSCTF and carries service traffic,
//! rotating flags, and optional interactive container shells over yamux streams.

use std::time::Duration;

mod agent;

pub(crate) const STREAM_SERVICE: u8 = b'S';
pub(crate) const STREAM_FLAG: u8 = b'F';
pub(crate) const STREAM_EXEC: u8 = b'E';

pub(crate) fn yamux_config() -> yamux::Config {
    yamux::Config::default()
}

pub(crate) fn env(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => value,
        _ => default.to_string(),
    }
}

pub(crate) fn must_env(key: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => value,
        _ => {
            tracing::error!(%key, "required environment variable is missing");
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mode = env("RSCTF_BYOC_MODE", "agent").to_lowercase();
    if mode != "agent" {
        tracing::error!(%mode, "RSCTF_BYOC_MODE must be 'agent'");
        std::process::exit(2);
    }

    agent::run_agent().await;
}

pub(crate) const RECONNECT_DELAY: Duration = Duration::from_secs(3);
