//! Shared validation for event windows and A&D/KotH configuration.
//!
//! Every ingestion path (editor, repository discovery, and archive import)
//! must apply this exact policy before it writes a `Games` row.

use chrono::{DateTime, Utc};

use crate::services::ad_engine::koth_cycle::CrownShapeError;
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, Clone)]
pub(crate) struct GameConfiguration {
    pub start_time_utc: DateTime<Utc>,
    pub end_time_utc: DateTime<Utc>,
    pub freeze_time_utc: Option<DateTime<Utc>>,
    pub team_member_count_limit: i32,
    pub container_count_limit: i32,
    pub ad_warmup_seconds: Option<i32>,
    pub ad_snapshot_retention_days: Option<i32>,
    pub ad_tick_seconds: Option<i32>,
    pub ad_flag_lifetime_ticks: Option<i32>,
    pub ad_reset_cooldown_minutes: Option<i32>,
    pub ad_getflag_window_fraction: Option<f64>,
    pub ad_min_grace_period_seconds: Option<i32>,
    pub ad_epoch_ticks: i32,
    pub koth_epoch_ticks: i32,
    pub koth_cycle_ticks: i32,
    pub koth_champion_cooldown_ticks: i32,
    pub koth_claim_confirmation_ticks: i32,
}

impl GameConfiguration {
    pub(crate) fn validate(&self) -> AppResult<()> {
        if self.start_time_utc >= self.end_time_utc {
            return Err(AppError::bad_request(
                "The event end must be later than its start.",
            ));
        }
        if self
            .freeze_time_utc
            .is_some_and(|freeze| freeze <= self.start_time_utc || freeze >= self.end_time_utc)
        {
            return Err(AppError::bad_request(
                "FreezeTimeUtc must be strictly between StartTimeUtc and EndTimeUtc.",
            ));
        }
        if self.team_member_count_limit < 0 {
            return Err(AppError::bad_request(
                "Team member limit cannot be negative.",
            ));
        }
        if self.container_count_limit < 0 {
            return Err(AppError::bad_request(
                "Container count limit cannot be negative.",
            ));
        }
        if self
            .ad_warmup_seconds
            .is_some_and(|value| !(0..=86_400).contains(&value))
        {
            return Err(AppError::bad_request(
                "A&D warmup must be between 0 and 86400 seconds.",
            ));
        }
        if self
            .ad_snapshot_retention_days
            .is_some_and(|value| !(1..=3_650).contains(&value))
        {
            return Err(AppError::bad_request(
                "A&D snapshot retention must be between 1 and 3650 days.",
            ));
        }
        if self
            .ad_tick_seconds
            .is_some_and(|value| !(30..=600).contains(&value))
        {
            return Err(AppError::bad_request(
                "A&D tick duration must be between 30 and 600 seconds.",
            ));
        }
        if self
            .ad_flag_lifetime_ticks
            .is_some_and(|value| !(1..=50).contains(&value))
        {
            return Err(AppError::bad_request(
                "A&D flag lifetime must be between 1 and 50 ticks.",
            ));
        }
        if self
            .ad_reset_cooldown_minutes
            .is_some_and(|value| !(0..=60).contains(&value))
        {
            return Err(AppError::bad_request(
                "A&D reset cooldown must be between 0 and 60 minutes.",
            ));
        }
        if self
            .ad_getflag_window_fraction
            .is_some_and(|value| !value.is_finite() || !(0.05..=0.9).contains(&value))
        {
            return Err(AppError::bad_request(
                "A&D getflag window fraction must be between 0.05 and 0.9.",
            ));
        }
        if self
            .ad_min_grace_period_seconds
            .is_some_and(|value| !(1..=60).contains(&value))
        {
            return Err(AppError::bad_request(
                "A&D minimum grace period must be between 1 and 60 seconds.",
            ));
        }
        let effective_tick = self.ad_tick_seconds.unwrap_or(60);
        // This is the same runway used by the runtime: bounded publication,
        // one second each for jitter and a real probe, two seconds for durable
        // checker persistence, and the scheduler's outer one-second margin.
        let reserved = i32::try_from(
            crate::services::ad_engine::FLAG_DELIVERY_PUBLICATION_RESERVE_SECONDS
                + crate::services::ad_engine::CHECKER_MINIMUM_RUNWAY_SECONDS
                + crate::services::ad_engine::CHECKER_SCHEDULER_OUTER_MARGIN_SECONDS,
        )
        .unwrap_or(i32::MAX);
        let maximum_grace = effective_tick.saturating_sub(reserved).max(1);
        if self
            .ad_min_grace_period_seconds
            .is_some_and(|grace| grace > maximum_grace)
        {
            return Err(AppError::bad_request(
                "A&D minimum grace period must leave bounded flag-publication, checker execution, and persistence runway.",
            ));
        }
        if !(1..=64).contains(&self.ad_epoch_ticks) {
            return Err(AppError::bad_request(
                "A&D epoch ticks must be between 1 and 64.",
            ));
        }

        let crown_error = crate::services::ad_engine::koth_cycle::validate_crown_shape(
            self.koth_epoch_ticks,
            self.koth_cycle_ticks,
            self.koth_champion_cooldown_ticks,
            self.koth_claim_confirmation_ticks,
        )
        .err();
        let message = match crown_error {
            None => return Ok(()),
            Some(CrownShapeError::Epoch) => "KotH epoch ticks must be between 2 and 64.",
            Some(CrownShapeError::Cycle) => {
                "KotH cycle ticks must divide the KotH epoch into at least two cycles."
            }
            Some(CrownShapeError::ChampionCooldown) => {
                "KotH champion cooldown ticks must be between 0 and one less than the cycle length."
            }
            Some(CrownShapeError::ClaimConfirmation) => {
                "KotH claim confirmation ticks must be between 1 and the cycle length."
            }
        };
        Err(AppError::bad_request(message))
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::GameConfiguration;

    fn valid() -> GameConfiguration {
        let start = Utc::now();
        GameConfiguration {
            start_time_utc: start,
            end_time_utc: start + Duration::hours(1),
            freeze_time_utc: None,
            team_member_count_limit: 0,
            container_count_limit: 3,
            ad_warmup_seconds: Some(0),
            ad_snapshot_retention_days: Some(30),
            ad_tick_seconds: Some(60),
            ad_flag_lifetime_ticks: Some(5),
            ad_reset_cooldown_minutes: Some(5),
            ad_getflag_window_fraction: Some(0.5),
            ad_min_grace_period_seconds: Some(3),
            ad_epoch_ticks: 8,
            koth_epoch_ticks: 12,
            koth_cycle_ticks: 3,
            koth_champion_cooldown_ticks: 1,
            koth_claim_confirmation_ticks: 2,
        }
    }

    #[test]
    fn accepts_the_documented_configuration_boundaries() {
        let mut config = valid();
        assert!(config.validate().is_ok());

        config.ad_tick_seconds = Some(30);
        config.ad_min_grace_period_seconds = Some(18);
        assert!(config.validate().is_ok());
        config.ad_warmup_seconds = Some(86_400);
        config.ad_snapshot_retention_days = Some(3_650);
        config.ad_tick_seconds = Some(600);
        config.ad_flag_lifetime_ticks = Some(50);
        config.ad_reset_cooldown_minutes = Some(60);
        config.ad_getflag_window_fraction = Some(0.9);
        config.ad_min_grace_period_seconds = Some(60);
        config.ad_epoch_ticks = 64;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_invalid_windows_limits_and_engine_timing() {
        let mut invalid = valid();
        invalid.end_time_utc = invalid.start_time_utc;
        assert!(invalid.validate().is_err());

        let mut invalid = valid();
        invalid.freeze_time_utc = Some(invalid.end_time_utc);
        assert!(invalid.validate().is_err());

        for mutate in [
            |config: &mut GameConfiguration| config.team_member_count_limit = -1,
            |config: &mut GameConfiguration| config.container_count_limit = -1,
            |config: &mut GameConfiguration| config.ad_warmup_seconds = Some(-1),
            |config: &mut GameConfiguration| config.ad_snapshot_retention_days = Some(0),
            |config: &mut GameConfiguration| config.ad_tick_seconds = Some(29),
            |config: &mut GameConfiguration| config.ad_flag_lifetime_ticks = Some(0),
            |config: &mut GameConfiguration| config.ad_reset_cooldown_minutes = Some(-1),
            |config: &mut GameConfiguration| config.ad_getflag_window_fraction = Some(f64::NAN),
            |config: &mut GameConfiguration| config.ad_min_grace_period_seconds = Some(0),
            |config: &mut GameConfiguration| config.ad_min_grace_period_seconds = Some(60),
            |config: &mut GameConfiguration| config.ad_epoch_ticks = 0,
            |config: &mut GameConfiguration| config.koth_cycle_ticks = 5,
        ] {
            let mut config = valid();
            mutate(&mut config);
            assert!(config.validate().is_err());
        }

        let mut invalid = valid();
        invalid.ad_tick_seconds = Some(30);
        invalid.ad_min_grace_period_seconds = Some(19);
        assert!(invalid.validate().is_err());
    }
}
