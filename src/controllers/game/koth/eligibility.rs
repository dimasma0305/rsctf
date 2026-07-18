use std::collections::BTreeSet;
use std::time::Duration;

use crate::app_state::SharedState;
use crate::services::cache::Cache;
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType};
use crate::utils::error::{AppError, AppResult};

const LIVE_HILL_CACHE_TTL: Duration = Duration::from_secs(1);
const LIVE_HILLS_SQL: &str = r#"SELECT id
      FROM "GameChallenges"
     WHERE game_id = $1
       AND is_enabled = TRUE
       AND review_status = $2
       AND "Type" = $3
     ORDER BY id"#;

static LIVE_HILL_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<BTreeSet<i32>>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

fn live_hill_cache_key(game_id: i32) -> String {
    format!("kothlivehills:{game_id}")
}

fn decode_live_hills(bytes: &[u8]) -> Option<BTreeSet<i32>> {
    serde_json::from_slice(bytes).ok()
}

async fn load_live_hills(st: &SharedState, game_id: i32) -> AppResult<BTreeSet<i32>> {
    let key = live_hill_cache_key(game_id);
    if let Some(bytes) = st.cache.get(&key).await {
        if let Some(hills) = decode_live_hills(&bytes) {
            return Ok(hills);
        }
    }

    let st = st.clone();
    let key_for_fill = key.clone();
    LIVE_HILL_SF
        .run(&key, move || async move {
            // Followers re-check after joining the flight so a completed fill is
            // reused instead of issuing a second query at the expiry boundary.
            if let Some(bytes) = st.cache.get(&key_for_fill).await {
                if let Some(hills) = decode_live_hills(&bytes) {
                    return Some(hills);
                }
            }

            let ids = match sqlx::query_scalar::<_, i32>(LIVE_HILLS_SQL)
                .bind(game_id)
                .bind(ChallengeReviewStatus::Active as i16)
                .bind(ChallengeType::KingOfTheHill as i16)
                .fetch_all(st.pg())
                .await
            {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!(game = game_id, %error, "KotH live-hill cache fill failed");
                    return None;
                }
            };
            let hills = ids.into_iter().collect::<BTreeSet<_>>();
            let encoded = match serde_json::to_vec(&hills) {
                Ok(encoded) => encoded,
                Err(error) => {
                    tracing::warn!(game = game_id, %error, "KotH live-hill cache serialization failed");
                    return None;
                }
            };
            st.cache
                .set(&key_for_fill, &encoded, Some(LIVE_HILL_CACHE_TTL))
                .await;
            Some(hills)
        })
        .await
        .ok_or_else(|| AppError::internal("KotH live-hill cache fill failed"))
}

/// Require a currently enabled, reviewed KotH challenge owned by this game.
/// Token/state/audit handlers are live-only, so a one-second game-global set is
/// sufficient; historical scoreboard projections never pass through this gate.
pub(super) async fn require_live_hill(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    load_live_hills(st, game_id)
        .await?
        .contains(&challenge_id)
        .then_some(())
        .ok_or_else(|| AppError::not_found("Active KotH hill not found"))
}

/// Evict the writer replica and Redis after challenge edits. Existing remote
/// L1 copies expire within one second; a pre-edit fill that races the eviction
/// can repopulate L2 and extend that rare stale window to about two cache TTLs.
pub(crate) async fn invalidate_live_hill_cache(cache: &dyn Cache, game_id: i32) {
    cache.remove(&live_hill_cache_key(game_id)).await;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use sqlx::{Connection, PgConnection};

    use crate::services::cache::{Cache, InMemoryCache};
    use crate::utils::enums::{ChallengeReviewStatus, ChallengeType};

    use super::{
        decode_live_hills, invalidate_live_hill_cache, live_hill_cache_key, LIVE_HILLS_SQL,
    };

    #[test]
    fn all_hills_in_one_game_share_one_cache_key() {
        assert_eq!(live_hill_cache_key(17), "kothlivehills:17");
    }

    #[test]
    fn cached_live_hills_preserve_exact_membership() {
        let expected = BTreeSet::from([7, 11]);
        let encoded = serde_json::to_vec(&expected).unwrap();
        assert_eq!(decode_live_hills(&encoded), Some(expected));
        assert!(decode_live_hills(b"not-json").is_none());
    }

    #[tokio::test]
    async fn challenge_edit_evicts_live_hill_membership() {
        let cache = InMemoryCache::new();
        let key = live_hill_cache_key(17);
        cache.set(&key, b"[7]", None).await;

        invalidate_live_hill_cache(&cache, 17).await;

        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn query_keeps_only_enabled_reviewed_koth_hills_in_the_requested_game() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"CREATE TEMP TABLE "GameChallenges" (
                 id INTEGER PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 is_enabled BOOLEAN NOT NULL,
                 review_status SMALLINT NOT NULL,
                 "Type" SMALLINT NOT NULL
               )"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges" VALUES
               (1, 10, TRUE, $1, $2),
               (2, 10, FALSE, $1, $2),
               (3, 10, TRUE, $3, $2),
               (4, 10, TRUE, $1, $4),
               (5, 11, TRUE, $1, $2)"#,
        )
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::KingOfTheHill as i16)
        .bind(ChallengeReviewStatus::Rejected as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .execute(&mut connection)
        .await
        .unwrap();

        let ids = sqlx::query_scalar::<_, i32>(LIVE_HILLS_SQL)
            .bind(10)
            .bind(ChallengeReviewStatus::Active as i16)
            .bind(ChallengeType::KingOfTheHill as i16)
            .fetch_all(&mut connection)
            .await
            .unwrap();

        assert_eq!(ids, vec![1]);
    }
}
