//! Bounded identity lookup for the game-manager selector.

use super::*;

pub(crate) const MANAGER_AUTOCOMPLETE_MIN_CHARS: usize = 2;
pub(crate) const MANAGER_AUTOCOMPLETE_MAX_CHARS: usize = 64;
pub(crate) const MANAGER_AUTOCOMPLETE_LIMIT: i64 = 10;

/// Each indexed branch is capped before the final de-duplication. A user whose
/// username and email both match therefore cannot make the intermediate set
/// exceed twice the public response limit.
pub(crate) const MANAGER_AUTOCOMPLETE_SQL: &str = r#"
WITH username_matches AS MATERIALIZED (
    SELECT id, LEFT(user_name, 128) AS user_name,
           LEFT(email, 320) AS email,
           LEFT(avatar_hash, 128) AS avatar_hash,
           normalized_user_name AS match_key,
           0::smallint AS source_rank
      FROM "AspNetUsers"
     WHERE normalized_user_name COLLATE "C" >= $1
       AND normalized_user_name COLLATE "C" < $2
     ORDER BY normalized_user_name COLLATE "C", id
     LIMIT $3
),
email_matches AS MATERIALIZED (
    SELECT id, LEFT(user_name, 128) AS user_name,
           LEFT(email, 320) AS email,
           LEFT(avatar_hash, 128) AS avatar_hash,
           normalized_email AS match_key,
           1::smallint AS source_rank
      FROM "AspNetUsers"
     WHERE normalized_email COLLATE "C" >= $1
       AND normalized_email COLLATE "C" < $2
     ORDER BY normalized_email COLLATE "C", id
     LIMIT $3
),
ranked AS (
    SELECT id, user_name, email, avatar_hash, match_key, source_rank,
           ROW_NUMBER() OVER (
               PARTITION BY id
               ORDER BY source_rank, match_key COLLATE "C", id
           ) AS duplicate_rank
      FROM (
          SELECT * FROM username_matches
          UNION ALL
          SELECT * FROM email_matches
      ) matches
)
SELECT id, user_name, email, avatar_hash
  FROM ranked
 WHERE duplicate_rank = 1
 ORDER BY source_rank, match_key COLLATE "C", id
 LIMIT $3
"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerAutocompleteQuery {
    #[serde(default)]
    pub query: String,
}

/// Deliberately excludes profile, role, IP, and activity fields that the
/// manager picker neither renders nor needs to authorize its later mutation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerAutocompleteUserModel {
    pub id: Uuid,
    pub user_name: Option<String>,
    pub email: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ManagerAutocompleteRow {
    id: Uuid,
    user_name: Option<String>,
    email: Option<String>,
    avatar_hash: Option<String>,
}

impl From<ManagerAutocompleteRow> for ManagerAutocompleteUserModel {
    fn from(row: ManagerAutocompleteRow) -> Self {
        Self {
            id: row.id,
            user_name: row.user_name,
            email: row.email,
            avatar: row.avatar_hash.map(|hash| format!("/assets/{hash}/avatar")),
        }
    }
}

/// Convert a prefix into the first UTF-8 string after every string beginning
/// with that prefix. PostgreSQL's `C` collation compares UTF-8 byte sequences,
/// whose scalar ordering matches this increment-and-truncate operation.
fn prefix_upper_bound(prefix: &str) -> Option<String> {
    let mut characters: Vec<char> = prefix.chars().collect();
    for index in (0..characters.len()).rev() {
        let codepoint = characters[index] as u32;
        let next = (codepoint.saturating_add(1)..=char::MAX as u32).find_map(char::from_u32);
        if let Some(next) = next {
            characters.truncate(index);
            characters.push(next);
            return Some(characters.into_iter().collect());
        }
    }
    None
}

fn normalized_manager_query(query: &str) -> AppResult<(String, String)> {
    let normalized = query.trim().to_uppercase();
    let character_count = normalized.chars().count();
    if !(MANAGER_AUTOCOMPLETE_MIN_CHARS..=MANAGER_AUTOCOMPLETE_MAX_CHARS).contains(&character_count)
    {
        return Err(AppError::bad_request(format!(
            "Manager search must contain between {MANAGER_AUTOCOMPLETE_MIN_CHARS} and {MANAGER_AUTOCOMPLETE_MAX_CHARS} characters"
        )));
    }
    if normalized.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "Manager search cannot contain control characters",
        ));
    }
    let upper_bound = prefix_upper_bound(&normalized)
        .ok_or_else(|| AppError::bad_request("Manager search prefix is unsupported"))?;
    Ok((normalized, upper_bound))
}

pub(crate) async fn manager_autocomplete_rows(
    pool: &sqlx::PgPool,
    prefix: &str,
    upper_bound: &str,
) -> AppResult<Vec<ManagerAutocompleteUserModel>> {
    let rows = sqlx::query_as::<_, ManagerAutocompleteRow>(MANAGER_AUTOCOMPLETE_SQL)
        .bind(prefix)
        .bind(upper_bound)
        .bind(MANAGER_AUTOCOMPLETE_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// `GET /api/admin/users/manager-autocomplete` — compact prefix-only identity
/// lookup for the platform-admin manager roster editor.
pub async fn manager_autocomplete(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(query): Query<ManagerAutocompleteQuery>,
) -> AppResult<Json<Vec<ManagerAutocompleteUserModel>>> {
    let (prefix, upper_bound) = normalized_manager_query(&query.query)?;
    Ok(Json(
        manager_autocomplete_rows(st.pg(), &prefix, &upper_bound).await?,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    use super::*;
    use crate::app_state::AppState;
    use crate::middlewares::privilege_authentication::CurrentUser;
    use crate::models::internal::configs::{AppConfig, RuntimeRole};
    use crate::services::cache::InMemoryCache;
    use crate::services::container::NoopContainerManager;
    use crate::services::token::TokenService;
    use crate::storage::LocalBlobStorage;
    use crate::utils::enums::Role;

    fn test_state() -> SharedState {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool);
        let root = std::env::temp_dir().join(format!(
            "rsctf-manager-autocomplete-route-{}",
            Uuid::new_v4().simple()
        ));
        let mut config = AppConfig::default();
        config.runtime_role = RuntimeRole::All;
        AppState::new(
            database,
            Arc::new(config),
            Arc::new(InMemoryCache::new()),
            Arc::new(LocalBlobStorage::new(root)),
            TokenService::new("0123456789abcdef0123456789abcdef", 60),
            Arc::new(NoopContainerManager),
        )
    }

    #[test]
    fn query_normalization_bounds_work_and_treats_wildcards_literally() {
        assert_eq!(
            normalized_manager_query("  aLiCe%_  ").unwrap(),
            ("ALICE%_".to_string(), "ALICE%`".to_string())
        );
        assert!(normalized_manager_query("a").is_err());
        assert!(normalized_manager_query(&"a".repeat(MANAGER_AUTOCOMPLETE_MAX_CHARS + 1)).is_err());
        assert!(normalized_manager_query("ab\ncd").is_err());
    }

    #[test]
    fn prefix_upper_bound_handles_unicode_and_carry() {
        assert_eq!(prefix_upper_bound("ABZ"), Some("AB[".to_string()));
        assert_eq!(prefix_upper_bound("A\u{10ffff}"), Some("B".to_string()));
        assert_eq!(prefix_upper_bound("\u{10ffff}"), None);
    }

    #[test]
    fn query_has_no_count_and_caps_both_index_branches() {
        assert!(!MANAGER_AUTOCOMPLETE_SQL.to_uppercase().contains("COUNT("));
        assert_eq!(MANAGER_AUTOCOMPLETE_SQL.matches("LIMIT $3").count(), 3);
        assert!(MANAGER_AUTOCOMPLETE_SQL.contains("username_matches AS MATERIALIZED"));
        assert!(MANAGER_AUTOCOMPLETE_SQL.contains("email_matches AS MATERIALIZED"));
        assert!(MANAGER_AUTOCOMPLETE_SQL.contains("COLLATE \"C\" >= $1"));
        assert!(MANAGER_AUTOCOMPLETE_SQL.contains("COLLATE \"C\" < $2"));
        assert!(MANAGER_AUTOCOMPLETE_SQL.contains("LEFT(user_name, 128)"));
        assert!(MANAGER_AUTOCOMPLETE_SQL.contains("LEFT(email, 320)"));
        assert!(MANAGER_AUTOCOMPLETE_SQL.contains("LEFT(avatar_hash, 128)"));
    }

    #[test]
    fn route_uses_admin_authentication_and_named_query_admission() {
        let router = include_str!("mod.rs");
        assert!(router.contains("\"/api/admin/users/manager-autocomplete\""));
        assert!(router.contains("limited(Policy::Query, get(manager_autocomplete))"));
        let handler = include_str!("users_manager_autocomplete.rs");
        assert!(handler.contains("_admin: AdminUser"));
    }

    #[tokio::test]
    async fn endpoint_rejects_anonymous_and_ordinary_users_before_querying() {
        let app = Router::new()
            .route(
                "/api/admin/users/manager-autocomplete",
                get(manager_autocomplete),
            )
            .with_state(test_state());

        let anonymous = app
            .clone()
            .oneshot(
                Request::get("/api/admin/users/manager-autocomplete?query=alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

        let ordinary_user = CurrentUser {
            id: Uuid::new_v4(),
            role: Role::User,
            name: "player".to_owned(),
            security_stamp: "stamp".to_owned(),
        };
        let forbidden = app
            .oneshot(
                Request::get("/api/admin/users/manager-autocomplete?query=alice")
                    .extension(ordinary_user)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }
}
