//! Teams and the user<->game participation link table.

pub mod team {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Teams")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
        pub bio: Option<String>,
        pub avatar_hash: Option<String>,
        pub locked: bool,
        /// Durable fail-closed marker while multi-stage deletion tears down
        /// credentials and workloads. Internal only; not part of the API DTO.
        #[serde(default, skip_serializing)]
        pub deletion_pending: bool,
        pub invite_token: String,
        pub captain_id: Uuid,
    }

    impl Model {
        /// `{Name}:{Id}:{InviteToken}` — matches RSCTF `Team.InviteCode`.
        pub fn invite_code(&self) -> String {
            format!("{}:{}:{}", self.name, self.id, self.invite_token)
        }
        pub fn avatar_url(&self) -> Option<String> {
            self.avatar_hash
                .as_ref()
                .map(|h| format!("/assets/{h}/avatar"))
        }
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "crate::models::data::user::Entity",
            from = "Column::CaptainId",
            to = "crate::models::data::user::Column::Id"
        )]
        Captain,
    }

    impl Related<crate::models::data::user::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Captain.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod user_participation {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    /// Join row: one user's membership of one team in one game.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "UserParticipations")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub user_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub game_id: i32,
        pub team_id: i32,
        pub participation_id: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
