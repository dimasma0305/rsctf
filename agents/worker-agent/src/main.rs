mod backoff;
mod client;
mod config;
mod enroll;
mod installation;
mod readiness;
mod runtime;
mod security;
mod tls;

use clap::Parser;
use config::{Cli, Command};

fn install_tls_crypto_provider() -> Result<(), &'static str> {
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        return Ok(());
    }
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| "failed to install the process-wide Ring TLS provider")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    if let Err(error) = install_tls_crypto_provider() {
        tracing::error!(error, "worker agent TLS initialization failed");
        std::process::exit(1);
    }

    let cli = Cli::parse();
    let result: Result<(), Box<dyn std::error::Error>> = match cli.command {
        Command::Run(arguments) => client::run(arguments)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>),
        Command::Enroll(arguments) => enroll::run(arguments)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>),
        Command::Doctor(arguments) => runtime::doctor(arguments)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>),
        Command::InstallationStatus(arguments) => installation::print_status(arguments)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>),
    };
    if let Err(error) = result {
        tracing::error!(%error, "worker agent stopped");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn installs_an_explicit_tls_crypto_provider() {
        install_tls_crypto_provider().unwrap();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
