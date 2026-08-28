use super::*;

/// Complete editable event model. `start`, `end`, `freeze`, `poster`, and
/// `bloodBonus` retain the established public JSON names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameInfoModel {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub accept_without_review: bool,
    #[serde(default)]
    pub allow_user_submissions: bool,
    #[serde(default)]
    pub writeup_required: bool,
    #[serde(default)]
    pub invite_code: Option<String>,
    #[serde(default)]
    pub team_member_count_limit: i32,
    #[serde(default = "default_container_limit")]
    pub container_count_limit: i32,
    #[serde(default)]
    pub discord_webhook: Option<String>,
    #[serde(default, rename = "poster")]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub public_key: String,
    #[serde(default = "default_true")]
    pub practice_mode: bool,
    #[serde(
        default = "epoch",
        rename = "start",
        with = "crate::utils::datetime::millis"
    )]
    pub start_time_utc: DateTime<Utc>,
    #[serde(
        default = "epoch",
        rename = "end",
        with = "crate::utils::datetime::millis"
    )]
    pub end_time_utc: DateTime<Utc>,
    #[serde(
        default,
        rename = "freeze",
        with = "crate::utils::datetime::millis_opt"
    )]
    pub freeze_time_utc: Option<DateTime<Utc>>,
    #[serde(default = "epoch", with = "crate::utils::datetime::millis")]
    pub writeup_deadline: DateTime<Utc>,
    #[serde(default)]
    pub writeup_note: String,
    #[serde(default = "default_blood_bonus", rename = "bloodBonus")]
    pub blood_bonus_value: i64,
    #[serde(default)]
    pub ad_warmup_seconds: Option<i32>,
    #[serde(default)]
    pub ad_snapshot_retention_days: Option<i32>,
    #[serde(default)]
    pub ad_tick_seconds: Option<i32>,
    #[serde(default)]
    pub ad_flag_lifetime_ticks: Option<i32>,
    #[serde(default)]
    pub ad_reset_cooldown_minutes: Option<i32>,
    #[serde(default)]
    pub ad_allow_snapshot_download: Option<bool>,
    #[serde(default)]
    pub ad_getflag_window_fraction: Option<f64>,
    #[serde(default)]
    pub ad_min_grace_period_seconds: Option<i32>,
    #[serde(default)]
    pub ad_epoch_ticks: Option<i32>,
    #[serde(default)]
    pub koth_epoch_ticks: Option<i32>,
    #[serde(default)]
    pub koth_cycle_ticks: Option<i32>,
    #[serde(default)]
    pub koth_champion_cooldown_ticks: Option<i32>,
    #[serde(default)]
    pub koth_claim_confirmation_ticks: Option<i32>,
    #[serde(default, skip_deserializing)]
    pub ad_scoring_start_round: Option<i32>,
    #[serde(default, skip_deserializing)]
    pub koth_scoring_start_round: Option<i32>,
    #[serde(default)]
    pub vpn_access_required: bool,
    #[serde(default)]
    pub vpn_behavior_telemetry_enabled: bool,
    #[serde(default)]
    pub vpn_flag_scan_enabled: bool,
    #[serde(default)]
    pub vpn_provider_dns_telemetry_enabled: bool,
    #[serde(default)]
    pub vpn_source_asn_telemetry_enabled: bool,
    #[serde(default)]
    pub vpn_device_sharing_telemetry_enabled: bool,
    #[serde(default)]
    pub configuration_revision: i64,
    #[serde(default)]
    pub challenge_configuration_revision: i64,
    #[serde(default, skip_serializing)]
    pub operation_id: Option<Uuid>,
    #[serde(skip_deserializing, with = "crate::utils::datetime::millis_opt")]
    pub server_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing)]
    pub vpn_policy_change_reason: Option<String>,
}

impl GameInfoModel {
    pub(super) fn from_game(g: &game::Model) -> Self {
        Self {
            id: g.id,
            title: g.title.clone(),
            hidden: g.hidden,
            summary: g.summary.clone(),
            content: g.content.clone(),
            accept_without_review: g.accept_without_review,
            allow_user_submissions: g.allow_user_submissions,
            writeup_required: g.writeup_required,
            invite_code: g.invite_code.clone(),
            team_member_count_limit: g.team_member_count_limit,
            container_count_limit: g.container_count_limit,
            discord_webhook: g.discord_webhook.clone(),
            poster_url: g.poster_url(),
            public_key: g.public_key.clone(),
            practice_mode: g.practice_mode,
            start_time_utc: g.start_time_utc,
            end_time_utc: g.end_time_utc,
            freeze_time_utc: g.freeze_time_utc,
            writeup_deadline: g.writeup_deadline,
            writeup_note: g.writeup_note.clone(),
            blood_bonus_value: g.blood_bonus_value,
            ad_warmup_seconds: g.ad_warmup_seconds,
            ad_snapshot_retention_days: g.ad_snapshot_retention_days,
            ad_tick_seconds: g.ad_tick_seconds,
            ad_flag_lifetime_ticks: g.ad_flag_lifetime_ticks,
            ad_reset_cooldown_minutes: g.ad_reset_cooldown_minutes,
            ad_allow_snapshot_download: Some(g.ad_allow_snapshot_download),
            ad_getflag_window_fraction: g.ad_getflag_window_fraction,
            ad_min_grace_period_seconds: g.ad_min_grace_period_seconds,
            ad_epoch_ticks: Some(g.ad_epoch_ticks),
            koth_epoch_ticks: Some(g.koth_epoch_ticks),
            koth_cycle_ticks: Some(g.koth_cycle_ticks),
            koth_champion_cooldown_ticks: Some(g.koth_champion_cooldown_ticks),
            koth_claim_confirmation_ticks: Some(g.koth_claim_confirmation_ticks),
            ad_scoring_start_round: g.ad_scoring_start_round,
            koth_scoring_start_round: g.koth_scoring_start_round,
            vpn_access_required: g.vpn_access_required,
            vpn_behavior_telemetry_enabled: g.vpn_behavior_telemetry_enabled,
            vpn_flag_scan_enabled: g.vpn_flag_scan_enabled,
            vpn_provider_dns_telemetry_enabled: g.vpn_provider_dns_telemetry_enabled,
            vpn_source_asn_telemetry_enabled: g.vpn_source_asn_telemetry_enabled,
            vpn_device_sharing_telemetry_enabled: g.vpn_device_sharing_telemetry_enabled,
            configuration_revision: g.configuration_revision,
            challenge_configuration_revision: g.challenge_configuration_revision,
            operation_id: None,
            server_time: Some(Utc::now()),
            vpn_policy_change_reason: None,
        }
    }

    pub(super) fn configuration(&self) -> crate::services::game_config::GameConfiguration {
        crate::services::game_config::GameConfiguration {
            start_time_utc: self.start_time_utc,
            end_time_utc: self.end_time_utc,
            freeze_time_utc: self.freeze_time_utc,
            team_member_count_limit: self.team_member_count_limit,
            container_count_limit: self.container_count_limit,
            ad_warmup_seconds: self.ad_warmup_seconds,
            ad_snapshot_retention_days: self.ad_snapshot_retention_days,
            ad_tick_seconds: self.ad_tick_seconds,
            ad_flag_lifetime_ticks: self.ad_flag_lifetime_ticks,
            ad_reset_cooldown_minutes: self.ad_reset_cooldown_minutes,
            ad_getflag_window_fraction: self.ad_getflag_window_fraction,
            ad_min_grace_period_seconds: self.ad_min_grace_period_seconds,
            ad_epoch_ticks: self.ad_epoch_ticks.unwrap_or(8),
            koth_epoch_ticks: self.koth_epoch_ticks.unwrap_or(12),
            koth_cycle_ticks: self.koth_cycle_ticks.unwrap_or(3),
            koth_champion_cooldown_ticks: self.koth_champion_cooldown_ticks.unwrap_or(1),
            koth_claim_confirmation_ticks: self.koth_claim_confirmation_ticks.unwrap_or(2),
        }
    }

    pub(super) fn validate(&self) -> AppResult<Option<String>> {
        self.configuration().validate()?;
        crate::services::discord_webhook::normalize_discord_webhook(self.discord_webhook.as_deref())
    }

    pub(super) fn validate_event_security(&self, st: &SharedState) -> AppResult<()> {
        let telemetry = self.vpn_behavior_telemetry_enabled
            || self.vpn_flag_scan_enabled
            || self.vpn_provider_dns_telemetry_enabled
            || self.vpn_source_asn_telemetry_enabled
            || self.vpn_device_sharing_telemetry_enabled;
        if telemetry && !self.vpn_access_required {
            return Err(AppError::bad_request(
                "Event VPN telemetry requires the per-event VPN access policy",
            ));
        }
        if self.vpn_access_required {
            if !crate::services::ad_vpn::enabled() {
                return Err(AppError::bad_request(
                    "Event VPN access requires RSCTF_AD_VPN_ENABLED=true",
                ));
            }
            crate::services::event_security::validate_credential_key(
                &st.config.event_vpn_credential_key,
            )?;
            crate::services::event_security::proof_url(self.id.max(1))?;
        }
        if telemetry
            && (st.config.event_sensor_token.len() < 32
                || st
                    .config
                    .event_sensor_token
                    .chars()
                    .any(char::is_whitespace))
        {
            return Err(AppError::bad_request(
                "Event VPN telemetry requires RSCTF_EVENT_SENSOR_TOKEN",
            ));
        }
        Ok(())
    }
}
