//! RSCTF retains PostgreSQL sessions for advisory ownership. A transaction
//! pooler cannot honor that contract, even when ordinary queries succeed.

pub(super) fn validate_mode(value: &str) -> anyhow::Result<()> {
    match value.trim().to_ascii_lowercase().as_str() {
        "direct" | "session" => Ok(()),
        "transaction" | "statement" => anyhow::bail!(
            "RSCTF_DB_POOL_MODE={value} is unsupported: RSCTF session advisory locks require direct PostgreSQL connections or PgBouncer pool_mode=session"
        ),
        _ => anyhow::bail!("RSCTF_DB_POOL_MODE must be direct or session"),
    }
}

pub(super) fn validate() -> anyhow::Result<()> {
    validate_mode(&std::env::var("RSCTF_DB_POOL_MODE").unwrap_or_else(|_| "direct".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lock_owners_reject_transaction_and_statement_pooling() {
        for value in ["direct", "session", " SESSION "] {
            validate_mode(value).unwrap();
        }
        for value in ["transaction", "statement", "", "automatic"] {
            assert!(validate_mode(value).is_err(), "accepted {value}");
        }
    }
}
