//! Authoritative runtime policy for player-managed container lifecycles.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::utils::error::{AppError, AppResult};

const AUTO_DESTROY_KEY: &str = "ContainerPolicy:AutoDestroyOnLimitReached";
const MAX_EXERCISE_KEY: &str = "ContainerPolicy:MaxExerciseContainerCountPerUser";
const DEFAULT_LIFETIME_KEY: &str = "ContainerPolicy:DefaultLifetime";
const EXTENSION_DURATION_KEY: &str = "ContainerPolicy:ExtensionDuration";
const RENEWAL_WINDOW_KEY: &str = "ContainerPolicy:RenewalWindow";

/// Values are minutes except for the exercise-container count.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ContainerPolicy {
    pub auto_destroy_on_limit_reached: bool,
    pub max_exercise_container_count_per_user: i32,
    pub default_lifetime: i32,
    pub extension_duration: i32,
    pub renewal_window: i32,
}

impl Default for ContainerPolicy {
    fn default() -> Self {
        Self {
            auto_destroy_on_limit_reached: false,
            max_exercise_container_count_per_user: 1,
            default_lifetime: 120,
            extension_duration: 120,
            renewal_window: 10,
        }
    }
}

impl ContainerPolicy {
    pub fn validate(&self) -> AppResult<()> {
        if !(1..=100).contains(&self.max_exercise_container_count_per_user) {
            return Err(AppError::bad_request(
                "Maximum exercise containers per user must be between 1 and 100",
            ));
        }
        if !(1..=7_200).contains(&self.default_lifetime) {
            return Err(AppError::bad_request(
                "Default container lifetime must be between 1 and 7200 minutes",
            ));
        }
        if !(1..=7_200).contains(&self.extension_duration) {
            return Err(AppError::bad_request(
                "Container renewal duration must be between 1 and 7200 minutes",
            ));
        }
        if !(1..=360).contains(&self.renewal_window) {
            return Err(AppError::bad_request(
                "Container renewal window must be between 1 and 360 minutes",
            ));
        }
        Ok(())
    }

    fn from_map(values: &BTreeMap<String, Option<String>>) -> Self {
        let defaults = Self::default();
        let value = |key: &str| values.get(key).and_then(|value| value.as_deref());
        let bool_value = |key: &str, default: bool| {
            value(key)
                .and_then(|value| value.parse::<bool>().ok())
                .unwrap_or(default)
        };
        let int_value = |key: &str, default: i32| {
            value(key)
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or(default)
        };
        Self {
            auto_destroy_on_limit_reached: bool_value(
                AUTO_DESTROY_KEY,
                defaults.auto_destroy_on_limit_reached,
            ),
            max_exercise_container_count_per_user: int_value(
                MAX_EXERCISE_KEY,
                defaults.max_exercise_container_count_per_user,
            ),
            default_lifetime: int_value(DEFAULT_LIFETIME_KEY, defaults.default_lifetime),
            extension_duration: int_value(EXTENSION_DURATION_KEY, defaults.extension_duration),
            renewal_window: int_value(RENEWAL_WINDOW_KEY, defaults.renewal_window),
        }
    }

    pub async fn load(pool: &sqlx::PgPool) -> AppResult<Self> {
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            r#"SELECT config_key, value
                 FROM "Configs"
                WHERE config_key LIKE 'ContainerPolicy:%'"#,
        )
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let values = rows.into_iter().collect::<BTreeMap<_, _>>();
        let policy = Self::from_map(&values);
        policy.validate().map_err(|error| {
            AppError::internal(format!("invalid stored container policy: {error}"))
        })?;
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_values_override_defaults() {
        let values = BTreeMap::from([
            (AUTO_DESTROY_KEY.to_string(), Some("true".to_string())),
            (MAX_EXERCISE_KEY.to_string(), Some("4".to_string())),
            (DEFAULT_LIFETIME_KEY.to_string(), Some("45".to_string())),
            (EXTENSION_DURATION_KEY.to_string(), Some("30".to_string())),
            (RENEWAL_WINDOW_KEY.to_string(), Some("5".to_string())),
        ]);
        let policy = ContainerPolicy::from_map(&values);
        assert!(policy.auto_destroy_on_limit_reached);
        assert_eq!(policy.max_exercise_container_count_per_user, 4);
        assert_eq!(policy.default_lifetime, 45);
        assert_eq!(policy.extension_duration, 30);
        assert_eq!(policy.renewal_window, 5);
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn unsafe_counts_and_durations_are_rejected() {
        let mut policy = ContainerPolicy::default();
        policy.max_exercise_container_count_per_user = 0;
        assert!(policy.validate().is_err());
        policy = ContainerPolicy::default();
        policy.default_lifetime = 0;
        assert!(policy.validate().is_err());
    }
}
