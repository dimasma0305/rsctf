//! Attack-Defense entities, ported from RSCTF `Models/Data/Ad*.cs`. The scoring
//! math lives in `services/ad/engine/`; these tables persist the round state.

pub mod ad_round {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AdRounds")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        /// Monotonic round number (tick index).
        pub number: i32,
        pub start_time_utc: DateTime<Utc>,
        pub end_time_utc: DateTime<Utc>,
        /// Whether scoring for this round has been finalized.
        pub finalized: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod ad_team_service {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    /// One team's instance of one A&D service (challenge) in a game.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AdTeamServices")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        pub participation_id: i32,
        pub challenge_id: i32,
        pub host: String,
        pub port: i32,
        /// Latest checker status (`AdCheckStatus` numeric).
        pub status: i16,
        /// Platform-launched container backing this service (self-hosted A&D);
        /// `None` for an externally-registered service.
        pub container_id: Option<String>,
        /// When this service's container was last reset (self-reset cooldown).
        pub last_reset_at: Option<DateTime<Utc>>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod ad_vpn_peer {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    /// One WireGuard peer (a team) on the A&D VPN — a persisted X25519 keypair and
    /// the /32 VPN address it's assigned. The in-process hub (`services::ad_vpn`)
    /// and the team's downloaded `.conf` are both rendered from this same key.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AdVpnPeers")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        pub participation_id: i32,
        /// Peer WireGuard private key (base64) — used to render the team's `.conf`.
        pub private_key: String,
        /// Peer WireGuard public key (base64) — added to the hub interface.
        pub public_key: String,
        /// Assigned VPN address, e.g. `10.13.37.10` (a /32 on the client CIDR).
        pub address: String,
        pub created_utc: DateTime<Utc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod ad_flag {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    /// A rotating flag planted in a team's service for one round.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AdFlags")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub round_id: i32,
        pub team_service_id: i32,
        pub flag: String,
        pub planted_at: DateTime<Utc>,
        /// Whether a custom checker was configured when this flag was minted.
        /// Combine this snapshot with the round result's `flag_verified` value.
        pub checker_qualified: bool,
        /// Precommitted challenge weight frozen when the flag was minted.
        pub service_weight: f64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod ad_attack {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    /// A successful flag capture: `attacker` stole `victim_service`'s flag.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AdAttacks")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub round_id: i32,
        pub attacker_participation_id: i32,
        pub victim_team_service_id: i32,
        pub flag_id: i32,
        pub submitted_at: DateTime<Utc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod ad_check_result {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    /// SLA checker outcome for one team-service in one round.
    // No `Eq`: `sla_credit` is `f64`, which is only `PartialEq`.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AdCheckResults")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub round_id: i32,
        pub team_service_id: i32,
        /// `AdCheckStatus` numeric (Ok/Mumble/Offline/InternalError).
        pub status: i16,
        pub message: Option<String>,
        pub checked_at: DateTime<Utc>,
        /// Operational per-tick credit plus an explicit completion marker. NULL
        /// identifies an unchecked round-preparation placeholder. Official epoch
        /// scoring recomputes normalized SLA from ordered statuses rather than
        /// treating this value as points.
        pub sla_credit: Option<f64>,
        /// True only when the executed custom checker received this round's exact flag.
        pub flag_verified: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
