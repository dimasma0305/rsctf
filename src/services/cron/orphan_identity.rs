//! Constant-time ownership checks for runtime orphan maintenance.

use std::collections::HashSet;

use crate::utils::error::{AppError, AppResult};

pub(super) const DOCKER_SHORT_ID_LEN: usize = 12;

const KNOWN_RUNTIME_IDS_SQL: &str = r#"
SELECT runtime_id
  FROM (
        SELECT container_id AS runtime_id
          FROM "Containers"
         WHERE NULLIF(BTRIM(container_id), '') IS NOT NULL
        UNION ALL
        SELECT container_id
          FROM "AdTeamServices"
         WHERE NULLIF(BTRIM(container_id), '') IS NOT NULL
        UNION ALL
        SELECT container_id
          FROM "KothTargets"
         WHERE NULLIF(BTRIM(container_id), '') IS NOT NULL
        UNION ALL
        SELECT runtime.runtime_id
          FROM "KothCrownCycles" cycle
          CROSS JOIN LATERAL unnest(ARRAY[
            cycle.old_container_id, cycle.replacement_container_id
          ]) runtime(runtime_id)
         WHERE cycle.phase <> 'Ended'
           AND NULLIF(BTRIM(runtime.runtime_id), '') IS NOT NULL
       ) owner
"#;

#[derive(Debug, Default)]
pub(super) struct RuntimeOwnership {
    exact: HashSet<String>,
    docker_prefixes: HashSet<String>,
}

impl RuntimeOwnership {
    pub(super) fn from_ids(ids: impl IntoIterator<Item = String>) -> Self {
        let mut ownership = Self::default();
        for id in ids {
            ownership.insert(&id);
        }
        ownership
    }

    fn insert(&mut self, id: &str) {
        let id = id.trim();
        if id.is_empty() {
            return;
        }
        if let Some(prefix) = normalized_docker_prefix(id) {
            self.docker_prefixes.insert(prefix);
        } else {
            self.exact.insert(id.to_string());
        }
    }

    pub(super) fn contains(&self, id: &str) -> bool {
        let id = id.trim();
        normalized_docker_prefix(id).map_or_else(
            || self.exact.contains(id),
            |prefix| self.docker_prefixes.contains(&prefix),
        )
    }
}

fn normalized_docker_prefix(value: &str) -> Option<String> {
    let value = value.trim();
    if !(DOCKER_SHORT_ID_LEN..=64).contains(&value.len())
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(value[..DOCKER_SHORT_ID_LEN].to_ascii_lowercase())
}

pub(super) async fn load_runtime_ownership(
    pool: &sqlx::PgPool,
    candidates: &[String],
) -> AppResult<RuntimeOwnership> {
    let exact = candidates
        .iter()
        .take(512)
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    let docker_prefixes = exact
        .iter()
        .filter_map(|id| normalized_docker_prefix(id))
        .collect::<Vec<_>>();
    if exact.is_empty() {
        return Ok(RuntimeOwnership::default());
    }
    let sql = format!(
        r#"SELECT runtime_id FROM ({KNOWN_RUNTIME_IDS_SQL}) known
            WHERE runtime_id = ANY($1)
               OR (LENGTH(BTRIM(runtime_id)) BETWEEN 12 AND 64
                   AND BTRIM(runtime_id) ~ '^[0-9A-Fa-f]+$'
                   AND LOWER(LEFT(BTRIM(runtime_id), 12)) = ANY($2))"#,
    );
    let ids = sqlx::query_scalar::<_, String>(&sql)
        .bind(&exact)
        .bind(&docker_prefixes)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RuntimeOwnership::from_ids(ids))
}

#[cfg(test)]
mod tests {
    use super::normalized_docker_prefix;

    #[test]
    fn candidate_prefixes_are_only_derived_from_docker_identities() {
        assert_eq!(
            normalized_docker_prefix("ABCDEF1234567890").as_deref(),
            Some("abcdef123456")
        );
        assert!(normalized_docker_prefix("rsctf-runtime-1").is_none());
    }
}
