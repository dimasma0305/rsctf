//! `Models/Data/ContainerAccessEvent.cs` ported to a sea-orm entity.
//!
//! One row per successful WebSocket proxy open of a challenge container
//! (`GET /api/proxy/{id}`), written best-effort by [`crate::controllers::proxy`].
//! It is the ground-truth access record the container-access cheat detectors
//! (`services::suspicion::container_access`) correlate against a solve: who
//! connected, from which IP, and when. RSCTF equivalent:
//! `RSCTF.Services.ContainerAccessLogger` writes this on every proxy open.
use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ContainerAccessEvents")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub game_id: i32,
    pub challenge_id: i32,
    /// Participation that owns the container (the team whose `GameInstance` this
    /// container belongs to).
    pub container_owner_participation_id: i32,
    pub container_id: Uuid,
    /// Authenticated user who opened the proxy WebSocket. `None` when the caller
    /// was anonymous.
    pub accessing_user_id: Option<Uuid>,
    #[sea_orm(column_type = "Text", nullable)]
    pub accessing_user_name: Option<String>,
    /// The accessing user's own participation in this game, when resolvable.
    /// Compared against `container_owner_participation_id` to detect cross-team
    /// proxy access.
    pub accessing_participation_id: Option<i32>,
    #[sea_orm(column_type = "Text")]
    pub remote_ip: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub user_agent: Option<String>,
    /// RSCTF `ConnectedAtUtc` — the proxy-open (container access) time.
    pub connected_at_utc: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
