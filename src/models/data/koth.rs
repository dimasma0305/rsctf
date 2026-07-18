//! RSCTF King-of-the-Hill persistence entities.

pub mod koth_target {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    /// A KotH target service teams compete to hold.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "KothTargets")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        pub challenge_id: i32,
        pub host: String,
        pub port: i32,
        /// Participation currently holding the target (None = uncaptured).
        pub holder_participation_id: Option<i32>,
        pub held_since: Option<DateTime<Utc>>,
        /// Platform-launched shared hill container (self-hosted KotH); the secret
        /// control token is planted inside it and read back to verify control.
        pub container_id: Option<String>,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod koth_token {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    /// An immutable crown-cycle capability issued for one team and hill.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "KothTokens")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        /// Exact hill target for this crown-cycle capability.
        pub target_id: i32,
        pub participation_id: i32,
        pub token: String,
        pub submitted_at: DateTime<Utc>,
        /// Authoritative crown-cycle round for which this token was issued.
        pub round_number: i32,
        /// Database identity of the issuing round.
        pub ad_round_id: i32,
        /// When this bearer capability stopped being accepted. The issuance row
        /// remains immutable evidence for historical scoring windows.
        pub revoked_at: Option<DateTime<Utc>>,
        /// Crown cycle that owns this capability.
        pub cycle_id: i64,
        /// Challenge whose exact shared hill this capability controls.
        pub challenge_id: i32,
        /// Durable reset attempt within the cycle. A stopped active backend
        /// starts a new immutable capability window without rewriting the
        /// revoked evidence from an earlier attempt.
        pub reset_attempt: i32,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod koth_control_result {
    //! Per-round record of which team controlled a KotH hill and the credit it
    //! earned that round, written by the crown-cycle checker.
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "KothControlResults")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        pub challenge_id: i32,
        pub ad_round_id: i32,
        /// Exact current-window token observed in `/koth/king` this round.
        pub controlling_participation_id: Option<i32>,
        /// Team accountable for hill correctness until another valid token is
        /// observed or the published holder is explicitly cleared.
        pub responsible_participation_id: Option<i32>,
        /// Whether the same marker value bracketed the functional probe. False
        /// is diagnostic: it elects no new controller but does not void the
        /// independent functional verdict.
        pub marker_observed: bool,
        /// AdCheckStatus numeric (Ok/Mumble/Offline/InternalError).
        pub status: i16,
        pub error_message: Option<String>,
        pub checked_at: DateTime<Utc>,
        /// Exact managed backend that the checker authoritatively observed dead.
        /// This terminal receipt gates holder-clearing automatic repair.
        pub dead_container_id: Option<String>,
        /// Reset-attempt token window observed by this immutable result.
        pub token_window_attempt: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
