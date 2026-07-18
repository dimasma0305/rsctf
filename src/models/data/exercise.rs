//! Exercise (practice) mode — per-user standalone challenges, ported from
//! RSCTF `Models/Data/ExerciseChallenge.cs` + `ExerciseInstance.cs`.

pub mod exercise_challenge {
    use crate::utils::enums::{ChallengeCategory, ChallengeType};
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "ExerciseChallenges")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub title: String,
        pub content: String,
        pub category: ChallengeCategory,
        #[sea_orm(column_name = "Type")]
        #[serde(rename = "type")]
        pub challenge_type: ChallengeType,
        pub hints: Option<Json>,
        pub is_enabled: bool,
        /// Difficulty tier (RSCTF `Difficulty` enum), stored numerically.
        pub difficulty: i16,
        pub container_image: Option<String>,
        pub memory_limit: Option<i32>,
        pub cpu_count: Option<i32>,
        pub expose_port: Option<i32>,
        pub file_name: Option<String>,
        pub flag_template: Option<String>,
        pub attachment_id: Option<i32>,
        pub accepted_count: i32,
        pub submission_count: i32,
        pub original_score: i32,
        pub publish_time_utc: DateTime<Utc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod exercise_instance {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    /// A user's attempt at an exercise (per-user, unlike game instances).
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "ExerciseInstances")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub exercise_id: i32,
        pub user_id: Uuid,
        pub is_loaded: bool,
        pub is_solved: bool,
        pub flag_id: Option<i32>,
        pub container_id: Option<Uuid>,
        pub last_container_operation: DateTime<Utc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
