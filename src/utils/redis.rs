//! Redis connection helpers shared by the cache, event bus, scheduler, and
//! distributed rate limiter.

use ::redis::aio::{ConnectionManager, ConnectionManagerConfig};

/// Build a connection manager without Redis 1.x's implicit 1-second connect
/// and 500-millisecond response deadlines. Callers either apply their own
/// operation-specific timeout or intentionally retain the previous unbounded
/// behavior.
pub(crate) async fn connection_manager(
    client: &::redis::Client,
) -> ::redis::RedisResult<ConnectionManager> {
    client
        .get_connection_manager_with_config(connection_manager_config())
        .await
}

fn connection_manager_config() -> ConnectionManagerConfig {
    ConnectionManagerConfig::new()
        .set_connection_timeout(None)
        .set_response_timeout(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_manager_has_no_implicit_deadlines() {
        let config = connection_manager_config();
        assert_eq!(config.connection_timeout(), None);
        assert_eq!(config.response_timeout(), None);
    }
}
