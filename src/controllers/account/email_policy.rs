//! Shared account email-domain policy lookups and validation.

use super::*;

pub(crate) fn verify_email_domain(email: &str, domain_list: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }
    if domain_list.trim().is_empty() {
        return true;
    }
    domain_list
        .split(',')
        .map(str::trim)
        .filter(|domain| !domain.is_empty())
        .any(|allowed| allowed.eq_ignore_ascii_case(domain))
}

pub(crate) async fn load_email_domain_list(st: &SharedState) -> AppResult<String> {
    Ok(
        config::Entity::find_by_id("AccountPolicy:EmailDomainList".to_string())
            .one(&st.db)
            .await?
            .and_then(|row| row.value)
            .unwrap_or_default(),
    )
}
