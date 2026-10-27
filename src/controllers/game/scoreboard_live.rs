//! HTTP compatibility exports for shared scoreboard invalidation.
pub(crate) use crate::services::scoreboard_invalidation::{
    invalidate_standard_scoreboard, publish_scoreboard_changed, scoreboard_refresh_is_publicly_safe,
};
