//! `HoneypotHits` — one row per hit on a honeypot bait route
//! ([`crate::controllers::honeypot`]). The `HoneypotChain` detector
//! ([`crate::services::suspicion::run_honeypot_chain_checks`]) aggregates distinct
//! baits per participation over a sliding window. RSCTF equivalent: the honeypot
//! hit persistence behind `HoneypotService.RecordHit`.
use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "HoneypotHits")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// The game the attributed participation belongs to (null when unattributed).
    pub game_id: Option<i32>,
    /// The participation the hit was attributed to (null when unattributed — a
    /// cross-site-forgeable GET with no same-origin authenticated caller).
    pub participation_id: Option<i32>,
    pub user_id: Option<Uuid>,
    /// The bait path that was hit (e.g. `/.env`, `/wp-login.php`).
    pub bait: String,
    pub remote_ip: String,
    pub user_agent: Option<String>,
    pub hit_at_utc: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
