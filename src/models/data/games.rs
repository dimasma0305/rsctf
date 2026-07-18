//! Games, divisions, and per-game notices/events.

pub mod game {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Games")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub title: String,
        #[serde(skip_serializing)]
        pub public_key: String,
        #[serde(skip_serializing)]
        pub private_key: String,
        pub hidden: bool,
        pub practice_mode: bool,
        pub poster_hash: Option<String>,
        pub summary: String,
        pub content: String,
        pub accept_without_review: bool,
        pub allow_user_submissions: bool,
        pub writeup_required: bool,
        pub invite_code: Option<String>,
        pub team_member_count_limit: i32,
        pub discord_webhook: Option<String>,
        pub container_count_limit: i32,
        pub start_time_utc: DateTime<Utc>,
        pub end_time_utc: DateTime<Utc>,
        pub writeup_deadline: DateTime<Utc>,
        pub freeze_time_utc: Option<DateTime<Utc>>,
        pub writeup_note: String,
        pub blood_bonus_value: i64,
        pub repo_binding_id: Option<i32>,
        pub event_manifest_path: Option<String>,

        // --- Attack-Defense / KotH engine tunables ---
        pub ad_warmup_seconds: Option<i32>,
        pub ad_tick_seconds: Option<i32>,
        pub ad_flag_lifetime_ticks: Option<i32>,
        pub ad_reset_cooldown_minutes: Option<i32>,
        pub ad_getflag_window_fraction: Option<f64>,
        pub ad_min_grace_period_seconds: Option<i32>,
        pub koth_refresh_ticks: Option<i32>,
        pub koth_hold_points_per_tick: Option<f64>,
        pub ad_allow_snapshot_download: bool,
        pub ad_snapshot_retention_days: Option<i32>,
        pub ad_scoring_paused: bool,
        pub ad_scoring_paused_at: Option<DateTime<Utc>>,
        /// Number of A&D ticks grouped into one scoring epoch (`1..=64`).
        pub ad_epoch_ticks: i32,
        /// First round included in official epoch scoring. Set once when ready.
        pub ad_scoring_start_round: Option<i32>,
        /// First KotH scoring round, aligned to a token-window boundary.
        pub koth_scoring_start_round: Option<i32>,
        /// Number of KotH scoring ticks in one official epoch.
        pub koth_epoch_ticks: i32,
        /// Number of scorable ticks between pristine hill resets.
        pub koth_cycle_ticks: i32,
        /// Opening ticks denied to the previous crown-cycle champion.
        pub koth_champion_cooldown_ticks: i32,
        /// Consecutive healthy observations required to confirm a claim.
        pub koth_claim_confirmation_ticks: i32,
    }

    impl Model {
        pub fn is_active(&self, now: DateTime<Utc>) -> bool {
            self.start_time_utc <= now && now <= self.end_time_utc
        }
        pub fn poster_url(&self) -> Option<String> {
            self.poster_hash
                .as_ref()
                .map(|h| format!("/assets/{h}/poster"))
        }
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod division {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Divisions")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        pub name: String,
        pub invite_code: Option<String>,
        /// `GamePermission` bit-flags.
        pub default_permissions: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod division_challenge_config {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "DivisionChallengeConfigs")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub division_id: i32,
        #[sea_orm(primary_key, auto_increment = false)]
        pub challenge_id: i32,
        pub permissions: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod game_notice {
    use crate::utils::enums::NoticeType;
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "GameNotices")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        #[sea_orm(column_name = "Type")]
        #[serde(rename = "type")]
        pub notice_type: NoticeType,
        /// Format-string arguments (`List<string>`), stored as JSON.
        pub values: Json,
        pub publish_time_utc: DateTime<Utc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod game_event {
    use crate::utils::enums::EventType;
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "GameEvents")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        #[sea_orm(column_name = "Type")]
        #[serde(rename = "type")]
        pub event_type: EventType,
        pub values: Json,
        pub publish_time_utc: DateTime<Utc>,
        pub user_id: Option<Uuid>,
        pub team_id: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
