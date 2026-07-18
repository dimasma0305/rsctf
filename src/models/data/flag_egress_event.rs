//! `FlagEgressEvents` — see [`crate::migrations`] m0019. One windowed row per
//! (participation, challenge, remote endpoint) where a team's flag bytes were
//! seen leaving its proxied container. Admin-feed only (never scored).
use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "FlagEgressEvents")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub game_id: i32,
    pub participation_id: i32,
    pub challenge_id: i32,
    pub container_id: Option<Uuid>,
    pub remote_ip: String,
    pub remote_port: i32,
    pub hit_count: i32,
    pub first_seen_utc: DateTime<Utc>,
    pub last_seen_utc: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
