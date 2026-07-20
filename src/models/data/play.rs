//! Gameplay tables: participations, submissions, instances, containers.

pub mod participation {
    use crate::utils::enums::ParticipationStatus;
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Participations")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub status: ParticipationStatus,
        pub token: String,
        pub writeup_id: Option<i32>,
        pub game_id: i32,
        pub team_id: i32,
        pub division_id: Option<i32>,
        pub suspicion_score: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "crate::models::data::team::Entity",
            from = "Column::TeamId",
            to = "crate::models::data::team::Column::Id"
        )]
        Team,
    }

    impl Related<crate::models::data::team::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Team.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod submission {
    use crate::utils::enums::AnswerResult;
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Submissions")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub answer: String,
        pub status: AnswerResult,
        pub submit_time_utc: DateTime<Utc>,
        pub user_id: Option<Uuid>,
        pub team_id: i32,
        pub participation_id: i32,
        pub game_id: i32,
        pub challenge_id: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod game_instance {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "GameInstances")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub challenge_id: i32,
        pub participation_id: i32,
        pub is_loaded: bool,
        pub last_container_operation: DateTime<Utc>,
        pub flag_id: Option<i32>,
        pub container_id: Option<Uuid>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod container {
    use crate::utils::enums::ContainerStatus;
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Containers")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub image: String,
        pub container_id: String,
        pub status: ContainerStatus,
        pub started_at: DateTime<Utc>,
        pub expect_stop_at: DateTime<Utc>,
        pub is_proxy: bool,
        pub ip: String,
        pub port: i32,
        pub public_ip: Option<String>,
        pub public_port: Option<i32>,
        pub game_instance_id: Option<i32>,
        pub exercise_instance_id: Option<i32>,
        /// Set only for a short-lived A&D inspector container. The database
        /// allows at most one live inspector row per team service.
        pub ad_team_service_id: Option<i32>,
    }

    impl Model {
        /// Connection entry: proxy id, or `ip:port`, matching RSCTF `Container.Entry`.
        pub fn entry(&self) -> String {
            if self.is_proxy {
                self.id.to_string()
            } else {
                let ip = self.public_ip.as_deref().unwrap_or(&self.ip);
                let port = self.public_port.unwrap_or(self.port);
                format!("{ip}:{port}")
            }
        }
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod first_solve {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "FirstSolves")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub participation_id: i32,
        #[sea_orm(primary_key, auto_increment = false)]
        pub challenge_id: i32,
        pub submission_id: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod cheat_info {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "CheatInfo")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        pub submit_team_id: i32,
        pub source_team_id: i32,
        pub submission_id: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
